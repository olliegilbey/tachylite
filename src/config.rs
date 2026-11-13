//! `tachylite.toml`: optional settings in the vault root.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use glob::{MatchOptions, Pattern};
use toml::de::{DeTable, DeValue};

use crate::Error;
use crate::vault::{ReadError, SkipReason, VaultDir};

pub const FILE: &str = "tachylite.toml";

/// The top-level keys the design names. `daily`, `pins` and `title` are read by later slices.
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
                    g.hit.store(true, Ordering::Relaxed);
                    excluded = true;
                }
            }
        }
        excluded
    }

    /// The patterns, as written, that have matched no path so far. After a full scan, each one
    /// is likely a typo. The scan enters no excluded folder, so a pattern inside one counts
    /// as unmatched too.
    pub fn unmatched(&self) -> Vec<&str> {
        self.globs
            .iter()
            .filter(|g| !g.hit.load(Ordering::Relaxed))
            .map(|g| g.text.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub exclude: Exclude,
    /// The names the server answers besides `127.0.0.1` and `localhost`. `None` answers any.
    pub hosts: Option<Vec<String>>,
}

/// Reads `tachylite.toml` from the vault root. A missing file is the default config.
pub fn load(vault: &VaultDir) -> Result<Config, Error> {
    reload(vault).map(Option::unwrap_or_default)
}

/// Reads the config like `load`, but `None` when the file is missing or mid-save. A running
/// server keeps its previous config then: a missing config would publish what it excludes.
pub fn reload(vault: &VaultDir) -> Result<Option<Config>, Error> {
    let bytes = match vault.read(FILE) {
        Ok(bytes) => bytes,
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
    })
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
