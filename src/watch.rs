//! The watch edge: inotify events in, site changes applied, output written, pushes sent.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use tokio::sync::broadcast;

use crate::config::{self, Config, Exclude};
use crate::output::Writer;
use crate::site::{Change, Note, Push, Site, Source};
use crate::vault::{ReadError, VaultDir, classify, scan_under};
use crate::{Error, report};

/// What the watch thread shares with the HTTP thread.
#[derive(Clone)]
pub struct Live {
    pub pushes: broadcast::Sender<Arc<Push>>,
    /// The current version of each published note page, by key.
    pub versions: Arc<Mutex<HashMap<String, String>>>,
}

const MASK: WatchFlags = WatchFlags::CLOSE_WRITE
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR)
    .union(WatchFlags::DONT_FOLLOW)
    .union(WatchFlags::EXCL_UNLINK);

/// How long a path stays held as mid-save with no save event. A save by `link()` sends
/// none, and neither does a note closed before its new folder's watch existed.
const HOLD: Duration = Duration::from_secs(1);

/// Sets the hold in milliseconds, so property tests do not wait out a second per case.
const HOLD_ENV: &str = "TACHYLITE_HOLD_MS";

struct Event {
    wd: i32,
    flags: ReadFlags,
    name: Option<OsString>,
}

pub struct Watcher {
    inotify: OwnedFd,
    /// Watch descriptor to vault-relative folder (`""` is the root).
    dirs: HashMap<i32, String>,
    vault: VaultDir,
    out: PathBuf,
    site: Site,
    live: Live,
    /// Paths that may be mid-save, each with the time it is read as saved anyway.
    held: HashMap<String, Instant>,
    hold: Duration,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The vault, as one batch reads it. A held path may be mid-save, and so may a file at a
/// path whose last event removed it: the new file's CREATE is still queued.
struct Batch<'a> {
    vault: &'a VaultDir,
    held: &'a HashMap<String, Instant>,
    removed: &'a HashSet<String>,
}

impl Batch<'_> {
    fn saved(&self, path: &str) -> bool {
        !self.held.contains_key(path) && !self.removed.contains(path)
    }
}

impl Source for Batch<'_> {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        self.vault.read_note(path, self.saved(path))
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.vault.open_after(path, self.saved(path)).map(|_| ())
    }
}

fn within(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl Watcher {
    /// Watches every published folder, then applies a full sync. Watching first means no
    /// change is lost between the scan and the first event.
    pub fn start(
        vault: VaultDir,
        out: PathBuf,
        live: Live,
        config: Config,
    ) -> Result<Watcher, Error> {
        let inotify = inotify::init(CreateFlags::CLOEXEC).map_err(|e| Error::Watch {
            path: vault.root().to_path_buf(),
            source: e.into(),
        })?;
        let mut watcher = Watcher {
            inotify,
            dirs: HashMap::new(),
            vault,
            out,
            site: Site::new(config.exclude),
            live,
            held: HashMap::new(),
            hold: std::env::var(HOLD_ENV)
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map_or(HOLD, Duration::from_millis),
        };
        watcher.watch_tree("")?;
        // A config saved before the root watch existed sent no event.
        if watcher.reload_config() {
            watcher.rewatch(&[""])?;
        }
        let sync = watcher.sync("")?;
        watcher.apply(vec![sync], HashSet::new())?;
        Ok(watcher)
    }

    /// Runs until a fatal error, which it returns.
    pub fn run(mut self) -> Error {
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 64 * 1024];
        loop {
            let result = self
                .wait()
                .and_then(|queued| match queued {
                    true => self.read_batch(&mut buf),
                    false => Ok(Vec::new()),
                })
                .and_then(|events| self.changes(events))
                .and_then(|(changes, removed)| self.apply(changes, removed));
            if let Err(e) = result {
                return e;
            }
        }
    }

    /// Waits for an event, or until the first hold ends. Returns whether events are queued.
    /// With no path held, it returns at once and the read blocks, so idle costs nothing.
    fn wait(&self) -> Result<bool, Error> {
        let Some(end) = self.held.values().min() else {
            return Ok(true);
        };
        let timeout = Timespec::try_from(end.saturating_duration_since(Instant::now())).ok();
        let mut fds = [PollFd::new(&self.inotify, PollFlags::IN)];
        match poll(&mut fds, timeout.as_ref()) {
            Ok(ready) => Ok(ready > 0),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(e) => Err(Error::Watch {
                path: self.vault.root().to_path_buf(),
                source: e.into(),
            }),
        }
    }

    /// One blocking read, plus every event already in its buffer.
    fn read_batch(&self, buf: &mut [MaybeUninit<u8>]) -> Result<Vec<Event>, Error> {
        let mut reader = inotify::Reader::new(&self.inotify, buf);
        let mut events = Vec::new();
        loop {
            match reader.next() {
                Ok(e) => events.push(Event {
                    wd: e.wd(),
                    flags: e.events(),
                    name: e
                        .file_name()
                        .map(|n| OsString::from_vec(n.to_bytes().to_vec())),
                }),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => {
                    return Err(Error::Watch {
                        path: self.vault.root().to_path_buf(),
                        source: e.into(),
                    });
                }
            }
            if reader.is_buffer_empty() {
                return Ok(events);
            }
        }
    }

    /// The batch's changes, and the paths whose last event removed them.
    fn changes(&mut self, events: Vec<Event>) -> Result<(Vec<Change>, HashSet<String>), Error> {
        let mut paths = BTreeSet::new();
        let mut removed = HashSet::new();
        // Each folder with an event, and whether one was a create.
        let mut folders: BTreeMap<String, bool> = BTreeMap::new();
        let mut reload = false;
        let now = Instant::now();
        for event in events {
            if event.flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                eprintln!("inotify queue overflowed; rescanning the vault");
                // The lost events may include a save of the config.
                self.reload_config();
                return self.resync();
            }
            if event.flags.contains(ReadFlags::IGNORED) {
                self.dirs.remove(&event.wd);
                continue;
            }
            let Some(dir) = self.dirs.get(&event.wd).cloned() else {
                continue;
            };
            if event
                .flags
                .intersects(ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF)
            {
                if dir.is_empty() {
                    return Err(Error::VaultGone(self.vault.root().to_path_buf()));
                }
                continue;
            }
            let Some(name) = event.name else { continue };
            if Exclude::segment(name.as_bytes()) {
                continue;
            }
            let Some(name) = name.to_str() else { continue };
            let path = join(&dir, name);
            if path == config::FILE {
                // Only a save reloads: mid-save, the file can be missing, and a missing
                // config would publish what it excludes.
                reload |= event
                    .flags
                    .intersects(ReadFlags::CLOSE_WRITE | ReadFlags::MOVED_TO);
                continue;
            }
            if event.flags.contains(ReadFlags::ISDIR) {
                if !self.site.exclude().path(&path) {
                    *folders.entry(path).or_default() |= event.flags.contains(ReadFlags::CREATE);
                }
                continue;
            }
            // A path the site would not publish changes nothing.
            if classify(&path, self.site.exclude()).is_none() {
                continue;
            }
            // The last event decides. An attribute change rereads the path as it stands.
            if event
                .flags
                .intersects(ReadFlags::CLOSE_WRITE | ReadFlags::MOVED_TO)
            {
                self.held.remove(&path);
                removed.remove(&path);
            } else if event.flags.contains(ReadFlags::CREATE) {
                self.held.insert(path.clone(), now + self.hold);
                removed.remove(&path);
            } else if event
                .flags
                .intersects(ReadFlags::DELETE | ReadFlags::MOVED_FROM)
            {
                self.held.remove(&path);
                removed.insert(path.clone());
            }
            paths.insert(path);
        }
        if reload && self.reload_config() {
            return self.resync();
        }

        // The folders as they stand now, each once: a parent's sync covers its children.
        let mut tops: Vec<(&str, bool)> = Vec::new();
        for (folder, created) in &folders {
            if !tops.iter().any(|(top, _)| within(folder, top)) {
                tops.push((folder, *created));
            }
        }
        let names: Vec<&str> = tops.iter().map(|(folder, _)| *folder).collect();
        self.rewatch(&names)?;
        let mut changes = Vec::new();
        for (folder, created) in tops {
            let sync = self.sync(folder)?;
            // A file in a new folder may be mid-save, and its CREATE came before the watch.
            if created && let Change::Sync { entries, .. } = &sync {
                for entry in entries {
                    self.held.insert(entry.path.clone(), now + self.hold);
                }
            }
            changes.push(sync);
        }

        let now = Instant::now();
        let ended: Vec<String> = self
            .held
            .iter()
            .filter(|(_, end)| **end <= now)
            .map(|(path, _)| path.clone())
            .collect();
        for path in ended {
            self.held.remove(&path);
            paths.insert(path);
        }
        changes.extend(paths.into_iter().map(Change::Touch));
        Ok((changes, removed))
    }

    /// Rereads `tachylite.toml`. Returns true when a config was read and is now in force. A
    /// missing, mid-save or broken file keeps the previous config.
    fn reload_config(&mut self) -> bool {
        match config::reload(&self.vault) {
            Ok(Some(config)) => {
                self.site.set_exclude(config.exclude);
                true
            }
            Ok(None) => false,
            Err(e) => {
                eprintln!("{e}; keeping the previous config");
                false
            }
        }
    }

    /// Rewatches the tree under the current exclusions and resyncs the root.
    fn resync(&mut self) -> Result<(Vec<Change>, HashSet<String>), Error> {
        self.rewatch(&[""])?;
        Ok((vec![self.sync("")?], HashSet::new()))
    }

    /// Watches `folders` and every published folder below them, as they stand now. A
    /// folder still in place keeps its watch descriptor, so no event queued for it is lost.
    fn rewatch(&mut self, folders: &[&str]) -> Result<(), Error> {
        if folders.is_empty() {
            return Ok(());
        }
        let old: Vec<i32> = self
            .dirs
            .iter()
            .filter(|(_, dir)| folders.iter().any(|f| within(dir, f)))
            .map(|(wd, _)| *wd)
            .collect();
        for wd in &old {
            self.dirs.remove(wd);
        }
        for folder in folders {
            self.watch_tree(folder)?;
        }
        for wd in old {
            if !self.dirs.contains_key(&wd) {
                // The folder may already be gone, which removes its watch too.
                let _ = inotify::remove_watch(&self.inotify, wd);
            }
        }
        Ok(())
    }

    fn sync(&self, under: &str) -> Result<Change, Error> {
        // Scans go by path, so a renamed parent would make them read nothing, or another folder.
        if self.vault.moved() {
            return Err(Error::VaultGone(self.vault.root().to_path_buf()));
        }
        let scan = scan_under(self.vault.root(), under, self.site.exclude())?;
        report::skipped(&scan.skipped);
        Ok(Change::Sync {
            under: under.to_string(),
            entries: scan.entries,
        })
    }

    /// Adds a watch to `rel` and every published folder below it. A folder that vanished
    /// or cannot be read is left to the scan to report.
    fn watch_tree(&mut self, rel: &str) -> Result<(), Error> {
        let path = self.vault.root().join(rel);
        match inotify::add_watch(&self.inotify, &path, MASK) {
            Ok(wd) => {
                self.dirs.insert(wd, rel.to_string());
            }
            Err(e) if rel.is_empty() || e == rustix::io::Errno::NOSPC => {
                return Err(Error::Watch {
                    path,
                    source: e.into(),
                });
            }
            Err(_) => return Ok(()),
        }
        let Ok(items) = fs::read_dir(&path) else {
            return Ok(());
        };
        for item in items.flatten() {
            let name = item.file_name();
            let is_dir = item.file_type().is_ok_and(|t| t.is_dir());
            let Some(name) = name.to_str() else { continue };
            let child = join(rel, name);
            if is_dir && !self.site.exclude().path(&child) {
                self.watch_tree(&child)?;
            }
        }
        Ok(())
    }

    fn apply(&mut self, changes: Vec<Change>, removed: HashSet<String>) -> Result<(), Error> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut batch = Batch {
            vault: &self.vault,
            held: &self.held,
            removed: &removed,
        };
        let live = &self.live;
        let mut writer = Writer::new(&self.out, &self.vault, |push: Push| {
            {
                let mut versions = live.versions.lock().unwrap_or_else(PoisonError::into_inner);
                match &push {
                    Push::Page { key, version, .. } => {
                        versions.insert(key.clone(), version.clone());
                    }
                    Push::Gone { key } => {
                        versions.remove(key);
                    }
                }
            }
            // An error only means no client is listening.
            let _ = live.pushes.send(Arc::new(push));
        });
        let report = self.site.apply_to(changes, &mut batch, &mut writer);
        let failed = writer.finish()?;
        report::skipped(&report.skipped);
        report::skipped(&failed);
        report::broken(&report.broken);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_file_the_site_would_not_publish_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let (vault, out) = crate::build::open(&vault, &tmp.path().join("out")).unwrap();
        crate::output::prepare(&out).unwrap();
        let live = Live {
            pushes: broadcast::channel(4).0,
            versions: Arc::default(),
        };
        let mut watcher = Watcher::start(vault, out, live, Config::default()).unwrap();
        let root = *watcher.dirs.keys().next().unwrap();
        let save = |name: &str| Event {
            wd: root,
            flags: ReadFlags::CLOSE_WRITE,
            name: Some(name.into()),
        };
        let (changes, _) = watcher
            .changes(vec![save("scratch.txt"), save("n.md")])
            .unwrap();
        assert_eq!(changes, [Change::Touch("n.md".into())]);
    }
}
