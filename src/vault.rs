use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};
use rustix::io::Errno;

use crate::Error;
use crate::config::Exclude;
use crate::tick::{self, To, Token};

const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "svg", "webp", "avif"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Note,
    Html,
    Asset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Symlink,
    NonUtf8Name,
    UrlClash,
    NameTooLong,
    /// Larger than `NOTE_MAX`.
    TooLarge,
    Unreadable(io::ErrorKind),
}

/// The largest note read. A render peaks at about 12 times a note's size, and at about
/// 280 times for a note of nothing but `*_`, so a bigger note is skipped rather than risk
/// the process on a small host. An html page past it is copied unread, so no sync downloads it.
pub const NOTE_MAX: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    pub entries: Vec<Entry>,
    pub skipped: Vec<Skipped>,
}

/// The publish list and the exclusions, applied to a vault-relative path.
pub fn classify(path: &str, exclude: &Exclude) -> Option<Kind> {
    if exclude.path(path) {
        return None;
    }
    let name = path.rsplit('/').next()?;
    let (_, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    match ext.as_str() {
        "md" => Some(Kind::Note),
        "html" => Some(Kind::Html),
        "pdf" => Some(Kind::Asset),
        e if IMAGE_EXTENSIONS.contains(&e) => Some(Kind::Asset),
        _ => None,
    }
}

pub fn is_image(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Lists what the vault publishes. Reads only; never follows a symlink or enters an excluded folder.
pub fn scan(vault: &Path, exclude: &Exclude) -> Result<Scan, Error> {
    scan_under(vault, "", exclude)
}

/// Lists what the vault publishes under the folder `under` (`""` is the root). Paths stay
/// vault-relative. A subfolder that vanished before it was read lists nothing.
pub fn scan_under(vault: &Path, under: &str, exclude: &Exclude) -> Result<Scan, Error> {
    let mut scan = Scan {
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    walk(vault, Path::new(under), exclude, &mut scan)?;
    scan.skipped.retain(|s| {
        !(s.path == Path::new(under) && s.reason == SkipReason::Unreadable(io::ErrorKind::NotFound))
    });
    scan.entries.sort_by(|a, b| a.path.cmp(&b.path));
    scan.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(scan)
}

fn walk(vault: &Path, rel: &Path, exclude: &Exclude, scan: &mut Scan) -> Result<(), Error> {
    let dir = vault.join(rel);
    let io = |source| Error::Io {
        path: dir.clone(),
        source,
    };
    let items = match fs::read_dir(&dir) {
        Ok(items) => items,
        Err(e) if !rel.as_os_str().is_empty() => {
            scan.skipped.push(Skipped {
                path: rel.to_path_buf(),
                reason: SkipReason::Unreadable(e.kind()),
            });
            return Ok(());
        }
        Err(source) => return Err(io(source)),
    };
    for item in items {
        let item = item.map_err(io)?;
        let name = item.file_name();
        if Exclude::segment(name.as_bytes()) {
            continue;
        }
        let rel_path = rel.join(&name);
        let Some(rel_str) = rel_path.to_str() else {
            scan.skipped.push(Skipped {
                path: rel_path,
                reason: SkipReason::NonUtf8Name,
            });
            continue;
        };
        if exclude.path(rel_str) {
            continue;
        }
        let file_type = item.file_type().map_err(io)?;
        if file_type.is_symlink() {
            scan.skipped.push(Skipped {
                path: rel_path,
                reason: SkipReason::Symlink,
            });
        } else if file_type.is_dir() {
            walk(vault, &rel_path, exclude, scan)?;
        } else if !file_type.is_file() {
            continue;
        } else if let Some(kind) = classify(rel_str, exclude) {
            scan.entries.push(Entry {
                path: rel_str.to_string(),
                kind,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn classify_publish_list() {
        let cases: &[(&str, Option<Kind>)] = &[
            ("a.md", Some(Kind::Note)),
            ("A.MD", Some(Kind::Note)),
            ("people/Jane Doe.md", Some(Kind::Note)),
            ("maps/route.html", Some(Kind::Html)),
            ("x.pdf", Some(Kind::Asset)),
            ("x.py", None),
            (".env", None),
            ("x.json", None),
            ("x.sql", None),
            ("x.txt", None),
            ("README", None),
            ("x.htm", None),
            (".obsidian/a.md", None),
            ("a/.hidden/b.md", None),
            ("target/a.md", None),
            ("a/node_modules/b.md", None),
            (".md", None),
        ];
        for (path, want) in cases {
            assert_eq!(classify(path, &Exclude::default()), *want, "{path}");
        }
        for ext in ["png", "jpg", "jpeg", "gif", "svg", "webp", "avif"] {
            assert_eq!(
                classify(&format!("img/x.{ext}"), &Exclude::default()),
                Some(Kind::Asset),
                "{ext}"
            );
            let upper = ext.to_uppercase();
            assert_eq!(
                classify(&format!("img/x.{upper}"), &Exclude::default()),
                Some(Kind::Asset),
                "{upper}"
            );
        }
    }

    /// Each file of `src/` that holds `needle`, with its count. A `|` in a needle keeps
    /// this test from holding it.
    fn holding(files: &[(String, String)], needle: &str) -> Vec<(String, usize)> {
        let needle = needle.replace('|', "");
        files
            .iter()
            .map(|(name, text)| (name.clone(), text.matches(&needle).count()))
            .filter(|(_, count)| *count > 0)
            .collect()
    }

    /// Tachylite writes one byte of a note, in `VaultDir::tick`. Every other open of a
    /// vault file reads, and the only other code that writes a file writes the output, or
    /// generates the budget vault.
    #[test]
    fn tick_is_the_only_write_to_the_vault() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        for entry in fs::read_dir(&src).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                let name = path.file_name().unwrap().to_str().unwrap().to_string();
                files.push((name, fs::read_to_string(&path).unwrap()));
            }
        }
        files.sort();
        let only = |name: &str, count| vec![(name.to_string(), count)];
        // The opens beneath the vault, and the one that may write.
        assert_eq!(holding(&files, "open|at2("), only("vault.rs", 3));
        assert_eq!(holding(&files, "RD|WR"), only("vault.rs", 1));
        assert_eq!(holding(&files, "write_|all_at"), only("vault.rs", 1));
        for needle in [
            "WR|ONLY",
            "OFlags::CRE|ATE",
            "OFlags::TR|UNC",
            "OFlags::APP|END",
            "write_|at(",
            "write_|vectored_at",
            "pwr|ite",
            "ftrunc|ate",
            "rename|at",
            "unlink|at",
            "mkdir|at",
            "link|at(",
        ] {
            assert_eq!(holding(&files, needle), [], "{needle}");
        }
        // The shipped code of every other file: what comes before its tests. This file's
        // tests sit mid-file, so it is read whole.
        let shipped: Vec<(String, String)> = files
            .into_iter()
            .map(|(name, text)| {
                let end = match name.as_str() {
                    "vault.rs" => text.len(),
                    _ => text.find("#[cfg(test)]").unwrap_or(text.len()),
                };
                (name, text[..end].to_string())
            })
            .collect();
        let mut writers = std::collections::BTreeSet::new();
        for needle in [
            "fs::wr|ite",
            "File::cre|ate",
            "Open|Options",
            "File::opt|ions",
            "fs::ren|ame",
            "remove_|file",
            "remove_|dir",
            "create_|dir",
            "set_perm|issions",
            "set_|len",
            "set_mod|ified",
            "set_tim|es",
            "fs::co|py",
            "hard_|link",
            "fs::sym|link",
        ] {
            writers.extend(holding(&shipped, needle).into_iter().map(|(name, _)| name));
        }
        let writers: Vec<&str> = writers.iter().map(String::as_str).collect();
        assert_eq!(writers, ["budget_vault.rs", "build.rs", "output.rs"]);
    }

    fn segment() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec![
            "a",
            "B c",
            ".git",
            ".x",
            "target",
            "node_modules",
            "targets",
        ])
    }

    fn extension() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec!["md", "MD", "html", "png", "PDF", "py", "json", ""])
    }

    proptest! {
        #[test]
        fn classified_paths_are_published_and_not_excluded(
            dirs in prop::collection::vec(segment(), 0..4),
            stem in segment(),
            ext in extension(),
        ) {
            let name = if ext.is_empty() { stem.to_string() } else { format!("{stem}.{ext}") };
            let mut segments: Vec<String> = dirs.iter().map(|s| s.to_string()).collect();
            segments.push(name.clone());
            let path = segments.join("/");

            let excluded = segments
                .iter()
                .any(|s| s.starts_with('.') || s == "target" || s == "node_modules");
            let published = !ext.is_empty()
                && ["md", "html", "png", "pdf"].contains(&ext.to_lowercase().as_str());
            prop_assert_eq!(classify(&path, &Exclude::default()).is_some(), !excluded && published, "{}", path);
        }
    }
}

/// Why a vault file could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Gone, or not a regular file. The scan ignores both silently, so reads do too.
    Missing,
    Skip(SkipReason),
    /// A new file that may be half written. Its save is still to come, so the path keeps
    /// what is published for it.
    MidSave,
}

/// How long a path stays held as mid-save with no save event. A save by `link()` sends
/// none, and neither does a note closed before its new folder's watch existed.
const HOLD: Duration = Duration::from_secs(1);

/// Sets the hold in milliseconds, so property tests do not wait out a second per case.
const HOLD_ENV: &str = "TACHYLITE_HOLD_MS";

/// The hold, from `TACHYLITE_HOLD_MS` when it is set.
pub fn hold() -> Duration {
    std::env::var(HOLD_ENV)
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map_or(HOLD, Duration::from_millis)
}

/// Why a tick wrote nothing.
#[derive(Debug)]
pub enum TickError {
    /// Gone, a symlink, or not a regular file.
    Missing,
    /// The task changed since its page was rendered, or the note is too large to render.
    Conflict,
    /// The note does not take writes.
    ReadOnly,
    Io(io::Error),
}

/// An open vault directory. Every open resolves beneath it and never follows a symlink.
/// Every open reads, but the one in `tick`.
#[derive(Debug)]
pub struct VaultDir {
    fd: OwnedFd,
    root: PathBuf,
}

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS);

impl VaultDir {
    pub fn open(root: &Path) -> Result<VaultDir, Error> {
        let io_err = |source: io::Error| Error::Io {
            path: root.to_path_buf(),
            source,
        };
        let root = root.canonicalize().map_err(io_err)?;
        let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let fd = rustix::fs::open(&root, dir_flags, Mode::empty()).map_err(|e| io_err(e.into()))?;
        match openat2(&fd, ".", dir_flags, Mode::empty(), RESOLVE) {
            Ok(_) => {}
            Err(Errno::NOSYS) => return Err(Error::NoOpenat2),
            Err(e) => return Err(io_err(e.into())),
        }
        Ok(VaultDir { fd, root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A second handle on the same open directory.
    pub fn try_clone(&self) -> io::Result<VaultDir> {
        Ok(VaultDir {
            fd: self.fd.try_clone()?,
            root: self.root.clone(),
        })
    }

    /// Whether the root path no longer names the open vault, as after its parent is renamed.
    pub fn moved(&self) -> bool {
        match (rustix::fs::fstat(&self.fd), rustix::fs::stat(&self.root)) {
            (Ok(open), Ok(named)) => (open.st_dev, open.st_ino) != (named.st_dev, named.st_ino),
            _ => true,
        }
    }

    /// Opens a vault-relative path for reading. A FIFO opens without blocking and reads as missing.
    pub fn open_file(&self, rel: &str) -> Result<File, ReadError> {
        self.open_after(rel, true)
    }

    /// Opens like `open_file`. Unless `saved`, a regular file with one link is `MidSave`: the
    /// watcher holds a new file's path until its save, or its hold ends. The check uses the
    /// open file, so it cannot race the read.
    pub fn open_after(&self, rel: &str, saved: bool) -> Result<File, ReadError> {
        self.stat_after(rel, saved).map(|(file, _)| file)
    }

    /// Checks that a regular file is there, as `open_after` does, without reading it.
    /// Returns its modified time in seconds since the epoch.
    pub fn modified(&self, rel: &str, saved: bool) -> Result<i64, ReadError> {
        self.stat_after(rel, saved).map(|(_, meta)| meta.mtime())
    }

    /// Opens like `open_after`, and returns what the open file's one stat said.
    fn stat_after(&self, rel: &str, saved: bool) -> Result<(File, fs::Metadata), ReadError> {
        let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY;
        let file = match openat2(&self.fd, rel, flags, Mode::empty(), RESOLVE) {
            Ok(fd) => File::from(fd),
            Err(Errno::NOENT | Errno::NOTDIR) => return Err(ReadError::Missing),
            Err(Errno::LOOP) => return Err(ReadError::Skip(SkipReason::Symlink)),
            Err(e) => {
                return Err(ReadError::Skip(SkipReason::Unreadable(
                    io::Error::from(e).kind(),
                )));
            }
        };
        match file.metadata() {
            Ok(meta) if meta.is_file() && !saved && meta.nlink() == 1 => Err(ReadError::MidSave),
            Ok(meta) if meta.is_file() => Ok((file, meta)),
            Ok(_) => Err(ReadError::Missing),
            Err(e) => Err(ReadError::Skip(SkipReason::Unreadable(e.kind()))),
        }
    }

    /// Sets the box of the task `token` names in the note at `rel`: Tachylite's one write to
    /// the vault. It changes one byte in place, so the file keeps its inode, size, owner and
    /// mode. A task already as asked takes no write. The close tells the watcher, as any
    /// save does.
    pub fn tick(&self, rel: &str, token: Token, to: To) -> Result<(), TickError> {
        let flags = OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY;
        let mut file = match openat2(&self.fd, rel, flags, Mode::empty(), RESOLVE) {
            Ok(fd) => File::from(fd),
            // A path that leaves the vault, through `..` or a symlink, names no note.
            Err(
                Errno::NOENT
                | Errno::NOTDIR
                | Errno::LOOP
                | Errno::XDEV
                | Errno::ISDIR
                | Errno::NXIO,
            ) => {
                return Err(TickError::Missing);
            }
            Err(Errno::ACCESS | Errno::PERM | Errno::ROFS | Errno::TXTBSY) => {
                return Err(TickError::ReadOnly);
            }
            Err(e) => return Err(TickError::Io(e.into())),
        };
        let meta = file.metadata().map_err(TickError::Io)?;
        if !meta.is_file() {
            return Err(TickError::Missing);
        }
        if meta.len() > NOTE_MAX {
            return Err(TickError::Conflict);
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(NOTE_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(TickError::Io)?;
        // The file can grow after the check.
        if bytes.len() as u64 > NOTE_MAX {
            return Err(TickError::Conflict);
        }
        let plan = tick::plan(&bytes, token, to).map_err(|tick::Conflict| TickError::Conflict)?;
        let Some((at, byte)) = plan else {
            return Ok(());
        };
        // The task's line as it stands now, with its newline: another writer may have changed
        // the note since the read.
        let start = bytes[..at]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |n| n + 1);
        let newline = bytes[at..].iter().position(|b| *b == b'\n');
        let end = newline.map_or(bytes.len(), |n| at + n + 1);
        // A last line with no newline must still end the file, so one byte more is asked for.
        let mut now = vec![0; end - start + usize::from(newline.is_none())];
        let mut got = 0;
        while got < now.len() {
            match file.read_at(&mut now[got..], (start + got) as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(TickError::Io(e)),
            }
        }
        if now[..got] != bytes[start..end] {
            return Err(TickError::Conflict);
        }
        file.write_all_at(&[byte], at as u64).map_err(TickError::Io)
    }

    pub fn read(&self, rel: &str) -> Result<Vec<u8>, ReadError> {
        self.read_note(rel, true).map(|note| note.bytes)
    }

    /// Reads a file with `open_after`'s mid-save check, and its modified time.
    pub fn read_note(&self, rel: &str, saved: bool) -> Result<crate::site::Note, ReadError> {
        let (mut file, meta) = self.stat_after(rel, saved)?;
        let unreadable = |e: io::Error| ReadError::Skip(SkipReason::Unreadable(e.kind()));
        if meta.len() > NOTE_MAX {
            return Err(ReadError::Skip(SkipReason::TooLarge));
        }
        let mut bytes = Vec::new();
        // The file can grow after the check.
        (&mut file)
            .take(NOTE_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(unreadable)?;
        if bytes.len() as u64 > NOTE_MAX {
            return Err(ReadError::Skip(SkipReason::TooLarge));
        }
        Ok(crate::site::Note {
            bytes,
            modified: meta.mtime(),
            nanos: u32::try_from(meta.mtime_nsec()).unwrap_or_default(),
        })
    }
}
