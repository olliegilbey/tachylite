//! Link resolution in the spec's order: relative, from the root then from the linking note,
//! the nearest bare name, then the nearest alias. Matching ignores case, and a note's `.md`
//! is optional.

use std::collections::HashMap;
use std::sync::Arc;

/// The published paths, bucketed for lookup by folded path and by basename key. Each path is
/// one shared allocation, so the link graph can hold many references to it cheaply.
#[derive(Debug, Clone, Default)]
pub struct Index {
    by_path: HashMap<String, Vec<Arc<str>>>,
    by_key: HashMap<String, Vec<Arc<str>>>,
    /// The paths with each alias, by the alias's key.
    by_alias: HashMap<String, Vec<Arc<str>>>,
    /// Each path's aliases, as written.
    aliases: HashMap<Arc<str>, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Found(Arc<str>),
    Missing,
    /// This many published paths tie, so the link is broken.
    Ambiguous(usize),
}

impl Index {
    pub fn new<'a>(paths: impl IntoIterator<Item = &'a str>) -> Index {
        let mut index = Index::default();
        for path in paths {
            index.insert(path);
        }
        index
    }

    pub fn insert(&mut self, path: &str) {
        if self.get(path).is_some() {
            return;
        }
        let shared: Arc<str> = Arc::from(path);
        self.by_path
            .entry(fold(path))
            .or_default()
            .push(Arc::clone(&shared));
        self.by_key.entry(key(path)).or_default().push(shared);
    }

    pub fn remove(&mut self, path: &str) {
        self.set_aliases(path, &[]);
        drop_from(&mut self.by_path, fold(path), path);
        drop_from(&mut self.by_key, key(path), path);
    }

    /// The shared copy of a published path.
    pub fn get(&self, path: &str) -> Option<Arc<str>> {
        self.by_path
            .get(&fold(path))?
            .iter()
            .find(|p| ***p == *path)
            .cloned()
    }

    /// Replaces a published path's aliases. A path not in the index takes none.
    pub fn set_aliases(&mut self, path: &str, aliases: &[String]) {
        let Some(shared) = self.get(path) else {
            return;
        };
        for old in self.aliases.remove(path).unwrap_or_default() {
            drop_from(&mut self.by_alias, fold(&old), path);
        }
        for alias in aliases {
            let owners = self.by_alias.entry(fold(alias)).or_default();
            if !owners.iter().any(|p| **p == *path) {
                owners.push(Arc::clone(&shared));
            }
        }
        if !aliases.is_empty() {
            self.aliases.insert(shared, aliases.to_vec());
        }
    }

    pub fn aliases(&self, path: &str) -> &[String] {
        self.aliases.get(path).map_or(&[], Vec::as_slice)
    }

    /// The published paths with an alias whose key is `key`.
    pub fn alias_bucket(&self, key: &str) -> &[Arc<str>] {
        self.by_alias.get(key).map_or(&[], Vec::as_slice)
    }

    /// The published paths whose key is `key`.
    pub fn bucket(&self, key: &str) -> &[Arc<str>] {
        self.by_key.get(key).map_or(&[], Vec::as_slice)
    }
}

fn drop_from(map: &mut HashMap<String, Vec<Arc<str>>>, k: String, path: &str) {
    if let Some(bucket) = map.get_mut(&k) {
        bucket.retain(|p| **p != *path);
        if bucket.is_empty() {
            map.remove(&k);
        }
    }
}

/// Lowercase, with a trailing `.md` dropped.
fn fold(s: &str) -> String {
    let lower = s.to_lowercase();
    match lower.strip_suffix(".md") {
        Some(stem) => stem.to_string(),
        None => lower,
    }
}

/// The lowercase basename with `.md` dropped: the bucket a bare link or a path falls in.
pub fn key(target: &str) -> String {
    fold(target.rsplit('/').next().unwrap_or(target))
}

/// Splits `note#heading` into the path and the heading.
pub fn split(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once('#') {
        Some((path, heading)) => (path, Some(heading)),
        None => (raw, None),
    }
}

/// Resolves `target`, a link without its label or heading, from the note at `from`.
pub fn resolve(index: &Index, from: &str, target: &str) -> Resolved {
    if target.is_empty() {
        return Resolved::Missing;
    }
    let relative = target.starts_with("./") || target.starts_with("../");
    if relative || target.contains('/') {
        let at = |base| {
            let path = normalize(base, target)?;
            match index.by_path.get(&fold(&path)).map(Vec::as_slice) {
                None | Some([]) => None,
                Some([one]) => Some(Resolved::Found(one.clone())),
                Some(many) => Some(Resolved::Ambiguous(many.len())),
            }
        };
        // Obsidian's relative format writes a path down from the note without `./`.
        let found = if relative {
            at(folder(from))
        } else {
            at(Vec::new()).or_else(|| at(folder(from)))
        };
        return found.unwrap_or(Resolved::Missing);
    }
    let mut candidates = index.bucket(&key(target));
    if candidates.is_empty() {
        candidates = index.alias_bucket(&key(target));
    }
    match candidates {
        [] => return Resolved::Missing,
        [one] => return Resolved::Found(Arc::clone(one)),
        _ => {}
    }
    let here = folder(from);
    let steps = |p: &str| distance(&here, &folder(p));
    let Some(nearest) = candidates.iter().map(|p| steps(p)).min() else {
        return Resolved::Missing;
    };
    let mut tied = candidates.iter().filter(|p| steps(p) == nearest);
    match (tied.next(), tied.count()) {
        (Some(one), 0) => Resolved::Found(one.clone()),
        (Some(_), more) => Resolved::Ambiguous(more + 1),
        (None, _) => Resolved::Missing,
    }
}

/// The folders above a vault path.
fn folder(path: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts
}

/// Folder steps up to the common folder, plus steps down from it.
fn distance(a: &[&str], b: &[&str]) -> usize {
    let common = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    a.len() + b.len() - 2 * common
}

/// Applies `target`'s segments to `base`. `None` when it climbs above the root or names a
/// folder, so a found path always has the target's last segment as its basename.
fn normalize<'a>(mut base: Vec<&'a str>, target: &'a str) -> Option<String> {
    if matches!(target.rsplit('/').next(), Some("" | "." | "..")) {
        return None;
    }
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                base.pop()?;
            }
            name => base.push(name),
        }
    }
    (!base.is_empty()).then(|| base.join("/"))
}
