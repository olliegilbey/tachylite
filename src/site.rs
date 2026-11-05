//! The pure core: vault changes in, an output delta out. A full build is `Sync` of the root.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Exclude;
use crate::render::{note_title, render_note};
use crate::vault::{Entry, Kind, ReadError, SkipReason, Skipped, VaultDir, classify};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The path may have changed. The site classifies it and reads its current state.
    Touch(String),
    /// `entries` are every publishable file under the folder `under` (`""` is the vault root).
    Sync { under: String, entries: Vec<Entry> },
}

/// Where the site reads the vault. `VaultDir` in production; a map in tests.
pub trait Source {
    fn note(&mut self, path: &str) -> Result<Vec<u8>, ReadError>;
    /// Checks that a regular file is there, without reading it.
    fn file(&mut self, path: &str) -> Result<(), ReadError>;
}

impl Source for VaultDir {
    fn note(&mut self, path: &str) -> Result<Vec<u8>, ReadError> {
        self.read(path)
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.open_file(path).map(|_| ())
    }
}

/// Output changes, with paths relative to the output directory. Apply deletes before writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delta {
    pub deletes: Vec<String>,
    pub writes: Vec<Write>,
    pub pushes: Vec<Push>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    Page { out: String, html: Arc<str> },
    Copy { out: String, from: String },
}

/// `key` is the note path without `.md`: the decoded URL path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Push {
    Page {
        key: String,
        version: String,
        html: Arc<str>,
    },
    Gone {
        key: String,
    },
}

#[derive(Debug, Default)]
pub struct Site {
    exclude: Exclude,
    /// Every readable, publishable file, winner or not.
    entries: BTreeMap<String, Kind>,
    /// Vault paths whose output is written.
    winners: BTreeSet<String>,
    /// The version of each winning note's written page.
    versions: HashMap<String, String>,
}

fn under(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn note_key(path: &str) -> &str {
    path.get(..path.len().saturating_sub(".md".len()))
        .unwrap_or(path)
}

/// The URL key and output path a file claims.
fn claim(path: &str, kind: Kind) -> (String, String) {
    match kind {
        Kind::Note => {
            let key = note_key(path);
            (key.to_string(), format!("pages/{key}.html"))
        }
        Kind::Html | Kind::Asset => (path.to_string(), format!("files/{path}")),
    }
}

fn parents(out: &str) -> impl Iterator<Item = &str> {
    out.match_indices('/').map(move |(i, _)| &out[..i])
}

/// The first claim in byte order wins. A claim loses if its URL is taken, if its output
/// path is a folder of a claimed output, or if one of its folders is a claimed output.
fn winners(entries: &BTreeMap<String, Kind>) -> (BTreeSet<String>, Vec<String>) {
    let mut urls = HashSet::new();
    let mut files: HashSet<String> = HashSet::new();
    let mut dirs: HashSet<String> = HashSet::new();
    let mut won = BTreeSet::new();
    let mut lost = Vec::new();
    for (path, &kind) in entries {
        let (url, out) = claim(path, kind);
        let clash =
            urls.contains(&url) || dirs.contains(&out) || parents(&out).any(|p| files.contains(p));
        if clash {
            lost.push(path.clone());
            continue;
        }
        dirs.extend(parents(&out).map(str::to_string));
        files.insert(out);
        urls.insert(url);
        won.insert(path.clone());
    }
    (won, lost)
}

impl Site {
    pub fn new(exclude: Exclude) -> Site {
        Site {
            exclude,
            ..Site::default()
        }
    }

    pub fn exclude(&self) -> &Exclude {
        &self.exclude
    }

    pub fn apply(&mut self, changes: Vec<Change>, source: &mut impl Source) -> Delta {
        let mut delta = Delta::default();
        let mut touched = BTreeSet::new();
        for change in changes {
            match change {
                Change::Touch(path) => {
                    if classify(&path, &self.exclude).is_some() {
                        touched.insert(path);
                    }
                }
                Change::Sync {
                    under: folder,
                    entries,
                } => {
                    let listed: HashSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
                    let gone: Vec<String> = self
                        .entries
                        .keys()
                        .filter(|p| under(p, &folder) && !listed.contains(p.as_str()))
                        .cloned()
                        .collect();
                    touched.extend(gone);
                    touched.extend(entries.into_iter().map(|e| e.path));
                }
            }
        }

        let mut fresh: HashMap<String, Vec<u8>> = HashMap::new();
        let mut mid_save = Vec::new();
        for path in &touched {
            if self.probe(path, source, &mut fresh, &mut delta.skipped) {
                mid_save.push(path.clone());
            }
        }
        for path in &mid_save {
            touched.remove(path);
        }

        // A loser promoted by this change was not read yet. Read it; if that fails, it
        // leaves the entries and the winners are recomputed.
        let (won, lost) = loop {
            let (won, lost) = winners(&self.entries);
            let unread: Vec<String> = won
                .iter()
                .filter(|p| {
                    self.entries.get(*p) == Some(&Kind::Note)
                        && !self.winners.contains(*p)
                        && !fresh.contains_key(*p)
                })
                .cloned()
                .collect();
            if unread.is_empty() {
                break (won, lost);
            }
            for path in &unread {
                self.probe(path, source, &mut fresh, &mut delta.skipped);
            }
        };

        for path in &lost {
            if touched.contains(path) || self.winners.contains(path) {
                delta.skipped.push(Skipped {
                    path: PathBuf::from(path),
                    reason: SkipReason::UrlClash,
                });
            }
        }

        let mut gone_keys = Vec::new();
        for path in self.winners.difference(&won) {
            let kind = classify(path, &self.exclude).unwrap_or(Kind::Asset);
            let (_, out) = claim(path, kind);
            delta.deletes.push(out);
            if kind == Kind::Note {
                self.versions.remove(path);
                gone_keys.push(note_key(path).to_string());
            }
        }

        for path in &won {
            let kind = self.entries.get(path).copied().unwrap_or(Kind::Asset);
            let (_, out) = claim(path, kind);
            let new = !self.winners.contains(path);
            match kind {
                Kind::Note => {
                    // Each source is dropped once rendered, so a full sync never holds both.
                    let Some(bytes) = fresh.remove(path) else {
                        continue;
                    };
                    let page = render_note(note_title(path), &String::from_utf8_lossy(&bytes));
                    drop(bytes);
                    if !new && self.versions.get(path) == Some(&page.version) {
                        continue;
                    }
                    self.versions.insert(path.clone(), page.version.clone());
                    let html: Arc<str> = Arc::from(page.html);
                    delta.pushes.push(Push::Page {
                        key: note_key(path).to_string(),
                        version: page.version,
                        html: Arc::clone(&html),
                    });
                    delta.writes.push(Write::Page { out, html });
                }
                Kind::Html | Kind::Asset => {
                    if new || touched.contains(path) {
                        delta.writes.push(Write::Copy {
                            out,
                            from: path.clone(),
                        });
                    }
                }
            }
        }

        let live: HashSet<&str> = delta
            .pushes
            .iter()
            .filter_map(|p| match p {
                Push::Page { key, .. } => Some(key.as_str()),
                Push::Gone { .. } => None,
            })
            .collect();
        let gone: Vec<Push> = gone_keys
            .into_iter()
            .filter(|k| !live.contains(k.as_str()))
            .map(|key| Push::Gone { key })
            .collect();
        delta.pushes.splice(0..0, gone);

        self.winners = won;
        delta.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        delta
    }

    /// Reads a path's current state into `entries`, and a note's bytes into `fresh`.
    /// Returns true for a published path that is mid-save, which keeps its page as it is.
    fn probe(
        &mut self,
        path: &str,
        source: &mut impl Source,
        fresh: &mut HashMap<String, Vec<u8>>,
        skipped: &mut Vec<Skipped>,
    ) -> bool {
        let Some(kind) = classify(path, &self.exclude) else {
            return false;
        };
        let result = match kind {
            Kind::Note => source.note(path).map(|bytes| {
                fresh.insert(path.to_string(), bytes);
            }),
            Kind::Html | Kind::Asset => source.file(path),
        };
        match result {
            Ok(()) => {
                self.entries.insert(path.to_string(), kind);
            }
            // An unpublished path has nothing to keep; its save adds it.
            Err(ReadError::MidSave) if self.winners.contains(path) => return true,
            Err(ReadError::Missing | ReadError::MidSave) => {
                self.entries.remove(path);
            }
            Err(ReadError::Skip(reason)) => {
                self.entries.remove(path);
                skipped.push(Skipped {
                    path: PathBuf::from(path),
                    reason,
                });
            }
        }
        false
    }
}
