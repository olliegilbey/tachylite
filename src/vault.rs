use std::collections::HashSet;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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

fn excluded_segment(segment: &[u8]) -> bool {
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
    let mut scan = Scan {
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    walk(vault, Path::new(""), &mut scan)?;
    scan.entries.sort_by(|a, b| a.path.cmp(&b.path));

    let mut seen = HashSet::new();
    let mut kept = Vec::with_capacity(scan.entries.len());
    for entry in scan.entries {
        if entry.kind == Kind::Note {
            let stem = &entry.path[..entry.path.len() - ".md".len()];
            if !seen.insert(stem.to_string()) {
                scan.skipped.push(Skipped {
                    path: PathBuf::from(&entry.path),
                    reason: SkipReason::UrlClash,
                });
                continue;
            }
        }
        kept.push(entry);
    }
    scan.entries = kept;
    scan.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(scan)
}

fn walk(vault: &Path, rel: &Path, scan: &mut Scan) -> Result<(), Error> {
    let dir = vault.join(rel);
    let io = |source| Error::Io {
        path: dir.clone(),
        source,
    };
    for item in fs::read_dir(&dir).map_err(io)? {
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
