//! The output directory: preparing it, and writing a site's output into it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::site::{Push, Sink, Write};
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

/// A sink that writes a site's output into `out` as the site makes it, and hands each
/// push to `on_push` once its page is on disk. After the first error it does nothing more.
pub struct Writer<'a, F: FnMut(Push)> {
    out: &'a Path,
    vault: &'a VaultDir,
    on_push: F,
    /// Note pages written; generated index pages are not counted.
    pub pages: usize,
    /// Copies made or skipped.
    pub copies: usize,
    skipped: Vec<Skipped>,
    error: Option<Error>,
}

impl<'a, F: FnMut(Push)> Writer<'a, F> {
    pub fn new(out: &'a Path, vault: &'a VaultDir, on_push: F) -> Self {
        Writer {
            out,
            vault,
            on_push,
            pages: 0,
            copies: 0,
            skipped: Vec::new(),
            error: None,
        }
    }

    /// The copies skipped because their source could not be read, or the first error.
    pub fn finish(self) -> Result<Vec<Skipped>, Error> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(self.skipped),
        }
    }

    fn write_copy(&mut self, rel: &str, from: &str) -> Result<(), Error> {
        self.copies += 1;
        match self.vault.open_file(from) {
            Ok(mut file) => write_via_tmp(&self.out.join(rel), |tmp| {
                let mut to = fs::File::create(tmp)?;
                io::copy(&mut file, &mut to).map(|_| ())
            }),
            // The source vanished; the watcher reports the removal next.
            Err(ReadError::Missing | ReadError::MidSave) => Ok(()),
            Err(ReadError::Skip(reason)) => {
                self.skipped.push(Skipped {
                    path: PathBuf::from(from),
                    reason,
                });
                Ok(())
            }
        }
    }
}

impl<F: FnMut(Push)> Sink for Writer<'_, F> {
    fn delete(&mut self, rel: String) {
        if self.error.is_none() {
            self.error = remove(self.out, &rel).err();
        }
    }

    fn write(&mut self, write: Write) {
        if self.error.is_some() {
            return;
        }
        let result = match write {
            Write::Page { out: rel, html } => {
                self.pages += usize::from(!crate::site::generated(&rel));
                write_via_tmp(&self.out.join(rel), |tmp| fs::write(tmp, html.as_bytes()))
            }
            Write::Copy { out: rel, from } => self.write_copy(&rel, &from),
            Write::Manifest { json } => {
                write_via_tmp(&self.out.join(crate::site::MANIFEST), |tmp| {
                    fs::write(tmp, json.as_bytes())
                })
            }
        };
        self.error = result.err();
    }

    fn push(&mut self, push: Push) {
        if self.error.is_none() {
            (self.on_push)(push);
        }
    }
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
