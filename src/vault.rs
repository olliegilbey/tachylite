use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};
use rustix::io::Errno;

use crate::Error;

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
    Unreadable(io::ErrorKind),
}

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

/// A file or folder name the vault never publishes.
pub fn excluded_segment(segment: &[u8]) -> bool {
    segment.starts_with(b".") || segment == b"target" || segment == b"node_modules"
}

/// The publish list and the default exclusions, applied to a vault-relative path.
pub fn classify(path: &str) -> Option<Kind> {
    if path.split('/').any(|s| excluded_segment(s.as_bytes())) {
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

/// Lists what the vault publishes. Reads only; never follows a symlink or enters an excluded folder.
pub fn scan(vault: &Path) -> Result<Scan, Error> {
    scan_under(vault, "")
}

/// Lists what the vault publishes under the folder `under` (`""` is the root). Paths stay
/// vault-relative. A subfolder that vanished before it was read lists nothing.
pub fn scan_under(vault: &Path, under: &str) -> Result<Scan, Error> {
    let mut scan = Scan {
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    walk(vault, Path::new(under), &mut scan)?;
    scan.skipped.retain(|s| {
        !(s.path == Path::new(under) && s.reason == SkipReason::Unreadable(io::ErrorKind::NotFound))
    });
    scan.entries.sort_by(|a, b| a.path.cmp(&b.path));
    scan.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(scan)
}

fn walk(vault: &Path, rel: &Path, scan: &mut Scan) -> Result<(), Error> {
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
        if excluded_segment(name.as_bytes()) {
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
        let file_type = item.file_type().map_err(io)?;
        if file_type.is_symlink() {
            scan.skipped.push(Skipped {
                path: rel_path,
                reason: SkipReason::Symlink,
            });
        } else if file_type.is_dir() {
            walk(vault, &rel_path, scan)?;
        } else if !file_type.is_file() {
            continue;
        } else if let Some(kind) = classify(rel_str) {
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
            assert_eq!(classify(path), *want, "{path}");
        }
        for ext in ["png", "jpg", "jpeg", "gif", "svg", "webp", "avif"] {
            assert_eq!(
                classify(&format!("img/x.{ext}")),
                Some(Kind::Asset),
                "{ext}"
            );
            let upper = ext.to_uppercase();
            assert_eq!(
                classify(&format!("img/x.{upper}")),
                Some(Kind::Asset),
                "{upper}"
            );
        }
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
            prop_assert_eq!(classify(&path).is_some(), !excluded && published, "{}", path);
        }
    }
}

/// Why a vault file could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Gone, or not a regular file. The scan ignores both silently, so reads do too.
    Missing,
    Skip(SkipReason),
}

/// An open vault directory. Every read resolves beneath it and never follows a symlink.
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

    /// Opens a vault-relative path for reading. A FIFO opens without blocking and reads as missing.
    pub fn open_file(&self, rel: &str) -> Result<File, ReadError> {
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
            Ok(meta) if meta.is_file() => Ok(file),
            Ok(_) => Err(ReadError::Missing),
            Err(e) => Err(ReadError::Skip(SkipReason::Unreadable(e.kind()))),
        }
    }

    pub fn read(&self, rel: &str) -> Result<Vec<u8>, ReadError> {
        let mut file = self.open_file(rel)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| ReadError::Skip(SkipReason::Unreadable(e.kind())))?;
        Ok(bytes)
    }
}
