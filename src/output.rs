//! The output directory: preparing it, and writing a site's output into it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::site::{Push, Sink, Write};
use crate::vault::{ReadError, SkipReason, Skipped, VaultDir};

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
    /// Copies made.
    pub copies: usize,
    skipped: Vec<Skipped>,
    /// The keys of the pages whose path was too long to write, so their pushes are dropped.
    unwritten: Vec<String>,
    error: Option<Error>,
    /// Whether this batch has checked that the marker is still there.
    marked: bool,
}

impl<'a, F: FnMut(Push)> Writer<'a, F> {
    pub fn new(out: &'a Path, vault: &'a VaultDir, on_push: F) -> Self {
        Writer {
            out,
            vault,
            on_push,
            copies: 0,
            skipped: Vec::new(),
            unwritten: Vec::new(),
            error: None,
            marked: false,
        }
    }

    /// The copies skipped because their source could not be read, the pages and copies
    /// skipped because their path was too long for the output, or the first error.
    pub fn finish(mut self) -> Result<Vec<Skipped>, Error> {
        if let Some(e) = self.error {
            return Err(e);
        }
        // A page written twice in one apply is skipped twice.
        self.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        self.skipped.dedup();
        Ok(self.skipped)
    }

    fn write_copy(&mut self, rel: &str, from: &str) -> Result<(), Error> {
        match self.vault.open_file(from) {
            Ok(mut file) => write_via_tmp(&self.out.join(rel), |tmp| {
                let mut to = fs::File::create(tmp)?;
                io::copy(&mut file, &mut to)?;
                // The asset's ETag is built from size and mtime, so the copy keeps the
                // source's mtime and its tag holds across restarts.
                to.set_modified(file.metadata()?.modified()?)
            })
            .map(|()| self.copies += 1),
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
        if !self.marked {
            self.marked = true;
            // A write into a deleted output would recreate it without the marker.
            if self.error.is_none() && !self.out.join(MARKER).is_file() {
                self.error = Some(Error::OutputGone(self.out.to_path_buf()));
            }
        }
        if self.error.is_some() {
            return;
        }
        let (result, skip) = match write {
            Write::Page { out: rel, html } => {
                let result =
                    write_via_tmp(&self.out.join(&rel), |tmp| fs::write(tmp, html.as_bytes()));
                (result, Some((source_of(&rel), Some(page_key(&rel)))))
            }
            Write::Copy { out: rel, from } => (self.write_copy(&rel, &from), Some((from, None))),
            Write::Manifest { json } => {
                let result = write_via_tmp(&self.out.join(crate::site::MANIFEST), |tmp| {
                    fs::write(tmp, json.as_bytes())
                });
                (result, None)
            }
        };
        match (result, skip) {
            // A name or path the output's filesystem cannot hold skips its file alone: the
            // site's 255-byte check cannot know every filesystem's limit, or the output's depth.
            (Err(Error::Io { source, .. }), Some((path, key)))
                if source.kind() == io::ErrorKind::InvalidFilename =>
            {
                self.skipped.push(Skipped {
                    path: PathBuf::from(path),
                    reason: SkipReason::NameTooLong,
                });
                self.unwritten.extend(key);
            }
            (result, _) => self.error = result.err(),
        }
    }

    fn push(&mut self, push: Push) {
        if let Push::Page { key, .. } = &push
            && let Some(at) = self.unwritten.iter().position(|k| k == key)
        {
            self.unwritten.swap_remove(at);
            return;
        }
        if self.error.is_none() {
            (self.on_push)(push);
        }
    }

    fn page(&mut self, rel: &str) -> Option<String> {
        if self.error.is_some() {
            return None;
        }
        fs::read_to_string(self.out.join(rel)).ok()
    }

    fn keeps(&self) -> bool {
        true
    }
}

/// The push key of the page at `rel`, an output path under `pages/`.
fn page_key(rel: &str) -> String {
    if rel == "pages/.tachylite/home.html" {
        return String::new();
    }
    let key = rel.strip_prefix("pages/").unwrap_or(rel);
    let key = key
        .strip_suffix("/.index.html")
        .or_else(|| key.strip_suffix(".html"))
        .unwrap_or(key);
    key.to_string()
}

/// The vault path behind the page at `rel`: a note's path, or a generated page's output path.
fn source_of(rel: &str) -> String {
    if crate::site::generated(rel) {
        return rel.to_string();
    }
    format!("{}.md", page_key(rel))
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

/// Writes `.tmp` beside the target, then renames it, so a reader never sees half a file and
/// no URL can reach the temp file. One short name fits beside any target name, and one name
/// per folder is enough: a writer writes one file at a time.
fn write_via_tmp(path: &Path, write: impl FnOnce(&Path) -> io::Result<()>) -> Result<(), Error> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(io_err(parent))?;
    let tmp = parent.join(".tmp");
    write(&tmp).map_err(io_err(&tmp))?;
    fs::rename(&tmp, path).map_err(|e| {
        // A name the filesystem refuses fails here, after the temp file is written.
        let _ = fs::remove_file(&tmp);
        io_err(path)(e)
    })
}
