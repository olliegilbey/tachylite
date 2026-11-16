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

use crate::config::{self, Config, Exclude, Nav};
use crate::output::Writer;
use crate::site::{Change, Note, Push, Site, Source};
use crate::vault::{ReadError, VaultDir, classify, hold, scan_under};
use crate::{Error, report};

/// What the watch thread shares with the HTTP thread.
#[derive(Clone)]
pub struct Live {
    pub pushes: broadcast::Sender<Arc<Push>>,
    /// The current version of each published page, by key.
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

/// The root also reports each write, so a config read mid-save can be told apart. No other
/// folder can hold the config, so no other folder pays for the events.
const ROOT_MASK: WatchFlags = MASK.union(WatchFlags::MODIFY);

/// The longest a burst of events holds back the index pages and the manifest.
const DERIVE_MAX: Duration = Duration::from_secs(1);

struct Event {
    wd: i32,
    flags: ReadFlags,
    name: Option<OsString>,
}

/// What a config reload did.
#[derive(Debug, PartialEq, Eq)]
enum Reload {
    /// A new config is in force.
    Loaded,
    /// The file is missing or broken, so the previous config stays.
    Kept,
    /// The file was written during the read, so the previous config stays until that
    /// save's own close.
    MidSave,
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
    /// When the first pass since the last derive began.
    since: Option<Instant>,
    /// Events a config reload read from the queue, still to be applied.
    pending: Vec<Event>,
    /// The `hosts` of the config in force. The server reads it once, after the start.
    hosts: Option<Vec<String>>,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The vault, as one batch reads it. A held path may be mid-save, and so may a file at a
/// path whose last event removed it: a new file is there, and its save may not be done.
struct Batch<'a> {
    vault: &'a VaultDir,
    held: &'a HashMap<String, Instant>,
    removed: &'a HashSet<String>,
    /// The paths read as mid-save that no hold covers yet.
    unheld: Vec<String>,
}

impl Batch<'_> {
    fn saved(&self, path: &str) -> bool {
        !self.held.contains_key(path) && !self.removed.contains(path)
    }

    /// Records a read that found a path mid-save with no hold to end the wait.
    fn noted<T>(&mut self, path: &str, read: Result<T, ReadError>) -> Result<T, ReadError> {
        if matches!(read, Err(ReadError::MidSave)) && !self.held.contains_key(path) {
            self.unheld.push(path.to_string());
        }
        read
    }
}

impl Source for Batch<'_> {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        let read = self.vault.read_note(path, self.saved(path));
        self.noted(path, read)
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        let read = self.vault.open_after(path, self.saved(path)).map(|_| ());
        self.noted(path, read)
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
        // Non-blocking, so a read tells the watcher its queue is empty.
        let inotify = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).map_err(|e| {
            Error::Watch {
                path: vault.root().to_path_buf(),
                source: e.into(),
            }
        })?;
        let watched = config.exclude.clone();
        let mut site = Site::new(config.exclude);
        site.set_nav(config.nav);
        site.set_obsidian(config::obsidian_daily(&vault));
        let mut watcher = Watcher {
            inotify,
            dirs: HashMap::new(),
            vault,
            out,
            site,
            live,
            held: HashMap::new(),
            hold: hold(),
            since: None,
            pending: Vec::new(),
            hosts: config.hosts,
        };
        watcher.watch_tree("")?;
        // A config saved before the root watch existed sent no event. One still being saved
        // may have been read half-written before the watch, so the first sync waits for it.
        loop {
            let from = watcher.pending.len();
            match watcher.reload_config()? {
                // The tree was watched under `config`, so only other exclusions move a watch.
                Reload::Loaded => {
                    if *watcher.site.exclude() != watched {
                        watcher.rewatch(&[""])?;
                    }
                    break;
                }
                Reload::Kept => break,
                Reload::MidSave => watcher.await_config_close(from)?,
            }
        }
        let sync = watcher.sync("")?;
        watcher.apply(vec![sync], HashSet::new())?;
        watcher.derive()?;
        Ok(watcher)
    }

    /// The exclusions in force.
    pub fn exclude(&self) -> &Exclude {
        self.site.exclude()
    }

    /// The names the server answers, from the config the start ended with.
    pub fn hosts(&self) -> Option<&[String]> {
        self.hosts.as_deref()
    }

    /// Runs until a fatal error, which it returns.
    pub fn run(mut self) -> Error {
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 64 * 1024];
        loop {
            let next = if self.pending.is_empty() {
                self.read_batch(&mut buf)
            } else {
                Ok((std::mem::take(&mut self.pending), false))
            };
            let result = next.and_then(|(events, drained)| {
                let (changes, removed) = self.changes(events)?;
                // A config reload may have read more events, and they come next.
                let drained = drained && self.pending.is_empty();
                let unpublished = self.apply(changes, removed)?;
                // Index pages wait until no event is queued, so a burst derives them once.
                // An unpublished page leaves them at once, and a long burst still derives
                // them every `DERIVE_MAX`.
                let burst = self.since.get_or_insert_with(Instant::now).elapsed();
                if drained || unpublished || burst >= DERIVE_MAX {
                    self.derive()?;
                }
                if drained {
                    self.wait()?;
                }
                Ok(())
            });
            if let Err(e) = result {
                return e;
            }
        }
    }

    /// Waits for an event, or until the first hold ends. With no path held it has no
    /// timeout, so idle costs nothing.
    fn wait(&self) -> Result<(), Error> {
        let timeout =
            self.held.values().min().and_then(|end| {
                Timespec::try_from(end.saturating_duration_since(Instant::now())).ok()
            });
        let mut fds = [PollFd::new(&self.inotify, PollFlags::IN)];
        match poll(&mut fds, timeout.as_ref()) {
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(e) => Err(self.watch_error(e)),
        }
    }

    fn watch_error(&self, e: rustix::io::Errno) -> Error {
        Error::Watch {
            path: self.vault.root().to_path_buf(),
            source: e.into(),
        }
    }

    /// The events in one read's buffer, and whether the queue was empty after them.
    /// The read never blocks.
    fn read_batch(&self, buf: &mut [MaybeUninit<u8>]) -> Result<(Vec<Event>, bool), Error> {
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
                Err(rustix::io::Errno::AGAIN) => return Ok((events, true)),
                Err(e) => return Err(self.watch_error(e)),
            }
            if reader.is_buffer_empty() {
                return Ok((events, false));
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
        let mut daily = false;
        let now = Instant::now();
        for event in events {
            if event.flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                eprintln!("inotify queue overflowed; rescanning the vault");
                // The lost events may include a save of the config.
                self.reload_config()?;
                return self.resync(removed);
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
            // Obsidian's folder may come or go while the server runs.
            if dir.is_empty() && name == ".obsidian" {
                if event
                    .flags
                    .intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO)
                {
                    self.watch_obsidian();
                }
                daily = true;
                continue;
            }
            if Exclude::segment(name.as_bytes()) {
                continue;
            }
            let Some(name) = name.to_str() else { continue };
            let path = join(&dir, name);
            if path == config::DAILY_NOTES {
                daily |= !event
                    .flags
                    .intersects(ReadFlags::MODIFY | ReadFlags::CREATE);
                continue;
            }
            if path == config::FILE {
                // Only a save reloads: mid-save, the file can be missing, empty or half
                // written, and each would publish what it excludes. A write after the save
                // cancels it, and that write's own close reloads.
                if event
                    .flags
                    .intersects(ReadFlags::CLOSE_WRITE | ReadFlags::MOVED_TO)
                {
                    reload = true;
                } else if event
                    .flags
                    .intersects(ReadFlags::MODIFY | ReadFlags::CREATE)
                {
                    reload = false;
                }
                continue;
            }
            // Only the config's writes matter; a note's save sends its close.
            if event.flags.contains(ReadFlags::MODIFY) {
                continue;
            }
            if event.flags.contains(ReadFlags::ISDIR) {
                // `rsync -t` sets every folder's times, so an attribute change rescans a
                // folder only when it became readable or unreadable.
                let attrib = event.flags.contains(ReadFlags::ATTRIB);
                if !self.site.exclude().path(&path) && !(attrib && self.settled(&path)) {
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
        if reload {
            let previous = (self.site.exclude().clone(), self.site.nav().clone());
            if self.reload_config()? == Reload::Loaded {
                return self.resync_reloaded(previous, removed);
            }
        }
        // Every page's header shows whether there is a Today page, so a change renders all.
        if daily && self.site.set_obsidian(config::obsidian_daily(&self.vault)) {
            return self.resync(removed);
        }

        // The folders as they stand now, each once: a parent's sync covers its children.
        let mut tops: Vec<&str> = Vec::new();
        for folder in folders.keys() {
            if !tops.iter().any(|top| within(folder, top)) {
                tops.push(folder);
            }
        }
        let created: Vec<&str> = folders
            .iter()
            .filter(|(_, created)| **created)
            .map(|(folder, _)| folder.as_str())
            .collect();
        self.rewatch(&tops)?;
        let mut changes = Vec::new();
        for folder in tops {
            let sync = self.sync(folder)?;
            // A file in a new folder may be mid-save, and its CREATE came before the watch.
            if let Change::Sync { entries, .. } = &sync {
                for entry in entries {
                    if created.iter().any(|new| within(&entry.path, new)) {
                        self.held.insert(entry.path.clone(), now + self.hold);
                    }
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

    /// Rereads `tachylite.toml`. A missing, broken or mid-save file keeps the previous config.
    fn reload_config(&mut self) -> Result<Reload, Error> {
        let read = config::reload(&self.vault);
        // The kernel queues IN_MODIFY for each truncate and write just after it lands. One
        // queued after the read means the read may have seen the file empty or half written.
        if self.config_written()? {
            return Ok(Reload::MidSave);
        }
        match read {
            Ok(Some(config)) => {
                self.site.set_exclude(config.exclude);
                self.site.set_nav(config.nav);
                self.hosts = config.hosts;
                Ok(Reload::Loaded)
            }
            Ok(None) => Ok(Reload::Kept),
            Err(e) => {
                eprintln!("{e}; keeping the previous config");
                Ok(Reload::Kept)
            }
        }
    }

    /// Moves every queued event to `pending`, and says whether one is a write to the config,
    /// or a lost event that could be one.
    fn config_written(&mut self) -> Result<bool, Error> {
        let from = self.pending.len();
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 4096];
        loop {
            let (events, drained) = self.read_batch(&mut buf)?;
            self.pending.extend(events);
            if drained {
                break;
            }
        }
        let written =
            ReadFlags::MODIFY | ReadFlags::CREATE | ReadFlags::CLOSE_WRITE | ReadFlags::MOVED_TO;
        Ok(self.pending[from..].iter().any(|e| {
            e.flags.contains(ReadFlags::QUEUE_OVERFLOW)
                || (e.flags.intersects(written) && self.names_config(e))
        }))
    }

    fn names_config(&self, event: &Event) -> bool {
        event.name.as_deref() == Some(config::FILE.as_ref())
            && self.dirs.get(&event.wd).is_some_and(String::is_empty)
    }

    /// Waits until the config's last event since `pending[from]` closes or removes it, or the
    /// queue overflows. Only start-up waits: with no config yet in force, it has none to keep.
    fn await_config_close(&mut self, from: usize) -> Result<(), Error> {
        eprintln!(
            "{} is being saved; waiting for the save to finish",
            config::FILE
        );
        let closed = ReadFlags::CLOSE_WRITE
            | ReadFlags::MOVED_TO
            | ReadFlags::DELETE
            | ReadFlags::MOVED_FROM;
        loop {
            let since = &self.pending[from..];
            // An attribute change says nothing about a save in progress.
            let last = since
                .iter()
                .rev()
                .find(|e| !e.flags.contains(ReadFlags::ATTRIB) && self.names_config(e));
            if last.is_some_and(|e| e.flags.intersects(closed))
                || since
                    .iter()
                    .any(|e| e.flags.contains(ReadFlags::QUEUE_OVERFLOW))
            {
                return Ok(());
            }
            self.wait()?;
            let mut buf = vec![MaybeUninit::<u8>::uninit(); 4096];
            let (events, _) = self.read_batch(&mut buf)?;
            self.pending.extend(events);
        }
    }

    /// Resyncs under a config just loaded, or under `previous` if the config was written
    /// before the rescan ended. A write's event lands a moment after its bytes, so a writer
    /// stalled in between got past the reload's check. The rescan gave it time to queue.
    fn resync_reloaded(
        &mut self,
        (exclude, nav): (Exclude, Nav),
        removed: HashSet<String>,
    ) -> Result<(Vec<Change>, HashSet<String>), Error> {
        let (changes, removed) = self.resync(removed)?;
        if self.config_written()? {
            self.site.set_exclude(exclude);
            self.site.set_nav(nav);
            return self.resync(removed);
        }
        report::unmatched(&self.site.exclude().unmatched());
        Ok((changes, removed))
    }

    /// Rewatches the tree under the current exclusions and resyncs the root.
    fn resync(
        &mut self,
        removed: HashSet<String>,
    ) -> Result<(Vec<Change>, HashSet<String>), Error> {
        self.rewatch(&[""])?;
        // A rescan renders every page, and its cause may have hidden a save of these.
        self.site.set_obsidian(config::obsidian_daily(&self.vault));
        Ok((vec![self.sync("")?], removed))
    }

    /// Watches `folders` and every published folder below them, as they stand now. A
    /// folder still in place keeps its watch descriptor, so no event queued for it is lost.
    fn rewatch(&mut self, folders: &[&str]) -> Result<(), Error> {
        if folders.is_empty() {
            return Ok(());
        }
        // Watches and scans go by path, so a renamed parent would point them at nothing, or
        // at another folder.
        if self.vault.moved() {
            return Err(Error::VaultGone(self.vault.root().to_path_buf()));
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

    /// Whether a folder's watch matches whether it can be read: watched and readable, or
    /// neither. Only a change between the two needs a rescan.
    fn settled(&self, rel: &str) -> bool {
        let watched = self.dirs.values().any(|dir| dir == rel);
        watched == fs::read_dir(self.vault.root().join(rel)).is_ok()
    }

    fn sync(&self, under: &str) -> Result<Change, Error> {
        let scan = scan_under(self.vault.root(), under, self.site.exclude())?;
        report::skipped(&scan.skipped);
        Ok(Change::Sync {
            under: under.to_string(),
            entries: scan.entries,
        })
    }

    /// Obsidian's daily-notes settings name the Today page's notes. The folder is never
    /// published, so its watch only reports changes to that file. A vault may have none.
    fn watch_obsidian(&mut self) {
        let obsidian = self.vault.root().join(".obsidian");
        if let Ok(wd) = inotify::add_watch(&self.inotify, &obsidian, MASK) {
            self.dirs.insert(wd, ".obsidian".to_string());
        }
    }

    /// Adds a watch to `rel` and every published folder below it. A folder that vanished
    /// or cannot be read is left to the scan to report.
    fn watch_tree(&mut self, rel: &str) -> Result<(), Error> {
        let path = self.vault.root().join(rel);
        let mask = if rel.is_empty() { ROOT_MASK } else { MASK };
        match inotify::add_watch(&self.inotify, &path, mask) {
            Ok(wd) => {
                self.dirs.insert(wd, rel.to_string());
                if rel.is_empty() {
                    self.watch_obsidian();
                }
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

    /// Applies a batch. Returns whether it unpublished a path.
    fn apply(&mut self, changes: Vec<Change>, removed: HashSet<String>) -> Result<bool, Error> {
        if changes.is_empty() {
            return Ok(false);
        }
        let mut batch = Batch {
            vault: &self.vault,
            held: &self.held,
            removed: &removed,
            unheld: Vec::new(),
        };
        let mut writer = writer(&self.out, &self.vault, &self.live);
        let report = self.site.apply_to(changes, &mut batch, &mut writer);
        // A file at a removed path sends no event if it was saved before its folder's watch,
        // in a folder made again or moved into place. A hold reads it whatever follows.
        let end = Instant::now() + self.hold;
        for path in batch.unheld {
            self.held.insert(path, end);
        }
        let failed = writer.finish()?;
        report::notices(&report.notices);
        report::skipped(&report.skipped);
        report::skipped(&failed);
        report::broken(&report.broken);
        Ok(report.unpublished)
    }

    /// Writes the index pages and the manifest that the applies since the last left due.
    fn derive(&mut self) -> Result<(), Error> {
        self.since = None;
        let mut writer = writer(&self.out, &self.vault, &self.live);
        self.site.derive_to(&mut writer);
        // An index page whose path the output refuses is skipped.
        report::skipped(&writer.finish()?);
        Ok(())
    }
}

/// A writer into `out` that records each push's version and sends it to the clients.
fn writer<'a>(
    out: &'a std::path::Path,
    vault: &'a VaultDir,
    live: &'a Live,
) -> Writer<'a, impl FnMut(Push) + 'a> {
    Writer::new(out, vault, move |push: Push| {
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
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A watcher on `vault`, with its output beside it.
    fn watcher(vault: &Path) -> Watcher {
        let out = vault.with_file_name("out");
        let (vault, out) = crate::build::open(vault, &out).unwrap();
        crate::output::prepare(&out).unwrap();
        let live = Live {
            pushes: broadcast::channel(4).0,
            versions: Arc::default(),
        };
        Watcher::start(vault, out, live, Config::default()).unwrap()
    }

    fn wd(watcher: &Watcher, dir: &str) -> i32 {
        *watcher.dirs.iter().find(|(_, d)| *d == dir).unwrap().0
    }

    fn event(wd: i32, flags: ReadFlags, name: &str) -> Event {
        Event {
            wd,
            flags,
            name: Some(name.into()),
        }
    }

    fn vault() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        fs::create_dir(&vault).unwrap();
        (tmp, vault)
    }

    #[test]
    fn a_file_the_site_would_not_publish_changes_nothing() {
        let (_tmp, vault) = vault();
        let mut watcher = watcher(&vault);
        let root = wd(&watcher, "");
        let saves = vec![
            event(root, ReadFlags::CLOSE_WRITE, "scratch.txt"),
            event(root, ReadFlags::CLOSE_WRITE, "n.md"),
        ];
        let (changes, _) = watcher.changes(saves).unwrap();
        assert_eq!(changes, [Change::Touch("n.md".into())]);
    }

    #[test]
    fn a_write_to_a_root_note_waits_for_its_close() {
        let (_tmp, vault) = vault();
        let mut watcher = watcher(&vault);
        let root = wd(&watcher, "");
        let (changes, _) = watcher
            .changes(vec![event(root, ReadFlags::MODIFY, "n.md")])
            .unwrap();
        assert_eq!(changes, []);
    }

    #[test]
    fn new_times_on_a_watched_folder_change_nothing() {
        let (_tmp, vault) = vault();
        fs::create_dir(vault.join("d")).unwrap();
        fs::write(vault.join("d/n.md"), "# n").unwrap();
        let mut watcher = watcher(&vault);
        let root = wd(&watcher, "");
        let attrib = event(root, ReadFlags::ATTRIB | ReadFlags::ISDIR, "d");
        let (changes, _) = watcher.changes(vec![attrib]).unwrap();
        assert_eq!(changes, []);
    }

    #[test]
    fn a_new_folder_is_held_when_its_parent_syncs_it() {
        let (_tmp, vault) = vault();
        fs::create_dir(vault.join("a")).unwrap();
        let mut watcher = watcher(&vault);
        let (root, a) = (wd(&watcher, ""), wd(&watcher, "a"));
        fs::write(vault.join("a/x.md"), "# x").unwrap();
        fs::create_dir(vault.join("a/new")).unwrap();
        fs::write(vault.join("a/new/n.md"), "# n").unwrap();
        // The parent's own event makes one sync of `a` cover the new child.
        let events = vec![
            event(root, ReadFlags::MOVED_TO | ReadFlags::ISDIR, "a"),
            event(a, ReadFlags::CREATE | ReadFlags::ISDIR, "new"),
        ];
        watcher.changes(events).unwrap();
        assert!(watcher.held.contains_key("a/new/n.md"));
        assert!(!watcher.held.contains_key("a/x.md"));
    }

    #[test]
    fn a_config_written_during_its_read_is_kept_until_its_save_closes() {
        use std::io::Write;
        let (_tmp, vault) = vault();
        fs::write(vault.join(config::FILE), "exclude = [\"d\"]\n").unwrap();
        let mut watcher = watcher(&vault);
        let mut file = fs::File::create(vault.join(config::FILE)).unwrap();
        file.write_all(b"# half\n").unwrap();
        let from = watcher.pending.len();
        assert_eq!(watcher.reload_config().unwrap(), Reload::MidSave);
        assert!(watcher.site.exclude().path("d/n.md"));
        file.write_all(b"exclude = [\"e\"]\n").unwrap();
        drop(file);
        // Returns at once: the save's close is queued.
        watcher.await_config_close(from).unwrap();
        assert_eq!(watcher.reload_config().unwrap(), Reload::Loaded);
        assert!(watcher.site.exclude().path("e/n.md"));
    }

    #[test]
    fn start_up_takes_a_save_followed_by_an_attribute_change_as_closed() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, vault) = vault();
        let toml = vault.join(config::FILE);
        fs::write(&toml, "exclude = []\n").unwrap();
        let mut watcher = watcher(&vault);
        let from = watcher.pending.len();
        fs::write(&toml, "exclude = [\"d\"]\n").unwrap();
        fs::set_permissions(&toml, fs::Permissions::from_mode(0o644)).unwrap();
        // Hangs if the attribute change hides the close.
        watcher.await_config_close(from).unwrap();
    }

    #[test]
    fn a_config_written_during_the_rescan_keeps_the_previous_one() {
        let (_tmp, vault) = vault();
        let toml = vault.join(config::FILE);
        fs::write(&toml, "exclude = [\"d\"]\n").unwrap();
        let mut watcher = watcher(&vault);
        let previous = watcher.site.exclude().clone();
        fs::write(&toml, "exclude = []\n").unwrap();
        // The save's own events, read as the batch that reloads it.
        watcher.config_written().unwrap();
        watcher.pending.clear();
        assert_eq!(watcher.reload_config().unwrap(), Reload::Loaded);
        // A write that slipped past the reload's check, queued by the rescan's end.
        fs::write(&toml, "# half\n").unwrap();
        watcher
            .resync_reloaded((previous, Nav::default()), HashSet::new())
            .unwrap();
        assert!(watcher.site.exclude().path("d/n.md"));
    }

    #[test]
    fn the_start_takes_hosts_from_its_own_read() {
        let (_tmp, vault) = vault();
        fs::write(vault.join(config::FILE), "hosts = [\"notes.example\"]\n").unwrap();
        // The helper passes the default config, as a first read of a half-written file gives.
        let watcher = watcher(&vault);
        assert_eq!(watcher.hosts(), Some(&["notes.example".to_string()][..]));
    }

    #[test]
    fn a_config_reload_keeps_the_removed_paths() {
        let (_tmp, vault) = vault();
        let mut watcher = watcher(&vault);
        let root = wd(&watcher, "");
        fs::write(vault.join(config::FILE), "exclude = []\n").unwrap();
        let events = vec![
            event(root, ReadFlags::DELETE, "n.md"),
            event(root, ReadFlags::CLOSE_WRITE, config::FILE),
        ];
        let (_, removed) = watcher.changes(events).unwrap();
        assert!(removed.contains("n.md"));
    }

    #[test]
    fn a_queue_overflow_holds_a_note_saved_again_in_a_new_folder() {
        let (_tmp, vault) = vault();
        fs::create_dir(vault.join("d")).unwrap();
        fs::write(vault.join("d/n.md"), "one").unwrap();
        let mut watcher = watcher(&vault);
        let d = wd(&watcher, "d");
        fs::remove_dir_all(vault.join("d")).unwrap();
        fs::create_dir(vault.join("d")).unwrap();
        fs::write(vault.join("d/n.md"), "two").unwrap();
        // The kernel kept the note's delete and dropped every event after it. The note was
        // saved before the new folder's watch, so none follows.
        watcher.config_written().unwrap();
        watcher.pending.clear();
        let overflow = Event {
            wd: -1,
            flags: ReadFlags::QUEUE_OVERFLOW,
            name: None,
        };
        let events = vec![event(d, ReadFlags::DELETE, "n.md"), overflow];
        let (changes, removed) = watcher.changes(events).unwrap();
        watcher.apply(changes, removed).unwrap();
        assert!(watcher.held.contains_key("d/n.md"));
    }
}
