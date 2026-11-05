//! The pure core: vault changes in, output out. A full build is `Sync` of the root.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Exclude;
use crate::front;
use crate::links::{Index, key};
use crate::render::{Broken, note_title, render_body, render_page, url};
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

impl Source for &VaultDir {
    fn note(&mut self, path: &str) -> Result<Vec<u8>, ReadError> {
        self.read(path)
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.open_file(path).map(|_| ())
    }
}

/// Where `Site::apply_to` sends output, as it makes it: deletes first, then gone pushes,
/// then writes, each page's write followed by its push. Paths are relative to the output
/// directory. Streaming means a full sync never holds every page at once.
pub trait Sink {
    fn delete(&mut self, out: String);
    fn write(&mut self, write: Write);
    fn push(&mut self, push: Push);
}

/// What an apply found that is not output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub skipped: Vec<Skipped>,
    /// The broken links of every note rendered for its own change or a link target's.
    pub broken: Vec<Broken>,
}

/// All of an apply's output and its report, collected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delta {
    pub deletes: Vec<String>,
    pub writes: Vec<Write>,
    pub pushes: Vec<Push>,
    pub skipped: Vec<Skipped>,
    pub broken: Vec<Broken>,
}

impl Sink for Delta {
    fn delete(&mut self, out: String) {
        self.deletes.push(out);
    }
    fn write(&mut self, write: Write) {
        self.writes.push(write);
    }
    fn push(&mut self, push: Push) {
        self.pushes.push(push);
    }
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
    /// The winners, for link resolution. The graph shares its paths.
    index: Index,
    /// What each rendered note links to.
    links: HashMap<Arc<str>, Links>,
    /// The notes that link to each path. A note that links to itself is listed too, so a
    /// path's linkers are found in one place; its page leaves itself out.
    inbound: HashMap<Arc<str>, BTreeSet<Arc<str>>>,
    /// The notes with an unresolved link, by the link's key.
    unresolved: HashMap<String, BTreeSet<Arc<str>>>,
    /// Frontmatter titles, for the notes that set one.
    titles: HashMap<String, String>,
}

/// One note's outgoing links.
#[derive(Debug, Default)]
struct Links {
    /// The published paths its links resolve to, sorted.
    targets: Vec<Arc<str>>,
    /// The keys of its broken and ambiguous links.
    unresolved: Vec<String>,
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

    /// Takes effect at the next `Sync` of the root, which re-lists every file.
    pub fn set_exclude(&mut self, exclude: Exclude) {
        self.exclude = exclude;
    }

    pub fn exclude(&self) -> &Exclude {
        &self.exclude
    }

    /// Applies changes and collects the output.
    pub fn apply(&mut self, changes: Vec<Change>, source: &mut impl Source) -> Delta {
        let mut delta = Delta::default();
        let report = self.apply_to(changes, source, &mut delta);
        delta.skipped = report.skipped;
        delta.broken = report.broken;
        delta
    }

    /// Applies changes and sends the output to `sink` as it is made.
    pub fn apply_to(
        &mut self,
        changes: Vec<Change>,
        source: &mut impl Source,
        sink: &mut impl Sink,
    ) -> Report {
        let mut report = Report::default();
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
            if self.probe(path, source, &mut fresh, &mut report.skipped) {
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
                self.probe(path, source, &mut fresh, &mut report.skipped);
            }
        };

        for path in &lost {
            if touched.contains(path) || self.winners.contains(path) {
                report.skipped.push(Skipped {
                    path: PathBuf::from(path),
                    reason: SkipReason::UrlClash,
                });
            }
        }

        let mut bodies = self.link(&won, &mut fresh, source, &mut report.broken);
        drop(fresh);

        // A new note always pushes, so its key stays live and gets no gone. No other
        // winner can hold a key a removed winner held.
        let live: HashSet<&str> = won
            .iter()
            .filter(|p| !self.winners.contains(*p) && self.entries.get(*p) == Some(&Kind::Note))
            .map(|p| note_key(p))
            .collect();
        let mut gone = Vec::new();
        for path in self.winners.difference(&won) {
            // Its kind by extension: the path may be excluded now.
            let kind = classify(path, &Exclude::default()).unwrap_or(Kind::Asset);
            let (_, out) = claim(path, kind);
            sink.delete(out);
            if kind == Kind::Note {
                self.versions.remove(path);
                if !live.contains(note_key(path)) {
                    gone.push(note_key(path).to_string());
                }
            }
        }
        for key in gone {
            sink.push(Push::Gone { key });
        }

        for path in &won {
            let kind = self.entries.get(path).copied().unwrap_or(Kind::Asset);
            let (_, out) = claim(path, kind);
            let new = !self.winners.contains(path);
            match kind {
                Kind::Note => {
                    let Some(body) = bodies.remove(path) else {
                        continue;
                    };
                    let backlinks: Vec<(String, String)> = self
                        .inbound
                        .get(path.as_str())
                        .into_iter()
                        .flatten()
                        .filter(|from| ***from != **path)
                        .map(|from| (self.title(from).to_string(), url(from)))
                        .collect();
                    let page = render_page(self.title(path), &body, &backlinks);
                    drop(body);
                    if !new && self.versions.get(path) == Some(&page.version) {
                        continue;
                    }
                    self.versions.insert(path.clone(), page.version.clone());
                    let html: Arc<str> = Arc::from(page.html);
                    sink.write(Write::Page {
                        out,
                        html: Arc::clone(&html),
                    });
                    sink.push(Push::Page {
                        key: note_key(path).to_string(),
                        version: page.version,
                        html,
                    });
                }
                Kind::Html | Kind::Asset => {
                    if new || touched.contains(path) {
                        sink.write(Write::Copy {
                            out,
                            from: path.clone(),
                        });
                    }
                }
            }
        }

        self.winners = won;
        report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        report
    }

    /// Updates the link graph for the new winners and renders the body of every winning
    /// note whose page may change: notes read this apply, notes whose links may now resolve
    /// differently, and notes whose backlinks changed. Returns the bodies by path.
    fn link(
        &mut self,
        won: &BTreeSet<String>,
        fresh: &mut HashMap<String, Vec<u8>>,
        source: &mut impl Source,
        broken: &mut Vec<Broken>,
    ) -> HashMap<String, String> {
        let removed: Vec<String> = self.winners.difference(won).cloned().collect();
        let added: Vec<String> = won.difference(&self.winners).cloned().collect();

        // A retitled note changes the backlink text on every page it links to.
        let mut changed = BTreeSet::new();
        for (path, bytes) in fresh.iter().filter(|(p, _)| won.contains(*p)) {
            let meta = front::meta(&String::from_utf8_lossy(bytes));
            if self.titles.get(path) == meta.title.as_ref() {
                continue;
            }
            match meta.title {
                Some(title) => self.titles.insert(path.clone(), title),
                None => self.titles.remove(path),
            };
            if let Some(links) = self.links.get(path.as_str()) {
                changed.extend(links.targets.iter().cloned());
            }
        }
        for path in &removed {
            self.titles.remove(path);
        }

        // A link can resolve differently once a path with its key appears or vanishes. Its
        // note links to a path in that key's bucket now, or the link is unresolved.
        let mut first: BTreeSet<String> =
            fresh.keys().filter(|p| won.contains(*p)).cloned().collect();
        let keys: HashSet<String> = removed.iter().chain(&added).map(|p| key(p)).collect();
        for k in &keys {
            for path in self.index.bucket(k) {
                first.extend(
                    self.inbound
                        .get(path)
                        .into_iter()
                        .flatten()
                        .map(|n| n.to_string()),
                );
            }
            first.extend(
                self.unresolved
                    .get(k)
                    .into_iter()
                    .flatten()
                    .map(|n| n.to_string()),
            );
        }
        first.retain(|p| won.contains(p));

        for path in &removed {
            if let Some(note) = self.index.get(path) {
                self.relink(&note, Links::default(), &mut changed);
                self.links.remove(path.as_str());
            }
            self.index.remove(path);
        }
        for path in &added {
            self.index.insert(path);
        }

        let mut bodies = HashMap::new();
        for path in first {
            // A dependent that cannot be read keeps its page; its own event follows. Each
            // source is dropped once rendered, so a full sync never holds both.
            let Some(bytes) = fresh.remove(&path).or_else(|| source.note(&path).ok()) else {
                continue;
            };
            let body = render_body(&path, &String::from_utf8_lossy(&bytes), &self.index);
            drop(bytes);
            let links = Links {
                targets: body.targets.into_iter().collect(),
                unresolved: body.broken.iter().map(|b| key(&b.target)).collect(),
            };
            broken.extend(body.broken);
            if let Some(note) = self.index.get(&path) {
                self.relink(&note, links, &mut changed);
            }
            bodies.insert(path, body.html);
        }

        // Their links resolve as before, so the graph stands.
        for path in changed {
            let path = path.to_string();
            let note = self.entries.get(&path) == Some(&Kind::Note);
            if !note || !won.contains(&path) || bodies.contains_key(&path) {
                continue;
            }
            if let Ok(bytes) = source.note(&path) {
                let body = render_body(&path, &String::from_utf8_lossy(&bytes), &self.index);
                bodies.insert(path, body.html);
            }
        }
        bodies
    }

    /// A note's frontmatter title, else its file stem.
    fn title<'a>(&'a self, path: &'a str) -> &'a str {
        self.titles
            .get(path)
            .map_or_else(|| note_title(path), String::as_str)
    }

    /// Replaces a note's links, and adds each path whose linkers changed to `changed`.
    fn relink(&mut self, note: &Arc<str>, links: Links, changed: &mut BTreeSet<Arc<str>>) {
        let old = self.links.remove(note).unwrap_or_default();
        for k in &old.unresolved {
            if let Some(notes) = self.unresolved.get_mut(k) {
                notes.remove(note);
                if notes.is_empty() {
                    self.unresolved.remove(k);
                }
            }
        }
        for k in &links.unresolved {
            self.unresolved
                .entry(k.clone())
                .or_default()
                .insert(Arc::clone(note));
        }
        for target in old
            .targets
            .iter()
            .filter(|t| links.targets.binary_search(t).is_err())
        {
            if let Some(from) = self.inbound.get_mut(target) {
                from.remove(note);
                if from.is_empty() {
                    self.inbound.remove(target);
                }
            }
            changed.insert(Arc::clone(target));
        }
        for target in links
            .targets
            .iter()
            .filter(|t| old.targets.binary_search(t).is_err())
        {
            self.inbound
                .entry(Arc::clone(target))
                .or_default()
                .insert(Arc::clone(note));
            changed.insert(Arc::clone(target));
        }
        self.links.insert(Arc::clone(note), links);
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
            // Excluded since it was listed, or never publishable.
            self.entries.remove(path);
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
