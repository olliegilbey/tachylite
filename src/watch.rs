//! The watch edge: inotify events in, site changes applied, output written, pushes sent.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use tokio::sync::broadcast;

use crate::site::{Change, Push, Site, Source};
use crate::vault::{ReadError, VaultDir, excluded_segment, scan_under};
use crate::{Error, output, report};

/// What the watch thread shares with the HTTP thread.
#[derive(Clone)]
pub struct Live {
    pub pushes: broadcast::Sender<Arc<Push>>,
    /// The current version of each published note page, by key.
    pub versions: Arc<Mutex<HashMap<String, String>>>,
}

const MASK: WatchFlags = WatchFlags::CLOSE_WRITE
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR)
    .union(WatchFlags::DONT_FOLLOW)
    .union(WatchFlags::EXCL_UNLINK);

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
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The vault, as one batch reads it. A path whose events held no save may be mid-save.
struct Batch<'a> {
    vault: &'a VaultDir,
    unsaved: &'a HashSet<String>,
}

impl Source for Batch<'_> {
    fn note(&mut self, path: &str) -> Result<Vec<u8>, ReadError> {
        self.vault.read_after(path, !self.unsaved.contains(path))
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.vault
            .open_after(path, !self.unsaved.contains(path))
            .map(|_| ())
    }
}

fn within(path: &str, folder: &str) -> bool {
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl Watcher {
    /// Watches every published folder, then applies a full sync. Watching first means no
    /// change is lost between the scan and the first event.
    pub fn start(vault: VaultDir, out: PathBuf, live: Live) -> Result<Watcher, Error> {
        let inotify = inotify::init(CreateFlags::CLOEXEC).map_err(|e| Error::Watch {
            path: vault.root().to_path_buf(),
            source: e.into(),
        })?;
        let mut watcher = Watcher {
            inotify,
            dirs: HashMap::new(),
            vault,
            out,
            site: Site::new(),
            live,
        };
        watcher.watch_tree("")?;
        let sync = watcher.sync("")?;
        watcher.apply(vec![sync], HashSet::new())?;
        Ok(watcher)
    }

    /// Runs until a fatal error, which it returns.
    pub fn run(mut self) -> Error {
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 64 * 1024];
        loop {
            let result = self
                .read_batch(&mut buf)
                .and_then(|events| self.changes(events))
                .and_then(|(changes, unsaved)| self.apply(changes, unsaved));
            if let Err(e) = result {
                return e;
            }
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

    /// The batch's changes, and the touched paths none of whose events was a save.
    fn changes(&mut self, events: Vec<Event>) -> Result<(Vec<Change>, HashSet<String>), Error> {
        let mut changes = Vec::new();
        // Path to whether any of its events was a save.
        let mut touched: BTreeMap<String, bool> = BTreeMap::new();
        for event in events {
            if event.flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                eprintln!("inotify queue overflowed; rescanning the vault");
                self.watch_tree("")?;
                return Ok((vec![self.sync("")?], HashSet::new()));
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
            if excluded_segment(name.as_bytes()) {
                continue;
            }
            let Some(name) = name.to_str() else { continue };
            let path = join(&dir, name);
            if event.flags.contains(ReadFlags::ISDIR) {
                if event
                    .flags
                    .intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO)
                {
                    self.watch_tree(&path)?;
                    changes.push(self.sync(&path)?);
                } else if event
                    .flags
                    .intersects(ReadFlags::MOVED_FROM | ReadFlags::DELETE)
                {
                    self.unwatch(&path);
                    changes.push(Change::Sync {
                        under: path,
                        entries: Vec::new(),
                    });
                }
            } else {
                let save = event
                    .flags
                    .intersects(ReadFlags::CLOSE_WRITE | ReadFlags::MOVED_TO);
                *touched.entry(path).or_default() |= save;
            }
        }
        let unsaved = touched
            .iter()
            .filter(|(_, saved)| !**saved)
            .map(|(path, _)| path.clone())
            .collect();
        changes.extend(touched.into_keys().map(Change::Touch));
        Ok((changes, unsaved))
    }

    fn sync(&self, under: &str) -> Result<Change, Error> {
        let scan = scan_under(self.vault.root(), under)?;
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
            if !is_dir || excluded_segment(name.as_bytes()) {
                continue;
            }
            if let Some(name) = name.to_str() {
                self.watch_tree(&join(rel, name))?;
            }
        }
        Ok(())
    }

    fn unwatch(&mut self, rel: &str) {
        let gone: Vec<i32> = self
            .dirs
            .iter()
            .filter(|(_, dir)| within(dir, rel))
            .map(|(wd, _)| *wd)
            .collect();
        for wd in gone {
            // The folder may already be gone, which removes its watch too.
            let _ = inotify::remove_watch(&self.inotify, wd);
            self.dirs.remove(&wd);
        }
    }

    fn apply(&mut self, changes: Vec<Change>, unsaved: HashSet<String>) -> Result<(), Error> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut batch = Batch {
            vault: &self.vault,
            unsaved: &unsaved,
        };
        let delta = self.site.apply(changes, &mut batch);
        let failed = output::apply(&self.out, &delta, &self.vault)?;
        report::skipped(&delta.skipped);
        report::skipped(&failed);
        {
            let mut versions = self
                .live
                .versions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for push in &delta.pushes {
                match push {
                    Push::Page { key, version, .. } => {
                        versions.insert(key.clone(), version.clone());
                    }
                    Push::Gone { key } => {
                        versions.remove(key);
                    }
                }
            }
        }
        for push in delta.pushes {
            // An error only means no client is listening.
            let _ = self.live.pushes.send(Arc::new(push));
        }
        Ok(())
    }
}
