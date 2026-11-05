//! Link resolution in the spec's order: relative, from the root, then the nearest bare name.
//! Matching ignores case, and a note's `.md` is optional.

use std::collections::HashMap;

/// The published paths, bucketed for lookup by folded path and by basename key.
#[derive(Debug, Clone, Default)]
pub struct Index {
    by_path: HashMap<String, Vec<String>>,
    by_key: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Found(String),
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
        add(&mut self.by_path, fold(path), path);
        add(&mut self.by_key, key(path), path);
    }

    pub fn remove(&mut self, path: &str) {
        drop_from(&mut self.by_path, fold(path), path);
        drop_from(&mut self.by_key, key(path), path);
    }
}

fn add(map: &mut HashMap<String, Vec<String>>, k: String, path: &str) {
    let bucket = map.entry(k).or_default();
    if !bucket.iter().any(|p| p == path) {
        bucket.push(path.to_string());
    }
}

fn drop_from(map: &mut HashMap<String, Vec<String>>, k: String, path: &str) {
    if let Some(bucket) = map.get_mut(&k) {
        bucket.retain(|p| p != path);
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
        let base = if relative { folder(from) } else { Vec::new() };
        let Some(path) = normalize(base, target) else {
            return Resolved::Missing;
        };
        return match index.by_path.get(&fold(&path)).map(Vec::as_slice) {
            None | Some([]) => Resolved::Missing,
            Some([one]) => Resolved::Found(one.clone()),
            Some(many) => Resolved::Ambiguous(many.len()),
        };
    }
    let Some(candidates) = index.by_key.get(&key(target)) else {
        return Resolved::Missing;
    };
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

/// Applies `target`'s segments to `base`. `None` when it climbs above the root or names no file.
fn normalize<'a>(mut base: Vec<&'a str>, target: &'a str) -> Option<String> {
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
