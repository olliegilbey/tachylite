//! The pure core: vault changes in, output out. A full build is `Sync` of the root.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Exclude;
use crate::front;
use crate::indexes::{self, Folder, Item};
use crate::links::{Index, key};
use crate::render::{
    Body, Broken, EMBED_DEPTH, Sources, embed_targets, encode, note_title, render_body,
    render_page, url,
};
use crate::vault::{Entry, Kind, ReadError, SkipReason, Skipped, VaultDir, classify};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The path may have changed. The site classifies it and reads its current state.
    Touch(String),
    /// `entries` are every publishable file under the folder `under` (`""` is the vault root).
    Sync { under: String, entries: Vec<Entry> },
}

/// A note as read: its bytes, and its modified time in seconds since the epoch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    pub bytes: Vec<u8>,
    pub modified: i64,
}

/// A note with no known modified time, as in-memory sources give.
impl From<Vec<u8>> for Note {
    fn from(bytes: Vec<u8>) -> Note {
        Note { bytes, modified: 0 }
    }
}

/// Where the site reads the vault. `VaultDir` in production; a map in tests.
pub trait Source {
    fn note(&mut self, path: &str) -> Result<Note, ReadError>;
    /// Checks that a regular file is there, without reading it.
    fn file(&mut self, path: &str) -> Result<(), ReadError>;
}

impl Source for &VaultDir {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        self.read_note(path, true)
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.open_file(path).map(|_| ())
    }
}

/// Where `Site::apply_to` sends output, as it makes it. Removed vault files come first: their
/// deletes, then their gone pushes. Then the vault files' writes, then the index pages'
/// deletes and gone pushes, then their writes, then the manifest last. Each page's write is
/// followed by its push. Paths are relative to the output directory. Streaming means a full
/// sync never holds every page at once.
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

/// The site's name, until config names it.
const SITE: &str = "Tachylite";

/// The switcher list's URL key.
const NOTES_JSON: &str = ".tachylite/notes.json";

/// Whether an output path holds a generated page rather than a vault file's.
pub fn generated(out: &str) -> bool {
    out.starts_with("pages/.tachylite/") || out.starts_with("files/.tachylite/")
}

/// Where a generated page's key is written in the output. A folder index is a dot name
/// inside its folder's path, so no folder name can clash with it.
fn generated_out(key: &str) -> String {
    match key {
        "" => "pages/.tachylite/home.html".into(),
        NOTES_JSON => format!("files/{key}"),
        _ if key.starts_with(FOLDERS) => format!("pages/{key}/.index.html"),
        _ => format!("pages/{key}.html"),
    }
}

/// The URL key prefix of the folder indexes.
const FOLDERS: &str = ".tachylite/folders/";

/// The longest file name the output filesystem takes, in bytes.
const NAME_MAX: usize = 255;

/// Where the manifest lives in the output: `/.tachylite/manifest.json`.
pub const MANIFEST: &str = "files/.tachylite/manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    Page {
        out: String,
        html: Arc<str>,
    },
    Copy {
        out: String,
        from: String,
    },
    /// Every page's URL and version, for client sync. It follows an apply's page writes.
    Manifest {
        json: String,
    },
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
    /// Sorted, so the manifest is written in one pass.
    versions: BTreeMap<String, String>,
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
    /// The notes whose embeds read each path, at any depth.
    embedded_by: HashMap<Arc<str>, BTreeSet<Arc<str>>>,
    /// The version of each index page and of the switcher list, by URL key.
    generated: BTreeMap<String, String>,
    /// Each read note's modified time, for the recent pages.
    modified: HashMap<String, i64>,
    /// Whether a manifest was written, so the first apply writes one even when empty.
    listed: bool,
}

/// One note's outgoing links.
#[derive(Debug, Default)]
struct Links {
    /// The published paths its links resolve to, sorted.
    targets: Vec<Arc<str>>,
    /// The keys of its broken and ambiguous links.
    unresolved: Vec<String>,
    /// The notes its embeds read, at any depth, sorted.
    embeds: Vec<Arc<str>>,
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

/// The first claim in byte order wins. A claim loses if its output file name is too long,
/// if its URL is taken, if its output path is a folder of a claimed output, or if one of
/// its folders is a claimed output.
fn winners(entries: &BTreeMap<String, Kind>) -> (BTreeSet<String>, Vec<(String, SkipReason)>) {
    let mut urls = HashSet::new();
    let mut files: HashSet<String> = HashSet::new();
    let mut dirs: HashSet<String> = HashSet::new();
    let mut won = BTreeSet::new();
    let mut lost = Vec::new();
    for (path, &kind) in entries {
        let (url, out) = claim(path, kind);
        // A note's page name is 2 bytes longer than the note's.
        if out.rsplit('/').next().unwrap_or(&out).len() > NAME_MAX {
            lost.push((path.clone(), SkipReason::NameTooLong));
            continue;
        }
        let clash =
            urls.contains(&url) || dirs.contains(&out) || parents(&out).any(|p| files.contains(p));
        if clash {
            lost.push((path.clone(), SkipReason::UrlClash));
            continue;
        }
        dirs.extend(parents(&out).map(str::to_string));
        files.insert(out);
        urls.insert(url);
        won.insert(path.clone());
    }
    (won, lost)
}

/// Renders each note's body, on as many threads as the batch is worth. A save of a few notes
/// stays on this thread, so the push path pays no spawn.
fn render_all<'p>(
    notes: &[(&'p str, Vec<u8>)],
    index: &Index,
    sources: &Sources,
) -> Vec<(&'p str, Body)> {
    let render = |notes: &[(&'p str, Vec<u8>)]| -> Vec<(&'p str, Body)> {
        notes
            .iter()
            .map(|(path, bytes)| {
                (
                    *path,
                    render_body(path, &String::from_utf8_lossy(bytes), index, sources),
                )
            })
            .collect()
    };
    if notes.len() <= 16 {
        return render(notes);
    }
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let threads = cores.min(notes.len().div_ceil(16));
    if threads <= 1 {
        return render(notes);
    }
    let size = notes.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = notes
            .chunks(size)
            .map(|part| scope.spawn(move || render(part)))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            // Copied on this thread, so a worker's heap holds no page once its batch is done.
            .map(|(path, body)| {
                let html = body.html.as_str().to_owned();
                (path, Body { html, ..body })
            })
            .collect()
    })
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

        for (path, reason) in lost {
            if touched.contains(&path) || self.winners.contains(&path) {
                report.skipped.push(Skipped {
                    path: PathBuf::from(path),
                    reason,
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
        let mut listing = !self.listed;
        let mut gone = Vec::new();
        for path in self.winners.difference(&won) {
            // Its kind by extension: the path may be excluded now.
            let kind = classify(path, &Exclude::default()).unwrap_or(Kind::Asset);
            let (_, out) = claim(path, kind);
            sink.delete(out);
            if kind == Kind::Note {
                listing |= self.versions.remove(path).is_some();
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
                    let page = render_page(path, self.title(path), &body, &backlinks);
                    drop(body);
                    if !new && self.versions.get(path) == Some(&page.version) {
                        continue;
                    }
                    self.versions.insert(path.clone(), page.version.clone());
                    listing = true;
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

        self.modified.retain(|path, _| won.contains(path));
        listing |= self.indexes(&won, sink);
        if listing {
            sink.write(Write::Manifest {
                json: self.manifest(),
            });
            self.listed = true;
        }

        self.winners = won;
        report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        report
    }

    /// Derives every index page and the switcher list from the winners, and writes and
    /// pushes the ones whose version changed. Returns whether any changed.
    fn indexes(&mut self, won: &BTreeSet<String>, sink: &mut impl Sink) -> bool {
        let mut built: Vec<(String, String, String)> = Vec::new();
        {
            // Every folder that holds a page, with its subfolders and pages.
            let mut tree: BTreeMap<&str, (BTreeSet<&str>, Vec<Item>)> = BTreeMap::new();
            tree.entry("").or_default();
            let mut notes: Vec<(i64, &str)> = Vec::new();
            let mut json = indexes::NotesJson::default();
            for path in won {
                let kind = self.entries.get(path).copied().unwrap_or(Kind::Asset);
                if kind == Kind::Asset {
                    continue;
                }
                let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
                let mut child = folder;
                while !child.is_empty() {
                    let parent = child.rsplit_once('/').map_or("", |(p, _)| p);
                    let (subs, _) = tree.entry(parent).or_default();
                    if !subs.insert(child) {
                        break;
                    }
                    child = parent;
                }
                let modified = self.modified.get(path).copied().unwrap_or_default();
                let title = if kind == Kind::Note {
                    self.title(path)
                } else {
                    name
                };
                let url = url(path);
                if kind == Kind::Note {
                    notes.push((modified, path));
                    json.push(title, &url, note_key(path), self.index.aliases(path));
                }
                tree.entry(folder).or_default().1.push(Item {
                    title,
                    url,
                    folder,
                    modified,
                });
            }
            // Newest first; equal times by path.
            let order = |a: &(i64, &str), b: &(i64, &str)| b.0.cmp(&a.0).then(a.1.cmp(b.1));
            if notes.len() > indexes::RECENT {
                notes.select_nth_unstable_by(indexes::RECENT, order);
                notes.truncate(indexes::RECENT);
            }
            notes.sort_unstable_by(order);
            let recent: Vec<Item> = notes
                .iter()
                .map(|&(modified, path)| Item {
                    title: self.title(path),
                    url: url(path),
                    folder: path.rsplit_once('/').map_or("", |(f, _)| f),
                    modified,
                })
                .collect();
            let recent: Vec<&Item> = recent.iter().collect();
            for (path, (subs, mut items)) in tree {
                items.sort_by_cached_key(|i| (indexes::order(i.title), i.url.clone()));
                let folder = Folder {
                    path,
                    folders: {
                        let mut folders: Vec<&str> = subs.into_iter().collect();
                        folders.sort_by_cached_key(|f| {
                            (indexes::order(f.rsplit('/').next().unwrap_or(f)), *f)
                        });
                        folders
                    },
                    items,
                };
                let (key, page) = if path.is_empty() {
                    (String::new(), indexes::home(SITE, &recent, &folder))
                } else {
                    (format!("{FOLDERS}{path}"), indexes::folder(&folder))
                };
                built.push((key, page.version, page.html));
            }
            let page = indexes::recent(&recent);
            built.push((".tachylite/recent".into(), page.version, page.html));
            let json = json.finish();
            let mut hasher = DefaultHasher::new();
            json.hash(&mut hasher);
            built.push((NOTES_JSON.into(), format!("{:016x}", hasher.finish()), json));
        }

        let mut changed = false;
        let fresh: HashSet<String> = built.iter().map(|(key, ..)| key.clone()).collect();
        let gone: Vec<String> = self
            .generated
            .keys()
            .filter(|key| !fresh.contains(*key))
            .cloned()
            .collect();
        for key in gone {
            self.generated.remove(&key);
            sink.delete(generated_out(&key));
            sink.push(Push::Gone { key });
            changed = true;
        }
        for (key, version, html) in built {
            if self.generated.get(&key) == Some(&version) {
                continue;
            }
            let html: Arc<str> = Arc::from(html);
            sink.write(Write::Page {
                out: generated_out(&key),
                html: Arc::clone(&html),
            });
            if key != NOTES_JSON {
                sink.push(Push::Page {
                    key: key.clone(),
                    version: version.clone(),
                    html,
                });
            }
            self.generated.insert(key, version);
            changed = true;
        }
        changed
    }

    /// `{"pages":{"<url>":"<version>",...}}`: note pages in path order, then generated
    /// pages in key order. URLs are percent-encoded and
    /// versions are hex, so neither needs escaping.
    fn manifest(&self) -> String {
        let mut json = String::with_capacity(16 + self.versions.len() * 64);
        json.push_str("{\"pages\":{");
        for (i, (path, version)) in self.versions.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push('"');
            json.push_str(&url(path));
            json.push_str("\":\"");
            json.push_str(version);
            json.push('"');
        }
        for (key, version) in &self.generated {
            if json.len() > "{\"pages\":{".len() {
                json.push(',');
            }
            json.push_str("\"/");
            json.push_str(&encode(key));
            json.push_str("\":\"");
            json.push_str(version);
            json.push('"');
        }
        json.push_str("}}");
        json
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

        // A link can resolve differently once a path or an alias with its key appears or
        // vanishes. Its note links to a path in that key's buckets now, or it is unresolved.
        let mut keys: HashSet<String> = removed.iter().chain(&added).map(|p| key(p)).collect();
        for path in &removed {
            keys.extend(self.index.aliases(path).iter().map(|a| key(a)));
            self.titles.remove(path);
        }

        // A retitled note changes the backlink text on every page it links to.
        let mut changed = BTreeSet::new();
        let mut aliased = Vec::new();
        for (path, bytes) in fresh.iter().filter(|(p, _)| won.contains(*p)) {
            let meta = front::meta(&String::from_utf8_lossy(bytes));
            if self.index.aliases(path) != meta.aliases.as_slice() {
                let old: HashSet<String> =
                    self.index.aliases(path).iter().map(|a| key(a)).collect();
                let new: HashSet<String> = meta.aliases.iter().map(|a| key(a)).collect();
                keys.extend(old.symmetric_difference(&new).cloned());
                aliased.push((path.clone(), meta.aliases));
            }
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

        let mut first: BTreeSet<String> =
            fresh.keys().filter(|p| won.contains(*p)).cloned().collect();
        for k in &keys {
            let owners = self
                .index
                .bucket(k)
                .iter()
                .chain(self.index.alias_bucket(k));
            for path in owners {
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
        // An embedder shows the note's body, so it re-renders with it. Embeds list what
        // they read at any depth, so one step reaches every embedder.
        let embedders: Vec<String> = first
            .iter()
            .chain(&removed)
            .filter_map(|p| self.embedded_by.get(p.as_str()))
            .flatten()
            .map(|n| n.to_string())
            .collect();
        first.extend(embedders);
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
        for (path, aliases) in aliased {
            self.index.set_aliases(&path, &aliases);
        }

        let mut bodies = HashMap::new();
        let first: Vec<String> = first.into_iter().collect();
        // Batches keep a full sync from holding every source and every body at once.
        for batch in first.chunks(256) {
            // A dependent that cannot be read keeps its page; its own event follows.
            let notes: Vec<(&str, Vec<u8>)> = batch
                .iter()
                .filter_map(|path| {
                    let bytes = fresh
                        .remove(path)
                        .or_else(|| source.note(path).ok().map(|n| n.bytes))?;
                    Some((path.as_str(), bytes))
                })
                .collect();
            let rendered = self.render_notes(&notes, fresh, source);
            drop(notes);
            for (path, body) in rendered {
                let links = Links {
                    targets: body.targets.into_iter().collect(),
                    unresolved: body.broken.iter().map(|b| key(&b.target)).collect(),
                    // An embed that could not be read is still one: its save re-renders this.
                    embeds: body.embeds.union(&body.missing).cloned().collect(),
                };
                broken.extend(body.broken);
                if let Some(note) = self.index.get(path) {
                    self.relink(&note, links, &mut changed);
                }
                bodies.insert(path.to_string(), body.html);
            }
        }

        // Their links resolve as before, so the graph stands.
        for path in changed {
            let path = path.to_string();
            let note = self.entries.get(&path) == Some(&Kind::Note);
            if !note || !won.contains(&path) || bodies.contains_key(&path) {
                continue;
            }
            if let Ok(Note { bytes, .. }) = source.note(&path) {
                let notes = [(path.as_str(), bytes)];
                if let Some((_, body)) = self.render_notes(&notes, fresh, source).pop() {
                    bodies.insert(path, body.html);
                }
            }
        }
        bodies
    }

    /// Renders `notes` with every note their embeds read. A scan of the text finds most
    /// of them first; the render reports any it missed, which load before that note
    /// renders again. So a note renders the same in any batch.
    fn render_notes<'p>(
        &self,
        notes: &[(&'p str, Vec<u8>)],
        fresh: &HashMap<String, Vec<u8>>,
        source: &mut impl Source,
    ) -> Vec<(&'p str, Body)> {
        let mut load = |target: &str| {
            notes
                .iter()
                .find(|(p, _)| *p == target)
                .map(|(_, b)| b.clone())
                .or_else(|| fresh.get(target).cloned())
                .or_else(|| source.note(target).ok().map(|n| n.bytes))
        };
        let targets = |from: &str, bytes: &[u8]| {
            embed_targets(from, &String::from_utf8_lossy(bytes), &self.index)
        };
        let mut sources = Sources::new();
        let mut tried: HashSet<Arc<str>> = HashSet::new();
        let mut level: Vec<Arc<str>> = notes.iter().flat_map(|(p, b)| targets(p, b)).collect();
        for _ in 0..EMBED_DEPTH {
            let mut next = Vec::new();
            for target in level {
                if !tried.insert(Arc::clone(&target)) {
                    continue;
                }
                if let Some(bytes) = load(&target) {
                    next.extend(targets(&target, &bytes));
                    sources.insert(target.to_string(), bytes);
                }
            }
            level = next;
        }
        let mut rendered = render_all(notes, &self.index, &sources);
        loop {
            let mut loaded = false;
            for (_, body) in &rendered {
                for target in &body.missing {
                    if tried.insert(Arc::clone(target))
                        && let Some(bytes) = load(target)
                    {
                        sources.insert(target.to_string(), bytes);
                        loaded = true;
                    }
                }
            }
            if !loaded {
                return rendered;
            }
            for ((path, body), (_, bytes)) in rendered.iter_mut().zip(notes) {
                if !body.missing.is_empty() {
                    let markdown = String::from_utf8_lossy(bytes);
                    *body = render_body(path, &markdown, &self.index, &sources);
                }
            }
        }
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
        for target in old
            .embeds
            .iter()
            .filter(|t| links.embeds.binary_search(t).is_err())
        {
            if let Some(by) = self.embedded_by.get_mut(target) {
                by.remove(note);
                if by.is_empty() {
                    self.embedded_by.remove(target);
                }
            }
        }
        for target in &links.embeds {
            self.embedded_by
                .entry(Arc::clone(target))
                .or_default()
                .insert(Arc::clone(note));
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
            Kind::Note => source.note(path).map(|note| {
                self.modified.insert(path.to_string(), note.modified);
                fresh.insert(path.to_string(), note.bytes);
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
