//! `tachylite.toml`: optional settings in the vault root.

use std::fmt;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use glob::{MatchOptions, Pattern};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use toml::de::{DeTable, DeValue};

use crate::Error;
use crate::vault::{ReadError, SkipReason, VaultDir};

pub const FILE: &str = "tachylite.toml";

/// Where Obsidian keeps its daily-notes settings. It is read, never published.
pub const DAILY_NOTES: &str = ".obsidian/daily-notes.json";

/// Obsidian's default daily-note format.
const DAILY_FORMAT: &str = "YYYY-MM-DD";

/// The top-level keys the design names.
const KEYS: [&str; 5] = ["exclude", "daily", "pins", "title", "hosts"];

// The publish list ignores case, so an exclusion must too, or `Day.MD` slips past `*.md`.
const MATCH: MatchOptions = MatchOptions {
    case_sensitive: false,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// Why a config was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Unreadable(SkipReason),
    /// A folder or other non-file at the config's name.
    NotFile,
    NotUtf8,
    Syntax {
        line: usize,
        message: String,
    },
    UnknownKey(String),
    /// `exclude` sits inside a table, where it would be ignored.
    Nested {
        table: String,
    },
    NotStrings(&'static str),
    NotString(&'static str),
    /// A key inside `daily` other than `folder` and `format`.
    DailyKey(String),
    DailyTable,
    BadGlob {
        glob: String,
        message: String,
    },
    EmptyGlob(String),
    /// A pattern with a `..` segment, which no vault path has.
    ParentGlob(String),
    /// A pattern that starts or ends with whitespace, almost always a typo.
    SpacedGlob(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Unreadable(reason) => f.write_str(&crate::report::reason(*reason)),
            ConfigError::NotFile => f.write_str("not a file"),
            ConfigError::NotUtf8 => f.write_str("not UTF-8"),
            ConfigError::Syntax { line, message } => write!(f, "line {line}: {message}"),
            ConfigError::UnknownKey(key) => write!(
                f,
                "unknown key {key:?}; the keys are exclude, daily, pins, title and hosts"
            ),
            ConfigError::Nested { table } => write!(
                f,
                "exclude sits inside [{table}], where it does nothing; move it above the first table"
            ),
            ConfigError::NotStrings(key) => write!(f, "{key} must be a list of strings"),
            ConfigError::NotString(key) => write!(f, "{key} must be a string"),
            ConfigError::DailyTable => f.write_str(
                "daily must be a table, as daily = { folder = \"journal\", format = \"YYYY-MM-DD\" }",
            ),
            ConfigError::DailyKey(key) => write!(
                f,
                "unknown key daily.{key}; daily takes folder and format, as strings"
            ),
            ConfigError::BadGlob { glob, message } => write!(f, "exclude {glob:?}: {message}"),
            ConfigError::EmptyGlob(glob) => {
                write!(f, "exclude {glob:?}: names no file or folder")
            }
            ConfigError::SpacedGlob(glob) => {
                write!(f, "exclude {glob:?}: starts or ends with a space")
            }
            ConfigError::ParentGlob(glob) => {
                write!(
                    f,
                    "exclude {glob:?}: a pattern starts at the vault root and cannot use .."
                )
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Glob {
    text: String,
    pattern: Pattern,
    /// Set once the pattern matches a path, so a pattern that matches nothing can be named.
    hit: Arc<AtomicBool>,
}

/// What the vault never publishes: the built-in names, plus the config's globs.
#[derive(Debug, Clone, Default)]
pub struct Exclude {
    globs: Vec<Glob>,
}

impl Exclude {
    pub fn new(globs: &[String]) -> Result<Exclude, ConfigError> {
        let globs = globs
            .iter()
            .map(|g| {
                if g.trim() != g {
                    return Err(ConfigError::SpacedGlob(g.clone()));
                }
                // Gitignore habits: `/private`, `./private` and `private/` all name the folder.
                let bare = g
                    .strip_prefix("./")
                    .or_else(|| g.strip_prefix('/'))
                    .unwrap_or(g);
                let bare = bare.strip_suffix('/').unwrap_or(bare);
                if bare.is_empty() {
                    return Err(ConfigError::EmptyGlob(g.clone()));
                }
                if bare.split('/').any(|s| s == "..") {
                    return Err(ConfigError::ParentGlob(g.clone()));
                }
                let pattern = Pattern::new(bare).map_err(|e| ConfigError::BadGlob {
                    glob: g.clone(),
                    message: e.msg.to_string(),
                })?;
                Ok(Glob {
                    text: g.clone(),
                    pattern,
                    hit: Arc::default(),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Exclude { globs })
    }

    /// A file or folder name that is never published: dot names, `target` and `node_modules`.
    pub fn segment(name: &[u8]) -> bool {
        name.starts_with(b".") || name == b"target" || name == b"node_modules"
    }

    /// True when the path, or any folder above it, is excluded.
    pub fn path(&self, rel: &str) -> bool {
        self.hides(rel, true)
    }

    /// Like `path`, but marks no pattern as matched: for a path named in config, which
    /// may name no file.
    pub fn covers(&self, rel: &str) -> bool {
        self.hides(rel, false)
    }

    fn hides(&self, rel: &str, mark: bool) -> bool {
        if rel.split('/').any(|s| Exclude::segment(s.as_bytes())) {
            return true;
        }
        let prefixes = rel.match_indices('/').map(|(i, _)| &rel[..i]);
        let mut excluded = false;
        // Every glob is tried on every prefix, with no early exit, so a correct pattern
        // that overlaps another is not reported as unmatched.
        for p in prefixes.chain([rel]) {
            for g in &self.globs {
                if g.pattern.matches_with(p, MATCH) {
                    if !mark {
                        return true;
                    }
                    g.hit.store(true, Ordering::Relaxed);
                    excluded = true;
                }
            }
        }
        excluded
    }

    /// The patterns, as written, that have matched no path so far. After a full scan, each one
    /// is likely a typo. The scan enters no excluded folder, so a pattern inside one is left
    /// out: it matched nothing because it had nothing left to match.
    pub fn unmatched(&self) -> Vec<&str> {
        self.globs
            .iter()
            .filter(|g| !g.hit.load(Ordering::Relaxed) && !self.inside_excluded(g))
            .map(|g| g.text.as_str())
            .collect()
    }

    /// Whether a folder in the pattern's own text is excluded, as `private` is for
    /// `private/draft.md`. Unlike `path`, it marks no pattern as matched.
    fn inside_excluded(&self, glob: &Glob) -> bool {
        let text = glob.pattern.as_str();
        text.match_indices('/').any(|(i, _)| {
            let folder = &text[..i];
            folder.split('/').any(|s| Exclude::segment(s.as_bytes()))
                || self
                    .globs
                    .iter()
                    .any(|g| g.pattern.matches_with(folder, MATCH))
        })
    }
}

/// Equal when both hold the same patterns, as written, in the same order.
impl PartialEq for Exclude {
    fn eq(&self, other: &Exclude) -> bool {
        let ours = self.globs.iter().map(|g| &g.text);
        ours.eq(other.globs.iter().map(|g| &g.text))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub exclude: Exclude,
    /// The names the server answers besides `127.0.0.1` and `localhost`. `None` answers any.
    pub hosts: Option<Vec<String>>,
    pub nav: Nav,
}

/// What the page header shows. Every page holds it, so it comes from config alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nav {
    /// The site name.
    pub title: Option<String>,
    /// Each a vault path, as written, with an optional `|label`.
    pub pins: Vec<String>,
    /// Overrides Obsidian's daily-notes settings.
    pub daily: Option<Daily>,
}

/// Where daily notes live and how they are named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daily {
    /// The folder, from the vault root, with no leading or trailing `/`.
    pub folder: String,
    /// A Moment.js format, as Obsidian writes it.
    pub format: String,
}

impl Daily {
    fn new(folder: &str, format: &str) -> Daily {
        let format = if format.is_empty() {
            DAILY_FORMAT
        } else {
            format
        };
        Daily {
            folder: folder.trim_matches('/').to_string(),
            format: format.to_string(),
        }
    }
}

/// Reads Obsidian's daily-notes settings, or `None` when they are missing or unreadable.
pub fn obsidian_daily(vault: &VaultDir) -> Option<Daily> {
    let bytes = vault.read(DAILY_NOTES).ok()?;
    let pairs = json_strings(std::str::from_utf8(&bytes).ok()?)?;
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v.as_str())
    };
    Some(Daily::new(get("folder"), get("format")))
}

/// The string members of a flat JSON object, or `None` if the text is not one. Other values
/// are skipped. Obsidian writes this file; a JSON crate would cost more than the parse.
fn json_strings(text: &str) -> Option<Vec<(String, String)>> {
    let mut chars = text.trim().strip_prefix('{')?.chars().peekable();
    let mut pairs = Vec::new();
    let skip_ws = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
    };
    let string = |chars: &mut std::iter::Peekable<std::str::Chars>| -> Option<String> {
        let mut out = String::new();
        loop {
            match chars.next()? {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
    };
    loop {
        skip_ws(&mut chars);
        match chars.next()? {
            '}' => return Some(pairs),
            ',' => continue,
            '"' => {}
            _ => return None,
        }
        let key = string(&mut chars)?;
        skip_ws(&mut chars);
        chars.next().filter(|c| *c == ':')?;
        skip_ws(&mut chars);
        if chars.next_if_eq(&'"').is_some() {
            pairs.push((key, string(&mut chars)?));
            continue;
        }
        // Any other value: skip to the comma or brace that ends it, outside strings.
        let mut depth = 0usize;
        loop {
            match chars.peek()? {
                ',' | '}' if depth == 0 => break,
                '{' | '[' => depth += 1,
                '}' | ']' => depth = depth.checked_sub(1)?,
                '"' => {
                    chars.next();
                    string(&mut chars)?;
                    continue;
                }
                _ => {}
            }
            chars.next();
        }
    }
}

/// Reads `tachylite.toml` from the vault root. A missing file is the default config.
pub fn load(vault: &VaultDir) -> Result<Config, Error> {
    // A name in another case is no config, and its exclusions would protect nothing.
    // An unreadable root is the scan's to report.
    if let Ok(items) = std::fs::read_dir(vault.root()) {
        let misnamed: Vec<String> = items
            .flatten()
            .filter_map(|item| item.file_name().into_string().ok())
            .filter(|name| name != FILE && name.eq_ignore_ascii_case(FILE))
            .filter(|name| !same_file(vault, name))
            .collect();
        crate::report::misnamed(&misnamed);
    }
    settle(vault)?;
    reload(vault).map(Option::unwrap_or_default)
}

/// Waits while the config may be mid-save with no event yet to show it. A save that truncated
/// it before this process watched it leaves it empty and just modified. Returns at a save's
/// close, or once the hold passes with no change to it. So a config meant to be empty waits
/// up to the hold, and a writer stalled for longer is read as it stands.
fn settle(vault: &VaultDir) -> Result<(), Error> {
    let hold = crate::vault::hold();
    let Some(left) = fresh_empty(vault, hold) else {
        return Ok(());
    };
    let watch_err = |e: rustix::io::Errno| Error::Watch {
        path: vault.root().to_path_buf(),
        source: e.into(),
    };
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).map_err(watch_err)?;
    let mask = WatchFlags::MODIFY
        | WatchFlags::CLOSE_WRITE
        | WatchFlags::MOVED_TO
        | WatchFlags::MOVED_FROM
        | WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::ONLYDIR;
    inotify::add_watch(&fd, vault.root(), mask).map_err(watch_err)?;
    // Printed once the watch exists, so a save that closes after this line is seen.
    eprintln!("{FILE} is empty and was just modified; waiting for its save to finish");
    let mut deadline = Instant::now() + left;
    let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
    loop {
        let Ok(timeout) = Timespec::try_from(deadline.saturating_duration_since(Instant::now()))
        else {
            return Ok(());
        };
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Ok(()),
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(watch_err(e)),
        }
        let mut reader = inotify::Reader::new(&fd, &mut buf);
        loop {
            match reader.next() {
                Ok(e) if e.events().contains(ReadFlags::QUEUE_OVERFLOW) => return Ok(()),
                Ok(e)
                    if e.file_name()
                        .is_some_and(|n| n.to_bytes() == FILE.as_bytes()) =>
                {
                    if e.events()
                        .intersects(ReadFlags::CLOSE_WRITE | ReadFlags::MOVED_TO)
                    {
                        return Ok(());
                    }
                    // A write, or a remove a create may follow: the save goes on.
                    deadline = Instant::now() + hold;
                }
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::AGAIN) => break,
                Err(e) => return Err(watch_err(e)),
            }
            if reader.is_buffer_empty() {
                break;
            }
        }
    }
}

/// How much longer an empty config counts as mid-save, or `None` if it is not empty or
/// not just modified. A modified time in the future counts as now.
fn fresh_empty(vault: &VaultDir, hold: Duration) -> Option<Duration> {
    let meta = vault.root().join(FILE).symlink_metadata().ok()?;
    if !meta.is_file() || meta.len() != 0 {
        return None;
    }
    let age = SystemTime::now()
        .duration_since(meta.modified().ok()?)
        .unwrap_or_default();
    hold.checked_sub(age).filter(|left| !left.is_zero())
}

/// Whether `name` is the config itself, as on a file system that folds case.
fn same_file(vault: &VaultDir, name: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let ino = |n: &str| {
        vault
            .root()
            .join(n)
            .symlink_metadata()
            .map(|m| (m.dev(), m.ino()))
    };
    matches!((ino(name), ino(FILE)), (Ok(a), Ok(b)) if a == b)
}

/// Reads the config like `load`, but `None` when the file is missing or mid-save. A running
/// server keeps its previous config then: a missing config would publish what it excludes.
pub fn reload(vault: &VaultDir) -> Result<Option<Config>, Error> {
    let bytes = match vault.read(FILE) {
        Ok(bytes) => bytes,
        // A folder of the config's name reads as missing, and would publish what the
        // config excludes.
        Err(ReadError::Missing) if vault.root().join(FILE).symlink_metadata().is_ok() => {
            return Err(Error::Config(ConfigError::NotFile));
        }
        Err(ReadError::Missing | ReadError::MidSave) => return Ok(None),
        Err(ReadError::Skip(reason)) => return Err(Error::Config(ConfigError::Unreadable(reason))),
    };
    let text = String::from_utf8(bytes).map_err(|_| Error::Config(ConfigError::NotUtf8))?;
    parse(&text).map(Some).map_err(Error::Config)
}

/// Parses the config text. An unknown key is refused: a typo would publish what it meant
/// to exclude.
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let table = DeTable::parse(text).map_err(|e| ConfigError::Syntax {
        line: e.span().map_or(1, |s| {
            text[..s.start.min(text.len())].matches('\n').count() + 1
        }),
        message: e.message().to_string(),
    })?;
    let table = table.get_ref();
    for (key, value) in table {
        let key = key.get_ref().as_ref();
        if !KEYS.contains(&key) {
            return Err(ConfigError::UnknownKey(key.to_string()));
        }
        nested_exclude(key, value.get_ref())?;
    }
    let exclude = strings(table, "exclude")?.unwrap_or_default();
    Ok(Config {
        exclude: Exclude::new(&exclude)?,
        hosts: strings(table, "hosts")?,
        nav: Nav {
            title: string(table, "title", "title")?,
            pins: strings(table, "pins")?.unwrap_or_default(),
            daily: daily(table)?,
        },
    })
}

/// The string at `key`, or `None` when absent. `name` names the key in an error.
fn string(table: &DeTable, key: &str, name: &'static str) -> Result<Option<String>, ConfigError> {
    match table.get(key).map(|v| v.get_ref()) {
        None => Ok(None),
        Some(DeValue::String(s)) => Ok(Some(s.to_string())),
        Some(_) => Err(ConfigError::NotString(name)),
    }
}

/// `daily = { folder = "...", format = "..." }`. Either key may be left out.
fn daily(table: &DeTable) -> Result<Option<Daily>, ConfigError> {
    let Some(value) = table.get("daily") else {
        return Ok(None);
    };
    let DeValue::Table(daily) = value.get_ref() else {
        return Err(ConfigError::DailyTable);
    };
    for key in daily.keys() {
        let key = key.get_ref().as_ref();
        if key != "folder" && key != "format" {
            return Err(ConfigError::DailyKey(key.to_string()));
        }
    }
    let folder = string(daily, "folder", "daily.folder")?.unwrap_or_default();
    let format = string(daily, "format", "daily.format")?.unwrap_or_default();
    Ok(Some(Daily::new(&folder, &format)))
}

/// The value at `key` as a list of strings, or `None` when the key is absent.
fn strings(table: &DeTable, key: &'static str) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let DeValue::Array(items) = value.get_ref() else {
        return Err(ConfigError::NotStrings(key));
    };
    items
        .iter()
        .map(|item| match item.get_ref() {
            DeValue::String(s) => Ok(s.to_string()),
            _ => Err(ConfigError::NotStrings(key)),
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// Refuses an `exclude` key anywhere inside the table at `path`: written after a `[table]`
/// header, TOML files it there, and the vault would publish what it names.
fn nested_exclude(path: &str, value: &DeValue) -> Result<(), ConfigError> {
    match value {
        DeValue::Table(table) => {
            for (key, value) in table {
                let key = key.get_ref().as_ref();
                if key == "exclude" {
                    return Err(ConfigError::Nested {
                        table: path.to_string(),
                    });
                }
                nested_exclude(&format!("{path}.{key}"), value.get_ref())?;
            }
            Ok(())
        }
        DeValue::Array(items) => items
            .iter()
            .try_for_each(|item| nested_exclude(path, item.get_ref())),
        _ => Ok(()),
    }
}
