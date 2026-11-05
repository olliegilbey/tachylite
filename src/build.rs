use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::Error;
use crate::render::{note_title, render_note};
use crate::vault::{Kind, ReadError, Skipped, VaultDir, scan};

const MARKER: &str = ".tachylite-output";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub notes: usize,
    pub files: usize,
    pub skipped: Vec<Skipped>,
}

/// `out/pages/<note without .md>.html`.
pub fn page_path(out: &Path, note: &str) -> PathBuf {
    let stem = note
        .get(..note.len().saturating_sub(".md".len()))
        .unwrap_or(note);
    out.join("pages").join(format!("{stem}.html"))
}

/// `out/files/<file>`.
pub fn file_path(out: &Path, file: &str) -> PathBuf {
    out.join("files").join(file)
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Renders every note and copies every html page and asset into `out`. Never writes inside the vault.
pub fn build(vault: &Path, out: &Path) -> Result<Summary, Error> {
    if !vault.is_dir() {
        return Err(Error::VaultNotDir(vault.to_path_buf()));
    }
    let vault = vault.canonicalize().map_err(io_err(vault))?;
    let out = resolve_out(out)?;
    if overlaps(&vault, &out) {
        return Err(Error::Overlap { vault, out });
    }
    let dir = VaultDir::open(&vault)?;
    let scan = scan(&vault)?;
    prepare_out(&out)?;

    let mut summary = Summary {
        notes: 0,
        files: 0,
        skipped: scan.skipped,
    };
    for entry in &scan.entries {
        let skip = |reason| Skipped {
            path: PathBuf::from(&entry.path),
            reason,
        };
        match entry.kind {
            Kind::Note => match dir.read(&entry.path) {
                Ok(bytes) => {
                    let page =
                        render_note(note_title(&entry.path), &String::from_utf8_lossy(&bytes));
                    write_file(&page_path(&out, &entry.path), page.html.as_bytes())?;
                    summary.notes += 1;
                }
                Err(ReadError::Missing) => {}
                Err(ReadError::Skip(reason)) => summary.skipped.push(skip(reason)),
            },
            Kind::Html | Kind::Asset => match dir.open_file(&entry.path) {
                Ok(mut file) => {
                    copy_file(&mut file, &file_path(&out, &entry.path))?;
                    summary.files += 1;
                }
                Err(ReadError::Missing) => {}
                Err(ReadError::Skip(reason)) => summary.skipped.push(skip(reason)),
            },
        }
    }
    summary.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(summary)
}

/// Makes `out` absolute without creating it: the deepest existing ancestor is canonicalised,
/// and the missing tail must be plain names, so the overlap check sees where it really leads.
fn resolve_out(out: &Path) -> Result<PathBuf, Error> {
    let abs = std::path::absolute(out).map_err(io_err(out))?;
    let components: Vec<Component> = abs.components().collect();
    let mut split = components.len();
    while split > 0 && !components[..split].iter().collect::<PathBuf>().exists() {
        split -= 1;
    }
    let existing: PathBuf = components[..split].iter().collect();
    let mut resolved = existing.canonicalize().map_err(io_err(&existing))?;
    for component in &components[split..] {
        match component {
            Component::Normal(name) => resolved.push(name),
            _ => return Err(Error::OutNotClean(out.to_path_buf())),
        }
    }
    Ok(resolved)
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// True if either path lies inside the other: as text, or by device and inode along the
/// ancestors, which catches an alias of the vault directory such as a bind mount of it.
/// It does not see a vault mounted somewhere below the output.
fn overlaps(vault: &Path, out: &Path) -> bool {
    if out.starts_with(vault) || vault.starts_with(out) {
        return true;
    }
    let inside = |inner: &Path, outer: &Path| {
        identity(outer).is_some_and(|id| inner.ancestors().any(|a| identity(a) == Some(id)))
    };
    inside(out, vault) || inside(vault, out)
}

/// Refuses a non-empty directory without the marker; otherwise marks it and clears old output.
fn prepare_out(out: &Path) -> Result<(), Error> {
    if out.exists() {
        let mut items = fs::read_dir(out).map_err(io_err(out))?;
        if items.next().is_some() && !out.join(MARKER).is_file() {
            return Err(Error::OutNotOurs(out.to_path_buf()));
        }
    }
    fs::create_dir_all(out).map_err(io_err(out))?;
    write_file(&out.join(MARKER), b"")?;
    for sub in ["pages", "files"] {
        let dir = out.join(sub);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(io_err(&dir))?;
        }
    }
    Ok(())
}

/// Writes through a temp file and a rename, so a reader never sees half a file.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    write_via_tmp(path, |tmp| fs::write(tmp, bytes))
}

/// Streams a file into place, so a large asset is never held in memory.
fn copy_file(from: &mut fs::File, path: &Path) -> Result<(), Error> {
    write_via_tmp(path, |tmp| {
        let mut to = fs::File::create(tmp)?;
        io::copy(from, &mut to).map(|_| ())
    })
}

fn write_via_tmp(path: &Path, write: impl FnOnce(&Path) -> io::Result<()>) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    write(&tmp).map_err(io_err(&tmp))?;
    fs::rename(&tmp, path).map_err(io_err(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_found_by_identity_not_path_text() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::create_dir(dir.path().join("other")).unwrap();
        std::os::unix::fs::symlink(&vault, dir.path().join("alias")).unwrap();
        // `alias` names the vault's directory under a different path, as a bind mount or a
        // case-insensitive filesystem would.
        assert!(overlaps(&vault, &dir.path().join("alias/site")));
        assert!(overlaps(&dir.path().join("alias"), &vault.join("site")));
        assert!(overlaps(&vault.join("deep"), dir.path()));
        assert!(!overlaps(&vault, &dir.path().join("other/site")));
    }
}
