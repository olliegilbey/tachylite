//! The pure core: vault changes in, output out. A full build is `Sync` of the root.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{Daily, Exclude, Nav};
use crate::front;
use crate::indexes::{self, Folder, Item, Tab};
use crate::links::{Index, key};
use crate::render::{
    Body, Broken, EMBED_DEPTH, NAME_MAX, Page, Sources, crumbs, day_links, embed_targets, encode,
    folder_url, note_title, render_body, render_page, url,
};
use crate::vault::{Entry, Kind, ReadError, SkipReason, Skipped, VaultDir, classify};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The path may have changed. The site classifies it and reads its current state.
    Touch(String),
    /// `entries` are every publishable file under the folder `under` (`""` is the vault root).
    Sync { under: String, entries: Vec<Entry> },
}

/// A note or html page as read: its bytes, and its modified time in seconds since the
/// epoch, with the nanoseconds past them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    pub bytes: Vec<u8>,
    pub modified: i64,
    pub nanos: u32,
}

/// A note with no known modified time, as in-memory sources give.
impl From<Vec<u8>> for Note {
    fn from(bytes: Vec<u8>) -> Note {
        Note {
            bytes,
            ..Note::default()
        }
    }
}

/// Where the site reads the vault. `VaultDir` in production; a map in tests.
pub trait Source {
    /// Reads a note, or an html page to copy. One past `NOTE_MAX` is `TooLarge`.
    fn note(&mut self, path: &str) -> Result<Note, ReadError>;
    /// Checks that a regular file is there, without reading it. Returns its modified time
    /// in seconds since the epoch.
    fn file(&mut self, path: &str) -> Result<i64, ReadError>;
}

impl Source for &VaultDir {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        self.read_note(path, true)
    }
    fn file(&mut self, path: &str) -> Result<i64, ReadError> {
        self.modified(path, true)
    }
}

/// Where `Site::apply_to` and `Site::derive_to` send output, as they make it. An apply sends:
/// 1. the removed vault files' deletes, then their gone pushes;
/// 2. the html pages' and assets' writes;
/// 3. the notes it read, then the pages that depend on them, in batches: each batch's page
///    writes, then their pushes;
/// 4. each page whose backlinks changed after its write: its write, then its push.
///
/// A derive sends:
/// 1. each removed index page's delete and gone push;
/// 2. the index pages' writes, each followed by its push, and the switcher list's write;
/// 3. each changed search shard's write, or its delete once it holds no note;
/// 4. the manifest.
///
/// Paths are relative to the output directory. Streaming means a full sync never holds
/// every page at once.
pub trait Sink {
    fn delete(&mut self, out: String);
    fn write(&mut self, write: Write);
    fn push(&mut self, push: Push);
    /// The page last written to `out`, if it can be read back. A note whose backlinks alone
    /// changed takes its new footer around the body there, rather than render again. A
    /// search shard takes each note's text from the body there.
    fn page(&mut self, out: &str) -> Option<String>;
    /// Whether `page` reads back the pages of earlier applies too. Search shards are built
    /// only through a sink that does: any other would write a shard with rows missing.
    fn keeps(&self) -> bool;
}

/// What an apply found that is not output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub skipped: Vec<Skipped>,
    /// The broken links of every note rendered for its own change or a link target's.
    pub broken: Vec<Broken>,
    /// Whether the apply unpublished a path. It may be excluded now, or moved into an
    /// excluded folder, so the caller derives at once rather than wait for its queue to
    /// drain, and no index page keeps it.
    pub unpublished: bool,
    /// After a sync of the root: each pin and daily format the header cannot show, and why.
    pub notices: Vec<String>,
}

/// All of an apply's output and its report, collected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delta {
    pub deletes: Vec<String>,
    pub writes: Vec<Write>,
    pub pushes: Vec<Push>,
    pub skipped: Vec<Skipped>,
    pub broken: Vec<Broken>,
    pub notices: Vec<String>,
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
    /// Only this apply's pages: an earlier page renders again.
    fn page(&mut self, out: &str) -> Option<String> {
        self.writes.iter().rev().find_map(|w| match w {
            Write::Page { out: o, html } if o == out => Some(html.to_string()),
            _ => None,
        })
    }
    /// So a delta holds no search shard, and its manifest lists none.
    fn keeps(&self) -> bool {
        false
    }
}

/// A note read this apply: what linking needs, and its bytes while they fit `FRESH_MAX`.
struct Fresh {
    meta: front::Meta,
    bytes: Option<Vec<u8>>,
}

/// What one apply has rendered and written so far.
#[derive(Default)]
struct Done {
    /// The notes whose bodies it rendered.
    rendered: HashSet<String>,
    /// The notes whose backlinks or daily neighbours may have changed since this apply last
    /// wrote their page.
    foot: BTreeSet<Arc<str>>,
    /// The notes it pushed.
    pushed: HashSet<String>,
    /// Rendered bodies, so a page whose backlinks change later takes a new footer unread.
    held: HashMap<String, String>,
    /// The held notes, oldest first.
    order: VecDeque<String>,
    /// The bytes `held` holds.
    bytes: usize,
    /// The held notes whose page is not written yet, in render order: the newest of `order`.
    unwritten: VecDeque<String>,
}

impl Done {
    fn hold(&mut self, path: &str, body: String) {
        self.bytes += body.len();
        if let Some(old) = self.held.insert(path.to_string(), body) {
            self.bytes -= old.len();
        } else {
            self.order.push_back(path.to_string());
        }
        self.unwritten.push_back(path.to_string());
    }

    /// Drops the oldest held bodies until `held` fits `max` bytes. Returns those whose page
    /// is not written yet, to be written at once.
    fn spill(&mut self, max: usize) -> Vec<(String, String)> {
        let mut spilled = Vec::new();
        while self.bytes > max {
            let Some(path) = self.order.pop_front() else {
                break;
            };
            let Some(body) = self.held.remove(&path) else {
                continue;
            };
            self.bytes -= body.len();
            if self.unwritten.front() == Some(&path) {
                self.unwritten.pop_front();
                spilled.push((path, body));
            }
        }
        spilled
    }
}

/// The rendered body bytes an apply holds after each render batch, so it holds at most this
/// plus one batch. A full sync past it writes its oldest pages early,
/// and a page whose backlinks change after takes its new footer from the written page. So a
/// vault of any size builds in bounded memory, and a small one writes each page once.
/// The budget vault's bodies come to 27 MB: under this, so its cold build writes no page twice.
const HOLD_MAX: usize = 32 * 1024 * 1024;

/// The note bytes an apply keeps from its first read. A save keeps its note, so its render
/// reads it once; a full sync reads the rest again per batch, so it never holds every note.
const FRESH_MAX: usize = 1024 * 1024;

/// The most threads a render batch takes. On the budget vault, the cold build ran little
/// faster past 6 threads, and a thread that shares a core billed CPU time for no speed.
const RENDER_THREADS: usize = 8;

/// The site's name when config names none.
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
        _ if key == NOTES_JSON || key.starts_with(SEARCH) => format!("files/{key}"),
        _ if key.starts_with(FOLDERS) || key.starts_with(RECENT_IN) => {
            format!("pages/{key}/.index.html")
        }
        _ => format!("pages/{key}.html"),
    }
}

/// The URL key prefix of the folder indexes.
const FOLDERS: &str = ".tachylite/folders/";

/// The URL key prefix of the recent pages of folders. Each is a dot name inside its
/// folder's path, as a folder index is.
const RECENT_IN: &str = ".tachylite/recent/";

/// The URL key of the Today page.
const TODAY: &str = ".tachylite/today";

/// The URL key prefix of the tag pages. A tag holds no `.`, so `<tag>.html` never clashes
/// with the folder of a nested tag.
const TAGS: &str = ".tachylite/tags/";

/// The URL key prefix of the search shards. Each holds the text of the notes whose paths
/// hash to it, one row a note.
const SEARCH: &str = ".tachylite/search/";

/// The rows of the tag pages one apply builds.
struct Rows {
    /// Each listed note's place in listing order.
    places: HashMap<String, usize>,
    /// Each note's row, by place.
    rows: Vec<String>,
}

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
    /// An html page as read from `from`, with its source's modified time. Its version
    /// hashes these bytes, so the file written is the one the manifest names.
    Html {
        out: String,
        from: String,
        bytes: Vec<u8>,
        modified: i64,
        nanos: u32,
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
    /// Vault paths whose output is written. Shared, so an apply that lists no new path
    /// reuses them without a copy.
    winners: Arc<BTreeSet<String>>,
    /// The claims that lost to them, with why.
    lost: Vec<(String, SkipReason)>,
    /// Whether a path joined or left `entries` since the winners were last worked out.
    relist: bool,
    /// The version of each winning note's written page.
    /// Sorted, so the manifest is written in one pass.
    versions: BTreeMap<String, String>,
    /// Where each written note page holds its body: start and length in bytes.
    spans: HashMap<String, (usize, usize)>,
    /// The rendered body bytes an apply holds, when not `HOLD_MAX`.
    hold_max: Option<usize>,
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
    /// The version of each winning html page read whole: a hash of its bytes. A sync
    /// downloads these. A larger html page has none, and loads on demand.
    html: BTreeMap<String, String>,
    /// The modified time of each note and html page, for the recent pages.
    modified: HashMap<String, i64>,
    /// Each winning note's tag keys, sorted.
    tags: HashMap<String, Vec<String>>,
    /// The winning notes that carry each tag key.
    tagged: BTreeMap<String, BTreeSet<String>>,
    /// The tag keys whose pages may have changed since the last apply.
    dirty_tags: BTreeSet<String>,
    /// The search shards whose rows may have changed since the last derive: a note of
    /// theirs was read, rendered or removed.
    dirty_shards: BTreeSet<u32>,
    /// Whether a manifest was written, so the first derive writes one even when empty.
    listed: bool,
    /// Whether an apply changed what the manifest lists since it was written.
    unlisted: bool,
    /// Whether an apply ran since the last derive.
    due: bool,
    /// The header and pins from config.
    nav: Nav,
    /// Obsidian's daily-notes settings, which `nav.daily` overrides.
    obsidian: Option<Daily>,
    /// The daily folder and its format, when a Today page is built.
    daily: Option<(String, Vec<indexes::Part>)>,
    /// The winning daily notes as `(ISO date, path)`, so in date order. A daily note's page
    /// links to the one before it and the one after.
    days: BTreeSet<(String, String)>,
    /// Every page's header, from `nav` and `daily` alone.
    header: String,
    /// The headers of the home page, the Today page and the Recent page: each marks its tab.
    tabs: [String; 3],
    /// What each folder's recent page was built from, hashed, so a derive rebuilds only the
    /// pages of the folders a save reached.
    scoped: HashMap<String, u64>,
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

/// A pin as written, `path` or `path|label`: its vault path, and how the header shows it.
/// A path the publish list names is a file; any other is a folder. Its URL follows from
/// the path alone, so no note's state reaches the header.
fn pin(text: &str) -> Option<(String, indexes::Pin)> {
    let (path, label) = text.split_once('|').unwrap_or((text, ""));
    let path = path.trim();
    let path = path.strip_prefix("./").unwrap_or(path).trim_matches('/');
    if path.is_empty() {
        return None;
    }
    let file = classify(path, &Exclude::default()).is_some();
    let (url, name) = if file {
        (url(path), note_title(path))
    } else {
        (folder_url(path), path.rsplit('/').next().unwrap_or(path))
    };
    let label = match label.trim() {
        "" => name,
        label => label,
    };
    Some((
        path.to_string(),
        indexes::Pin {
            label: label.to_string(),
            url,
        },
    ))
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// The name a daily note's date is read from: its path under `folder`, without `.md`.
fn daily_name<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    let rest = if folder.is_empty() {
        path
    } else {
        path.strip_prefix(folder)?.strip_prefix('/')?
    };
    rest.get(..rest.len().checked_sub(".md".len())?)
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
    let threads = cores.min(RENDER_THREADS).min(notes.len().div_ceil(16));
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
        let mut site = Site {
            exclude,
            ..Site::default()
        };
        site.renav();
        site
    }

    /// Takes effect at the next `Sync` of the root, which re-lists every file.
    pub fn set_exclude(&mut self, exclude: Exclude) {
        self.exclude = exclude;
        self.renav();
    }

    /// Sets the header's config. A note page takes it at the next `Sync` of the root, which
    /// renders every note; the index pages, at the next derive.
    pub fn set_nav(&mut self, nav: Nav) {
        self.nav = nav;
        self.renav();
    }

    /// Sets Obsidian's daily-notes settings, as `set_nav` sets the config's. Returns
    /// whether they changed.
    pub fn set_obsidian(&mut self, daily: Option<Daily>) -> bool {
        if self.obsidian == daily {
            return false;
        }
        self.obsidian = daily;
        self.renav();
        true
    }

    fn renav(&mut self) {
        // An excluded folder publishes no daily note, so its Today link would lead nowhere.
        let daily = self.nav.daily.as_ref().or(self.obsidian.as_ref());
        let daily = daily
            .filter(|d| !self.exclude.covers(&d.folder))
            .and_then(|d| {
                let format = indexes::date_format(&d.format).ok()?;
                Some((d.folder.clone(), format))
            });
        if daily != self.daily {
            self.daily = daily;
            // Other notes are daily notes now. The sync of the root that follows renders
            // every note, so each page takes its new line.
            self.days = self
                .winners
                .iter()
                .filter_map(|path| Some((self.day(path)?, path.clone())))
                .collect();
        }
        let pins: Vec<indexes::Pin> = self
            .nav
            .pins
            .iter()
            .filter_map(|text| pin(text))
            .filter(|(path, _)| !self.exclude.covers(path))
            .map(|(_, pin)| pin)
            .collect();
        let today = self.daily.is_some();
        let header = indexes::header(today, &pins, None);
        // The Today page and the home page's name derive again.
        self.due = true;
        if header != self.header {
            self.header = header;
            self.tabs = [Tab::Home, Tab::Today, Tab::Recent]
                .map(|tab| indexes::header(today, &pins, Some(tab)));
            // Tag pages derive only where marked; every one holds the header.
            self.dirty_tags.extend(self.tagged.keys().cloned());
        }
    }

    /// Each pin and daily format the header cannot show, and why. A pin that names no
    /// published path is likely a typo, as an unmatched exclude pattern is.
    fn nav_notices(&self) -> Vec<String> {
        let mut notices = Vec::new();
        if let Some(daily) = self.nav.daily.as_ref().or(self.obsidian.as_ref()) {
            if let Err(why) = indexes::date_format(&daily.format) {
                notices.push(format!("{why}; there is no Today page"));
            } else if self.exclude.covers(&daily.folder) {
                notices.push(format!(
                    "daily folder {:?} is excluded; there is no Today page",
                    daily.folder
                ));
            }
        }
        for text in &self.nav.pins {
            let Some((path, _)) = pin(text) else {
                notices.push(format!("pin {text:?} names no path"));
                continue;
            };
            if self.exclude.covers(&path) {
                notices.push(format!(
                    "pin {text:?} is excluded, so the header leaves it out"
                ));
            } else if !self.winners.contains(&path) && !self.holds(&path) {
                notices.push(format!(
                    "pin {text:?} matches no published note, folder or html page; check it for a typo"
                ));
            }
        }
        notices
    }

    /// Whether a published note or html page lies under `folder`, so it has an index.
    fn holds(&self, folder: &str) -> bool {
        let prefix = format!("{folder}/");
        self.winners
            .range(prefix.clone()..)
            .take_while(|p| p.starts_with(&prefix))
            .any(|p| self.entries.get(p).is_some_and(|k| *k != Kind::Asset))
    }

    /// The ISO date a note's name carries, if the daily settings make it a daily note.
    fn day(&self, path: &str) -> Option<String> {
        let (folder, format) = self.daily.as_ref()?;
        let date = indexes::date_of(format, daily_name(path, folder)?)?;
        // Its kind by extension: a removed path is in no list.
        (classify(path, &Exclude::default()) == Some(Kind::Note)).then_some(date)
    }

    /// The daily notes just before and just after `day`, which need not be listed itself.
    fn around(&self, day: &(String, String)) -> [Option<&(String, String)>; 2] {
        let after = (Bound::Excluded(day), Bound::Unbounded);
        [
            self.days.range(..day).next_back(),
            self.days.range::<(String, String), _>(after).next(),
        ]
    }

    /// A daily note's line of links to its neighbours, or nothing for any other note.
    fn day_links(&self, path: &str) -> String {
        let Some(date) = self.day(path) else {
            return String::new();
        };
        let days = self.around(&(date, path.to_string()));
        day_links(days.map(|day| day.map(|(date, path)| (date.as_str(), path.as_str()))))
    }

    pub fn nav(&self) -> &Nav {
        &self.nav
    }

    pub fn exclude(&self) -> &Exclude {
        &self.exclude
    }

    /// Sets the rendered body bytes an apply holds. A test lowers it to reach the early writes.
    pub fn hold_at_most(&mut self, bytes: usize) {
        self.hold_max = Some(bytes);
    }

    /// How many note pages are published.
    pub fn notes(&self) -> usize {
        self.versions.len()
    }

    /// Applies changes, derives the index pages, and collects the output. A delta cannot
    /// read an earlier apply's pages back, so it holds no search shard.
    pub fn apply(&mut self, changes: Vec<Change>, source: &mut impl Source) -> Delta {
        let mut delta = Delta::default();
        let report = self.apply_to(changes, source, &mut delta);
        self.derive_to(&mut delta);
        delta.skipped = report.skipped;
        delta.broken = report.broken;
        delta.notices = report.notices;
        delta
    }

    /// Applies changes and sends the vault files' output to `sink` as it is made. The index
    /// pages and the manifest wait for `derive_to`.
    pub fn apply_to(
        &mut self,
        changes: Vec<Change>,
        source: &mut impl Source,
        sink: &mut impl Sink,
    ) -> Report {
        self.due = true;
        let mut report = Report::default();
        let mut touched = BTreeSet::new();
        let root = changes
            .iter()
            .any(|c| matches!(c, Change::Sync { under, .. } if under.is_empty()));
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

        let mut fresh: HashMap<String, Fresh> = HashMap::new();
        let mut kept = 0;
        let mut mid_save = Vec::new();
        for path in &touched {
            if self.probe(path, source, &mut fresh, &mut kept, &mut report.skipped) {
                mid_save.push(path.clone());
            }
        }
        for path in &mid_save {
            touched.remove(path);
        }

        // Winners depend only on the listed paths, so a save that lists none new keeps them.
        // A loser promoted by this change was not read yet. Read it, and recompute: if the
        // read failed, it left the entries.
        let (won, lost) = loop {
            if !self.relist {
                break (Arc::clone(&self.winners), self.lost.clone());
            }
            self.relist = false;
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
                break (Arc::new(won), lost);
            }
            for path in &unread {
                self.probe(path, source, &mut fresh, &mut kept, &mut report.skipped);
            }
            self.relist = true;
        };

        for (path, reason) in &lost {
            if touched.contains(path) || self.winners.contains(path) {
                report.skipped.push(Skipped {
                    path: PathBuf::from(path),
                    reason: *reason,
                });
            }
        }
        self.lost = lost;

        let (mut own, deps, foot) = self.link(&won, &mut fresh);

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
            report.unpublished = true;
            self.unlisted |= kind != Kind::Note;
            self.html.remove(path);
            if kind == Kind::Note {
                self.unlisted |= self.versions.remove(path).is_some();
                self.spans.remove(path);
                self.dirty_shards.insert(indexes::shard(path));
                if !live.contains(note_key(path)) {
                    gone.push(note_key(path).to_string());
                }
            }
        }
        for key in gone {
            sink.push(Push::Gone { key });
        }

        // Html pages and assets go first, so no pushed page links to a file not yet written.
        // Only a new or touched one is copied, so a save walks no other path.
        let copies: BTreeSet<&String> = won
            .difference(&self.winners)
            .chain(touched.iter().filter(|p| won.contains(*p)))
            .collect();
        for path in copies {
            let kind = self.entries.get(path).copied().unwrap_or(Kind::Asset);
            if kind == Kind::Note {
                continue;
            }
            self.unlisted |= !self.winners.contains(path);
            let (out, from) = (claim(path, kind).1, path.clone());
            // An html page that fits `NOTE_MAX` is read here, once, and written from that
            // read. Any other read leaves the copy, and its error, to the sink.
            let page = (kind == Kind::Html).then(|| source.note(path).ok());
            let Some(note) = page.flatten() else {
                self.unlisted |= self.html.remove(path).is_some();
                sink.write(Write::Copy { out, from });
                continue;
            };
            let mut hasher = DefaultHasher::new();
            note.bytes.hash(&mut hasher);
            let version = format!("{:016x}", hasher.finish());
            self.unlisted |= self.html.get(path) != Some(&version);
            self.html.insert(from.clone(), version);
            sink.write(Write::Html {
                out,
                from,
                bytes: note.bytes,
                modified: note.modified,
                nanos: note.nanos,
            });
        }

        // The notes read this apply go first, so a save's own push waits for no dependent.
        // New notes lead, so a page that links to one finds it written. Their pages show the
        // backlinks as their own links leave them. Then the notes whose links may resolve
        // differently, and the embedders.
        let mut done = Done {
            foot,
            ..Done::default()
        };
        own.sort_by_key(|p| self.winners.contains(p));
        for paths in [own, deps.into_iter().collect()] {
            let paths: Vec<String> = paths
                .into_iter()
                .filter(|p| !done.rendered.contains(p))
                .collect();
            let notes = (&mut fresh, &mut *source);
            self.render(&paths, notes, sink, &mut done, &mut report.broken);
            self.flush(&mut done, sink);
        }
        // Last, each page whose backlinks changed since it was written takes its new footer
        // around its body: the one held, else the one its written page holds. A page with
        // neither renders again, once.
        loop {
            let stale: Vec<String> = std::mem::take(&mut done.foot)
                .into_iter()
                .filter(|p| won.contains(&**p) && self.entries.get(&**p) == Some(&Kind::Note))
                .map(|p| p.to_string())
                .collect();
            let again: Vec<String> = self
                .refoot(&stale, &mut done, sink)
                .into_iter()
                .filter(|p| !done.rendered.contains(p))
                .collect();
            if again.is_empty() {
                break;
            }
            let notes = (&mut fresh, &mut *source);
            self.render(&again, notes, sink, &mut done, &mut report.broken);
            self.flush(&mut done, sink);
        }
        drop(fresh);

        self.modified.retain(|path, _| won.contains(path));
        self.winners = won;
        if root {
            report.notices = self.nav_notices();
        }
        report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        report.broken.sort_by(|a, b| a.from.cmp(&b.from));
        report
    }

    /// The `(title, url)` of each note that links to `path`, by path.
    fn backlinks(&self, path: &str) -> Vec<(String, String)> {
        self.inbound
            .get(path)
            .into_iter()
            .flatten()
            .filter(|from| ***from != *path)
            .map(|from| (self.title(from).to_string(), url(from)))
            .collect()
    }

    /// Wraps each of `bodies` in its page with its current backlinks, writes each whose
    /// version changed or whose note is new and not yet pushed, and returns their pushes.
    /// `rendered` says the bodies are new renders, so their search rows may have changed.
    /// A body read back for a new footer is the one its row was built from.
    fn emit_notes(
        &mut self,
        bodies: &[(&str, &str)],
        rendered: bool,
        done: &mut Done,
        sink: &mut impl Sink,
    ) -> Vec<Push> {
        let mut pushes = Vec::new();
        for &(path, body) in bodies {
            done.foot.remove(path);
            let backlinks = self.backlinks(path);
            let days = self.day_links(path);
            let title = self.title(path);
            let page = render_page(&self.header, path, title, &days, body, &backlinks);
            let new = !self.winners.contains(path) && !done.pushed.contains(path);
            if !new && self.versions.get(path) == Some(&page.version) {
                continue;
            }
            done.pushed.insert(path.to_string());
            self.versions.insert(path.to_string(), page.version.clone());
            self.unlisted = true;
            if rendered {
                self.dirty_shards.insert(indexes::shard(path));
            }
            // Where the body sits in the page, so a new footer can go around it unrendered.
            let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
            let start = page
                .html
                .find("<main>\n")
                .map(|at| at + "<main>\n".len() + crumbs(folder).len() + days.len());
            match start.filter(|&at| page.html.get(at..at + body.len()) == Some(body)) {
                Some(at) => self.spans.insert(path.to_string(), (at, body.len())),
                None => self.spans.remove(path),
            };
            let html: Arc<str> = Arc::from(page.html);
            sink.write(Write::Page {
                out: claim(path, Kind::Note).1,
                html: Arc::clone(&html),
            });
            pushes.push(Push::Page {
                key: note_key(path).to_string(),
                version: page.version,
                html,
            });
        }
        pushes
    }

    /// Writes and pushes each note page whose backlinks or daily neighbours may have changed
    /// since this apply last wrote it, with the new line of days and the new footer around
    /// its body: the one held, else the one its written page holds. Returns the notes with
    /// neither.
    fn refoot(&mut self, paths: &[String], done: &mut Done, sink: &mut impl Sink) -> Vec<String> {
        let mut lost = Vec::new();
        // Small batches keep a full sync from holding many pages read back at once.
        for batch in paths.chunks(32) {
            let mut read: Vec<(&str, String, (usize, usize))> = Vec::new();
            let mut kept: Vec<&str> = Vec::new();
            for path in batch {
                if done.held.contains_key(path.as_str()) {
                    kept.push(path);
                    continue;
                }
                match self.written(path, sink) {
                    Some((page, span)) => read.push((path, page, span)),
                    None => lost.push(path.clone()),
                }
            }
            let held = std::mem::take(&mut done.held);
            let bodies: Vec<(&str, &str)> = read
                .iter()
                .map(|(path, page, (start, len))| (*path, &page[*start..start + len]))
                .chain(
                    kept.iter()
                        .filter_map(|p| Some((*p, held.get(*p)?.as_str()))),
                )
                .collect();
            self.write_pages(&bodies, false, done, sink);
            drop(bodies);
            done.held = held;
        }
        lost
    }

    /// Derives the index pages and the switcher list, and writes the manifest, if any apply
    /// since the last derive changed what they show. The watcher calls it once its queue is
    /// empty, so a burst of saves pays it once. It also calls it at once after an unpublish, and
    /// after a second of unbroken burst. A full build calls it after its one apply.
    pub fn derive_to(&mut self, sink: &mut impl Sink) {
        if !self.due && self.listed {
            return;
        }
        self.due = false;
        let won = std::mem::take(&mut self.winners);
        self.unlisted |= self.indexes(&won, sink);
        if self.unlisted || !self.listed {
            sink.write(Write::Manifest {
                json: self.manifest(&won),
            });
            self.listed = true;
            self.unlisted = false;
        }
        self.winners = won;
    }

    /// Derives every index page and the switcher list from the winners, and writes and
    /// pushes the ones whose version changed. Returns whether any changed.
    fn indexes(&mut self, won: &BTreeSet<String>, sink: &mut impl Sink) -> bool {
        let mut built: Vec<(String, String, String)> = Vec::new();
        let mut kept: Vec<String> = Vec::new();
        let mut inputs: HashMap<String, u64> = HashMap::new();
        {
            let header = self.header.as_str();
            let [home, today, recent_tab] = &self.tabs;
            // Every folder that holds a page, with its subfolders and pages.
            let mut tree: BTreeMap<&str, (BTreeSet<&str>, Vec<Item>)> = BTreeMap::new();
            tree.entry("").or_default();
            // The notes and html pages, for the recent lists.
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
                    let (subs, _) = tree.entry(parent(child)).or_default();
                    if !subs.insert(child) {
                        break;
                    }
                    child = parent(child);
                }
                let modified = self.modified.get(path).copied().unwrap_or_default();
                let title = if kind == Kind::Note {
                    self.title(path)
                } else {
                    name
                };
                let url = url(path);
                // The recent lists and the switcher name an html page without its `.html`.
                // Its path keeps it, so the switcher shows which kind a row opens.
                let key = if kind == Kind::Note {
                    note_key(path)
                } else {
                    path
                };
                notes.push((modified, path));
                json.push(self.title(path), &url, key, self.index.aliases(path));
                tree.entry(folder).or_default().1.push(Item {
                    title,
                    url,
                    folder,
                    modified,
                });
            }
            // Newest first; equal times by path.
            notes.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
            // The newest daily notes by date open the Recent page. Its list of the rest and
            // the home page's leave them out: an agent saves today's daily note all day.
            let newest: Vec<usize> = self
                .days
                .iter()
                .rev()
                .take(indexes::DAILY)
                .filter_map(|(_, day)| notes.iter().position(|&(_, path)| path == day))
                .collect();
            let listed: Vec<usize> = (0..notes.len())
                .filter(|place| !newest.contains(place))
                .take(indexes::RECENT)
                .collect();
            // The newest notes under each folder, at any depth, by place in `notes`. A folder
            // has every note its subfolder has, so once a folder's list is full, so is each
            // folder above it.
            let mut scoped: HashMap<&str, Vec<usize>> = HashMap::new();
            for (place, &(_, path)) in notes.iter().enumerate() {
                let mut folder = parent(path);
                while !folder.is_empty() {
                    let list = scoped.entry(folder).or_default();
                    if list.len() == indexes::RECENT {
                        break;
                    }
                    list.push(place);
                    folder = parent(folder);
                }
            }
            // A folder whose list reads as it did needs no new page.
            scoped.retain(|folder, places| {
                let mut hasher = DefaultHasher::new();
                header.hash(&mut hasher);
                for &place in places.iter() {
                    let (modified, path) = notes[place];
                    (modified, path, self.title(path)).hash(&mut hasher);
                }
                let input = hasher.finish();
                inputs.insert(folder.to_string(), input);
                let same = self.scoped.get(*folder) == Some(&input)
                    && self.generated.contains_key(&format!("{RECENT_IN}{folder}"));
                if same {
                    kept.push(format!("{RECENT_IN}{folder}"));
                }
                !same
            });
            // Each listed note's row, made once.
            let mut rows: Vec<Option<String>> = vec![None; notes.len()];
            let places = listed
                .iter()
                .chain(&newest)
                .chain(scoped.values().flatten())
                .copied();
            for place in places {
                let (modified, path) = notes[place];
                rows[place].get_or_insert_with(|| {
                    indexes::recent_row(&Item {
                        title: self.title(path),
                        url: url(path),
                        folder: parent(path),
                        modified,
                    })
                });
            }
            let row = |place: usize| rows[place].as_deref().unwrap_or_default();
            let recent: Vec<&str> = listed.iter().map(|&place| row(place)).collect();
            let daily: Vec<&str> = newest.iter().map(|&place| row(place)).collect();
            let site = self.nav.title.as_deref().unwrap_or(SITE);
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
                    (String::new(), indexes::home(home, site, &recent, &folder))
                } else {
                    let page = indexes::folder(header, &folder, inputs.contains_key(path));
                    (format!("{FOLDERS}{path}"), page)
                };
                built.push((key, page.version, page.html));
            }
            let page = indexes::recent(recent_tab, &daily, &recent);
            built.push((".tachylite/recent".into(), page.version, page.html));
            for (folder, places) in &scoped {
                let page = indexes::recent_in(header, folder, places.iter().map(|&p| row(p)));
                built.push((format!("{RECENT_IN}{folder}"), page.version, page.html));
            }
            if self.daily.is_some() {
                let days: Vec<(&str, &str, String)> = self
                    .days
                    .iter()
                    .rev()
                    .take(indexes::TODAY)
                    .map(|(date, path)| (date.as_str(), self.title(path), url(path)))
                    .collect();
                let page = indexes::today(today, &days);
                built.push((TODAY.into(), page.version, page.html));
            }
            let json = json.finish();
            let mut hasher = DefaultHasher::new();
            json.hash(&mut hasher);
            built.push((NOTES_JSON.into(), format!("{:016x}", hasher.finish()), json));
        }

        self.scoped = inputs;
        let fresh: HashSet<String> = built
            .iter()
            .map(|(key, ..)| key.clone())
            .chain(kept)
            .collect();
        // Tag pages derive only where a tag was added, removed or retitled, so a save that
        // changes no tag builds none. Search shards derive only where a note changed.
        let mut gone: Vec<String> = self
            .generated
            .keys()
            .filter(|key| !key.starts_with(TAGS) && !key.starts_with(SEARCH))
            .filter(|key| !fresh.contains(*key))
            .cloned()
            .collect();
        // A tag page is built as it is written, so a full sync never holds them all.
        let tags = self.dirty_tag_pages();
        for tag in &tags {
            let key = format!("{TAGS}{tag}");
            if !self.tagged_any(tag) && self.generated.contains_key(&key) {
                gone.push(key);
            }
        }

        let mut changed = false;
        for key in gone {
            self.generated.remove(&key);
            sink.delete(generated_out(&key));
            sink.push(Push::Gone { key });
            changed = true;
        }
        for (key, version, html) in built {
            changed |= self.emit(key, version, html, sink);
        }
        let rows = self.rows(&tags);
        for tag in tags {
            if let Some(page) = self.tag_page(&tag, &rows) {
                changed |= self.emit(format!("{TAGS}{tag}"), page.version, page.html, sink);
            }
        }
        changed | self.shards(sink)
    }

    /// Rebuilds each dirty search shard, and writes the ones whose version changed. A shard
    /// holds one row for each of its notes, in path order. Each note's text comes from the
    /// body in its written page, read back one shard at a time, so the site holds no text
    /// and a full build takes this path too. A page that cannot be read back has no row.
    /// A sink that keeps no earlier page builds none, and leaves them dirty.
    /// Returns whether any shard changed.
    fn shards(&mut self, sink: &mut impl Sink) -> bool {
        if !sink.keeps() {
            return false;
        }
        let dirty = std::mem::take(&mut self.dirty_shards);
        let mut notes: BTreeMap<u32, Vec<String>> =
            dirty.into_iter().map(|s| (s, Vec::new())).collect();
        for path in self.versions.keys() {
            if let Some(paths) = notes.get_mut(&indexes::shard(path)) {
                paths.push(path.clone());
            }
        }
        let mut changed = false;
        for (shard, paths) in notes {
            let mut rows = String::new();
            for path in &paths {
                if let Some((page, (start, len))) = self.written(path, sink) {
                    let modified = self.modified.get(path).copied().unwrap_or_default();
                    indexes::search_row(&mut rows, &url(path), modified, &page[start..start + len]);
                }
            }
            let key = indexes::shard_key(shard);
            if rows.is_empty() {
                if self.generated.remove(&key).is_some() {
                    sink.delete(generated_out(&key));
                    changed = true;
                }
                continue;
            }
            let mut hasher = DefaultHasher::new();
            rows.hash(&mut hasher);
            // Short, since the manifest lists hundreds of them: 32 bits tell two
            // contents of one shard apart.
            let version = format!("{:08x}", hasher.finish() & 0xffff_ffff);
            changed |= self.emit(key, version, rows, sink);
        }
        changed
    }

    /// A note's written page and where its body sits in it, if the page can be read back
    /// and is the one this site last wrote.
    fn written(&self, path: &str, sink: &mut impl Sink) -> Option<(String, (usize, usize))> {
        let (start, len) = self.spans.get(path).copied()?;
        let version = self.versions.get(path)?;
        let page = sink.page(&claim(path, Kind::Note).1)?;
        // The version in its head shows the page is the one this site wrote.
        let ours = page.get(..start)?.contains(version.as_str());
        (ours && page.get(start..start + len).is_some()).then_some((page, (start, len)))
    }

    /// Writes and pushes a generated page whose version changed. Returns whether it did.
    fn emit(&mut self, key: String, version: String, html: String, sink: &mut impl Sink) -> bool {
        if self.generated.get(&key) == Some(&version) {
            return false;
        }
        let html: Arc<str> = Arc::from(html);
        let out = generated_out(&key);
        // A file has no page to push to: the switcher list, a search shard.
        let page = out.starts_with("pages/");
        sink.write(Write::Page {
            out,
            html: Arc::clone(&html),
        });
        if page {
            sink.push(Push::Page {
                key: key.clone(),
                version: version.clone(),
                html,
            });
        }
        self.generated.insert(key, version);
        true
    }

    /// Takes the dirty tags, with every tag above each, since a tag's page lists the notes
    /// of the tags nested under it.
    fn dirty_tag_pages(&mut self) -> BTreeSet<String> {
        let mut pages = BTreeSet::new();
        for tag in std::mem::take(&mut self.dirty_tags) {
            let mut tag = tag.as_str();
            while pages.insert(tag.to_string()) {
                match tag.rsplit_once('/') {
                    Some((parent, _)) => tag = parent,
                    None => break,
                }
            }
        }
        pages
    }

    /// The notes of each tag nested under `tag`.
    fn nested(&self, tag: &str) -> impl Iterator<Item = &BTreeSet<String>> {
        // Every key that starts `tag/` sorts between `tag/` and `tag0`.
        self.tagged
            .range(format!("{tag}/")..format!("{tag}0"))
            .map(|(_, notes)| notes)
    }

    /// Whether any note carries `tag` or a tag under it.
    fn tagged_any(&self, tag: &str) -> bool {
        self.tagged.contains_key(tag) || self.nested(tag).next().is_some()
    }

    /// The notes tagged `tag` or a tag under it, in no order. A note with two of the tags
    /// comes twice.
    fn listed(&self, tag: &str) -> impl Iterator<Item = &str> {
        self.tagged
            .get(tag)
            .into_iter()
            .chain(self.nested(tag))
            .flatten()
            .map(String::as_str)
    }

    /// Each note the pages of `tags` list, with its place in listing order, and its row by
    /// that place. A note is on several tag pages, so its row is made once.
    fn rows(&self, tags: &BTreeSet<String>) -> Rows {
        // A set, not a sort, takes out the notes on several pages: paths compare slowly.
        let mut seen = HashSet::new();
        let mut notes: Vec<&str> = tags
            .iter()
            .flat_map(|tag| self.listed(tag))
            .filter(|path| seen.insert(*path))
            .collect();
        notes.sort_by_cached_key(|path| (indexes::order(self.title(path)), *path));
        let rows = notes
            .iter()
            .map(|path| indexes::row(self.title(path), &url(path)))
            .collect();
        let places = notes
            .into_iter()
            .enumerate()
            .map(|(place, path)| (path.to_string(), place))
            .collect();
        Rows { places, rows }
    }

    /// The page of tag `tag`: the notes tagged `tag` or a tag under it, or none if no
    /// note is. `rows` holds every note it lists.
    fn tag_page(&self, tag: &str, rows: &Rows) -> Option<Page> {
        // Places, not titles, are sorted: a title compare reads scattered memory.
        let mut places: Vec<usize> = self
            .listed(tag)
            .filter_map(|path| rows.places.get(path).copied())
            .collect();
        if places.is_empty() {
            return None;
        }
        places.sort_unstable();
        // A note with two nested tags is listed once.
        places.dedup();
        Some(indexes::tag(
            &self.header,
            tag,
            places.into_iter().map(|place| rows.rows[place].as_str()),
        ))
    }

    /// Replaces a note's tag keys, and marks each added or removed tag dirty.
    fn retag(&mut self, path: &str, tags: Vec<String>) {
        let old = self.tags.remove(path).unwrap_or_default();
        if old != tags {
            for tag in old.iter().filter(|t| tags.binary_search(t).is_err()) {
                if let Some(notes) = self.tagged.get_mut(tag) {
                    notes.remove(path);
                    if notes.is_empty() {
                        self.tagged.remove(tag);
                    }
                }
                self.dirty_tags.insert(tag.clone());
            }
            for tag in tags.iter().filter(|t| old.binary_search(t).is_err()) {
                self.tagged
                    .entry(tag.clone())
                    .or_default()
                    .insert(path.to_string());
                self.dirty_tags.insert(tag.clone());
            }
        }
        if !tags.is_empty() {
            self.tags.insert(path.to_string(), tags);
        }
    }

    /// `{"pages":{"<url>":"<version>",...},"files":["<url>",...]}`: note pages in path
    /// order, then generated pages in key order, then the html pages a sync downloads in
    /// path order. `files` lists every html page and asset of `won` in path order. URLs
    /// are percent-encoded and versions are hex, so neither needs escaping.
    fn manifest(&self, won: &BTreeSet<String>) -> String {
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
        for (path, version) in &self.html {
            if json.len() > "{\"pages\":{".len() {
                json.push(',');
            }
            json.push_str("\"/");
            json.push_str(&encode(path));
            json.push_str("\":\"");
            json.push_str(version);
            json.push('"');
        }
        json.push_str("},\"files\":[");
        let files = won
            .iter()
            .filter(|path| self.entries.get(*path).is_some_and(|k| *k != Kind::Note));
        for (i, path) in files.enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str("\"/");
            json.push_str(&encode(path));
            json.push('"');
        }
        json.push_str("]}");
        json
    }

    /// Updates the link index for the new winners. Returns the notes read this apply; every
    /// other winning note whose body may change with them: notes whose links may now resolve
    /// differently, and the embedders of either; and the notes whose backlinks may change
    /// without their body: the targets of a retitled or removed note.
    fn link(
        &mut self,
        won: &BTreeSet<String>,
        fresh: &mut HashMap<String, Fresh>,
    ) -> (Vec<String>, BTreeSet<String>, BTreeSet<Arc<str>>) {
        let removed: Vec<String> = self.winners.difference(won).cloned().collect();
        let added: Vec<String> = won.difference(&self.winners).cloned().collect();

        // A link can resolve differently once a path or an alias with its key appears or
        // vanishes. Its note links to a path in that key's buckets now, or it is unresolved.
        let mut keys: HashSet<String> = removed.iter().chain(&added).map(|p| key(p)).collect();
        for path in &removed {
            keys.extend(self.index.aliases(path).iter().map(|a| key(a)));
            self.titles.remove(path);
        }

        let mut deps = BTreeSet::new();
        let mut foot = BTreeSet::new();

        // A daily note that comes or goes changes the line of the one before it and the one
        // after. Each takes the new line around the body it has, as with a new backlink.
        if self.daily.is_some() {
            let days = removed.iter().map(|p| (p, false));
            for (path, new) in days.chain(added.iter().map(|p| (p, true))) {
                let Some(date) = self.day(path) else {
                    continue;
                };
                let day = (date, path.clone());
                if !new {
                    self.days.remove(&day);
                }
                foot.extend(
                    self.around(&day)
                        .into_iter()
                        .flatten()
                        .map(|(_, path)| Arc::from(path.as_str())),
                );
                if new {
                    self.days.insert(day);
                }
            }
        }

        // A retitled note changes the backlink text on every page it links to.
        let mut aliased = Vec::new();
        for (path, note) in fresh.iter_mut().filter(|(p, _)| won.contains(*p)) {
            let meta = std::mem::take(&mut note.meta);
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
            // A retitled note changes its line on its tags' pages.
            if let Some(tags) = self.tags.get(path) {
                self.dirty_tags.extend(tags.iter().cloned());
            }
            match meta.title {
                Some(title) => self.titles.insert(path.clone(), title),
                None => self.titles.remove(path),
            };
            if let Some(links) = self.links.get(path.as_str()) {
                foot.extend(links.targets.iter().cloned());
            }
        }

        let own: Vec<String> = fresh.keys().filter(|p| won.contains(*p)).cloned().collect();
        for k in &keys {
            let owners = self
                .index
                .bucket(k)
                .iter()
                .chain(self.index.alias_bucket(k));
            for path in owners {
                deps.extend(
                    self.inbound
                        .get(path)
                        .into_iter()
                        .flatten()
                        .map(|n| n.to_string()),
                );
            }
            deps.extend(
                self.unresolved
                    .get(k)
                    .into_iter()
                    .flatten()
                    .map(|n| n.to_string()),
            );
        }
        // An embedder shows the note's body, so it re-renders with it. Embeds list what
        // they read at any depth, so one step reaches every embedder.
        let embedders: Vec<String> = own
            .iter()
            .chain(&deps)
            .chain(&removed)
            .filter_map(|p| self.embedded_by.get(p.as_str()))
            .flatten()
            .map(|n| n.to_string())
            .collect();
        deps.extend(embedders);
        deps.retain(|p| won.contains(p) && !fresh.contains_key(p));

        for path in &removed {
            if let Some(note) = self.index.get(path) {
                self.relink(&note, Links::default(), &mut foot);
                self.links.remove(path.as_str());
            }
            self.retag(path, Vec::new());
            self.index.remove(path);
        }
        for path in &added {
            self.index.insert(path);
        }
        for (path, aliases) in aliased {
            self.index.set_aliases(&path, &aliases);
        }
        let mut own = own;
        own.sort_unstable();
        (own, deps, foot)
    }

    /// Renders the bodies of `paths` in batches, and updates the link graph and tags from
    /// each. A note whose backlinks change joins `done.foot`. Holds each body until `flush`
    /// writes its page; past `HOLD_MAX`, the oldest bodies' pages are written at once. Reports
    /// the broken links new to their note, so a re-render logs none twice.
    fn render(
        &mut self,
        paths: &[String],
        (fresh, source): (&mut HashMap<String, Fresh>, &mut impl Source),
        sink: &mut impl Sink,
        done: &mut Done,
        broken: &mut Vec<Broken>,
    ) {
        // Batches keep a full sync from holding every source at once.
        for batch in paths.chunks(256) {
            // A dependent that cannot be read keeps its page; its own event follows.
            let notes: Vec<(&str, Vec<u8>)> = batch
                .iter()
                .filter_map(|path| {
                    let bytes = fresh
                        .remove(path)
                        .and_then(|f| f.bytes)
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
                let old = self.links.get(path).map_or(&[][..], |l| &l.unresolved);
                broken.extend(
                    body.broken
                        .into_iter()
                        .filter(|b| !old.contains(&key(&b.target))),
                );
                if let Some(note) = self.index.get(path) {
                    self.relink(&note, links, &mut done.foot);
                }
                self.retag(path, body.tags.into_iter().collect());
                done.rendered.insert(path.to_string());
                done.hold(path, body.html);
            }
            let spilled = done.spill(self.hold_max.unwrap_or(HOLD_MAX));
            let bodies: Vec<(&str, &str)> = spilled
                .iter()
                .map(|(p, b)| (p.as_str(), b.as_str()))
                .collect();
            self.write_pages(&bodies, true, done, sink);
        }
    }

    /// Writes the pages of the held bodies not yet written.
    fn flush(&mut self, done: &mut Done, sink: &mut impl Sink) {
        let unwritten = std::mem::take(&mut done.unwritten);
        let held = std::mem::take(&mut done.held);
        let bodies: Vec<(&str, &str)> = unwritten
            .iter()
            .filter_map(|p| Some((p.as_str(), held.get(p)?.as_str())))
            .collect();
        self.write_pages(&bodies, true, done, sink);
        drop(bodies);
        done.held = held;
    }

    /// Writes and pushes the pages of `bodies` in small batches: each batch's writes, then
    /// its pushes, so a pushed page links to no page of its batch not yet written, and a
    /// batch holds few pages.
    fn write_pages(
        &mut self,
        bodies: &[(&str, &str)],
        rendered: bool,
        done: &mut Done,
        sink: &mut impl Sink,
    ) {
        for batch in bodies.chunks(32) {
            for push in self.emit_notes(batch, rendered, done, sink) {
                sink.push(push);
            }
        }
    }

    /// Renders `notes` with every note their embeds read. A scan of the text finds most
    /// of them first; the render reports any it missed, which load before that note
    /// renders again. So a note renders the same in any batch.
    fn render_notes<'p>(
        &self,
        notes: &[(&'p str, Vec<u8>)],
        fresh: &HashMap<String, Fresh>,
        source: &mut impl Source,
    ) -> Vec<(&'p str, Body)> {
        let mut load = |target: &str| {
            notes
                .iter()
                .find(|(p, _)| *p == target)
                .map(|(_, b)| b.clone())
                .or_else(|| fresh.get(target).and_then(|f| f.bytes.clone()))
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

    /// A note's frontmatter title, else its file stem. An html page's file stem.
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

    /// Reads a path's current state into `entries`, and a note into `fresh`: its metadata,
    /// and its bytes while `kept` stays within `FRESH_MAX`. Returns true for a published
    /// path that is mid-save, which keeps its page as it is.
    fn probe(
        &mut self,
        path: &str,
        source: &mut impl Source,
        fresh: &mut HashMap<String, Fresh>,
        kept: &mut usize,
        skipped: &mut Vec<Skipped>,
    ) -> bool {
        let Some(kind) = classify(path, &self.exclude) else {
            // Excluded since it was listed, or never publishable.
            self.relist |= self.entries.remove(path).is_some();
            return false;
        };
        let result = match kind {
            // A search row carries the modified time, which a save can move alone.
            Kind::Note => source.note(path).map(|note| {
                self.dirty_shards.insert(indexes::shard(path));
                self.modified.insert(path.to_string(), note.modified);
                let meta = front::meta(&String::from_utf8_lossy(&note.bytes));
                *kept += note.bytes.len();
                let bytes = (*kept <= FRESH_MAX).then_some(note.bytes);
                fresh.insert(path.to_string(), Fresh { meta, bytes });
            }),
            Kind::Html | Kind::Asset => source.file(path).map(|modified| {
                if kind == Kind::Html {
                    self.modified.insert(path.to_string(), modified);
                }
            }),
        };
        match result {
            Ok(()) => {
                self.relist |= self.entries.insert(path.to_string(), kind).is_none();
            }
            // An unpublished path has nothing to keep; its save adds it.
            Err(ReadError::MidSave) if self.winners.contains(path) => return true,
            Err(ReadError::Missing | ReadError::MidSave) => {
                self.relist |= self.entries.remove(path).is_some();
            }
            Err(ReadError::Skip(reason)) => {
                self.relist |= self.entries.remove(path).is_some();
                skipped.push(Skipped {
                    path: PathBuf::from(path),
                    reason,
                });
            }
        }
        false
    }
}
