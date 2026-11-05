//! The output directory: preparing it, and applying a site delta to it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::site::{Delta, Write};
use crate::vault::{ReadError, Skipped, VaultDir};

pub const MARKER: &str = ".tachylite-output";

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Refuses a non-empty directory without the marker; otherwise marks it and clears old output.
pub fn prepare(out: &Path) -> Result<(), Error> {
    if out.exists() {
        let mut items = fs::read_dir(out).map_err(io_err(out))?;
        if items.next().is_some() && !out.join(MARKER).is_file() {
            return Err(Error::OutNotOurs(out.to_path_buf()));
        }
    }
    fs::create_dir_all(out).map_err(io_err(out))?;
    write_via_tmp(&out.join(MARKER), |tmp| fs::write(tmp, b""))?;
    for sub in ["pages", "files"] {
        let dir = out.join(sub);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(io_err(&dir))?;
        }
    }
    Ok(())
}

/// Applies deletes, then writes. A copy whose source vanished is skipped; the watcher
/// reports the removal next.
pub fn apply(out: &Path, delta: &Delta, vault: &VaultDir) -> Result<Vec<Skipped>, Error> {
    for rel in &delta.deletes {
        remove(out, rel)?;
    }
    let mut skipped = Vec::new();
    for write in &delta.writes {
        match write {
            Write::Page { out: rel, html } => {
                write_via_tmp(&out.join(rel), |tmp| fs::write(tmp, html.as_bytes()))?;
            }
            Write::Copy { out: rel, from } => match vault.open_file(from) {
                Ok(mut file) => write_via_tmp(&out.join(rel), |tmp| {
                    let mut to = fs::File::create(tmp)?;
                    io::copy(&mut file, &mut to).map(|_| ())
                })?,
                Err(ReadError::Missing | ReadError::MidSave) => {}
                Err(ReadError::Skip(reason)) => skipped.push(Skipped {
                    path: PathBuf::from(from),
                    reason,
                }),
            },
        }
    }
    Ok(skipped)
}

/// Removes one output file, then any parent folders it left empty, up to `pages/` or `files/`.
fn remove(out: &Path, rel: &str) -> Result<(), Error> {
    let path = out.join(rel);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(&path)(e)),
    }
    for (i, _) in rel.rmatch_indices('/') {
        let parent = &rel[..i];
        if !parent.contains('/') {
            break;
        }
        let dir = out.join(parent);
        match fs::remove_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// Writes `.<name>.tmp` beside the target, then renames it, so a reader never sees half a
/// file and no URL can reach the temp file.
fn write_via_tmp(path: &Path, write: impl FnOnce(&Path) -> io::Result<()>) -> Result<(), Error> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(io_err(parent))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = parent.join(format!(".{name}.tmp"));
    write(&tmp).map_err(io_err(&tmp))?;
    fs::rename(&tmp, path).map_err(io_err(path))
}
