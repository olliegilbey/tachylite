//! `tachylite.toml`: optional settings in the vault root.

use glob::{MatchOptions, Pattern};
use toml::de::{DeTable, DeValue};

use crate::Error;
use crate::vault::{ReadError, VaultDir};

pub const FILE: &str = "tachylite.toml";

const MATCH: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// What the vault never publishes: the built-in names, plus the config's globs.
#[derive(Debug, Clone, Default)]
pub struct Exclude {
    globs: Vec<Pattern>,
}

impl Exclude {
    pub fn new(globs: &[String]) -> Result<Exclude, String> {
        let globs = globs
            .iter()
            .map(|g| Pattern::new(g).map_err(|e| format!("exclude {g:?}: {}", e.msg)))
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
        prefixes
            .chain([rel])
            .any(|p| self.globs.iter().any(|g| g.matches_with(p, MATCH)))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub exclude: Exclude,
}

/// Reads `tachylite.toml` from the vault root. A missing file is the default config.
pub fn load(vault: &VaultDir) -> Result<Config, Error> {
    let bytes = match vault.read(FILE) {
        Ok(bytes) => bytes,
        Err(ReadError::Missing) => return Ok(Config::default()),
        Err(ReadError::Skip(reason)) => return Err(config_error(format!("{reason:?}"))),
        Err(ReadError::MidSave) => return Ok(Config::default()),
    };
    let text = String::from_utf8(bytes).map_err(|_| config_error("not UTF-8".into()))?;
    parse(&text).map_err(config_error)
}

fn config_error(message: String) -> Error {
    Error::Config { message }
}

/// Parses the config text. Unknown keys are ignored; later slices read them.
pub fn parse(text: &str) -> Result<Config, String> {
    let table = DeTable::parse(text).map_err(|e| {
        let line = e.span().map_or(1, |s| {
            text[..s.start.min(text.len())].matches('\n').count() + 1
        });
        format!("line {line}: {}", e.message())
    })?;
    let mut globs = Vec::new();
    if let Some(value) = table.get_ref().get("exclude") {
        let DeValue::Array(items) = value.get_ref() else {
            return Err("exclude must be a list of strings".into());
        };
        for item in items {
            let DeValue::String(glob) = item.get_ref() else {
                return Err("exclude must be a list of strings".into());
            };
            globs.push(glob.to_string());
        }
    }
    Ok(Config {
        exclude: Exclude::new(&globs)?,
    })
}
