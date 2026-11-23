//! The manifest's recent history, so a sync fetches only what changed since the one it holds.
//!
//! Each step is a diff of two whole manifests, so a step is right by construction: no change
//! the site makes can be missed.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

/// The bytes of steps the log keeps. On the budget vault one saved note makes a step of
/// about 250 bytes, so the log reaches back about 250 saves.
pub const LIMIT: usize = 64 * 1024;

/// What changed from one manifest to the next.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// The tag of the manifest it starts from.
    from: String,
    /// The pages new or at a new version, with that version.
    pages: Vec<(Box<str>, Box<str>)>,
    gone: Vec<Box<str>>,
    files: Vec<Box<str>>,
    unfiled: Vec<Box<str>>,
}

impl Step {
    /// About what the step costs to hold and to send.
    fn size(&self) -> usize {
        let urls = self.gone.iter().chain(&self.files).chain(&self.unfiled);
        self.from.len()
            + self
                .pages
                .iter()
                .map(|(u, v)| u.len() + v.len() + 6)
                .sum::<usize>()
            + urls.map(|u| u.len() + 3).sum::<usize>()
    }
}

/// The current manifest and the steps that lead to it, oldest first.
#[derive(Debug, Default)]
pub struct Log {
    text: Arc<String>,
    tag: String,
    steps: VecDeque<Step>,
    size: usize,
}

/// A manifest's pages and its files, as the text of each list.
fn parse(json: &str) -> Option<(&str, &str)> {
    let body = json.strip_prefix("{\"pages\":{")?.strip_suffix("]}")?;
    // From the end: the files list is the shorter.
    body.rsplit_once("},\"files\":[")
}

/// The step from `old` to `new`, or `None` if either is not a manifest.
pub fn diff(old: &str, new: &str) -> Option<Step> {
    let (old_pages, old_files) = parse(old)?;
    let (new_pages, new_files) = parse(new)?;
    let (pages, gone) = changes(old_pages, new_pages, true)?;
    let (files, unfiled) = changes(old_files, new_files, false)?;
    let boxed = |urls: Vec<&str>| urls.into_iter().map(Box::from).collect();
    Some(Step {
        from: String::new(),
        pages: pages
            .into_iter()
            .map(|(u, v)| (u.into(), v.into()))
            .collect(),
        gone: boxed(gone),
        files: boxed(files.into_iter().map(|(u, _)| u).collect()),
        unfiled: boxed(unfiled),
    })
}

/// The entry at `at` of a list, `"<url>":"<version>"` or `"<url>"`, and where the next one
/// starts. URLs are percent-encoded and versions hex, so no quote falls inside either.
fn entry(list: &str, at: usize, versioned: bool) -> Option<((&str, &str), usize)> {
    let rest = list.get(at..)?.strip_prefix('"')?;
    let (url, mut rest) = rest.split_once('"')?;
    let mut version = "";
    if versioned {
        (version, rest) = rest.strip_prefix(":\"")?.split_once('"')?;
    }
    let next = list.len() - rest.len();
    Some(((url, version), next + usize::from(rest.starts_with(','))))
}

/// The entries new or at a new version, and the URLs gone.
type Changes<'a> = (Vec<(&'a str, &'a str)>, Vec<&'a str>);

/// How many bytes `a` and `b` share from their start.
fn common(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 64 <= n && a[i..i + 64] == b[i..i + 64] {
        i += 64;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// The entries of `new` that are new or at a new version, and the URLs of `old` that `new`
/// lacks. Two manifests list pages in the same order and mostly the same bytes, so the walk
/// skips each run they share, and hashes only the entries out of line, which a page added
/// or removed puts there for a while. `None` if a list is not one.
fn changes<'a>(old: &'a str, new: &'a str, versioned: bool) -> Option<Changes<'a>> {
    let (mut i, mut j) = (0, 0);
    let mut changed = Vec::new();
    // Entries out of line, each kept until its match on the other side comes.
    let mut was: HashMap<&str, &str> = HashMap::new();
    let mut is: HashMap<&str, &str> = HashMap::new();
    let mut resolve = |url, then, now| {
        if then != now {
            changed.push((url, now));
        }
    };
    loop {
        // A shared run is whole entries alike on both sides; skip to the start of the last.
        let run = common(&old.as_bytes()[i..], &new.as_bytes()[j..]);
        let back = old[i..i + run].rfind("\",\"").map_or(0, |k| k + 2);
        (i, j) = (i + back, j + back);
        let o = (i < old.len())
            .then(|| entry(old, i, versioned))
            .map(|e| e.ok_or(()));
        let n = (j < new.len())
            .then(|| entry(new, j, versioned))
            .map(|e| e.ok_or(()));
        let (o, n) = (o.transpose().ok()?, n.transpose().ok()?);
        match (o, n) {
            (Some(((u, then), a)), Some(((v, now), b))) if u == v => {
                resolve(v, then, now);
                (i, j) = (a, b);
            }
            (_, Some(((v, now), b))) if was.contains_key(v) => {
                resolve(v, was.remove(v).unwrap_or_default(), now);
                j = b;
            }
            (Some(((u, then), a)), _) if is.contains_key(u) => {
                resolve(u, then, is.remove(u).unwrap_or_default());
                i = a;
            }
            (None, None) => break,
            (o, n) => {
                if let Some((e, a)) = o {
                    was.insert(e.0, e.1);
                    i = a;
                }
                if let Some((e, b)) = n {
                    is.insert(e.0, e.1);
                    j = b;
                }
            }
        }
    }
    // A URL only `new` has is new.
    changed.extend(is);
    Some((changed, was.into_keys().collect()))
}

impl Log {
    /// The current manifest and its tag.
    pub fn current(&self) -> (Arc<String>, &str) {
        (self.text.clone(), &self.tag)
    }

    /// Makes `text`, tagged `tag`, the current manifest, and keeps `step`, the diff to it
    /// from the last, within `limit` bytes of steps. The oldest steps go first. With no
    /// step, no earlier tag can be answered.
    pub fn record(&mut self, text: Arc<String>, tag: String, step: Option<Step>, limit: usize) {
        if tag == self.tag {
            return;
        }
        let from = std::mem::replace(&mut self.tag, tag);
        self.text = text;
        match step {
            Some(step) => {
                let step = Step { from, ..step };
                self.size += step.size();
                self.steps.push_back(step);
            }
            // The first manifest has no step before it.
            None => {
                self.steps.clear();
                self.size = 0;
            }
        }
        while self.size > limit
            && let Some(step) = self.steps.pop_front()
        {
            self.size -= step.size();
        }
    }

    /// The changes from the manifest tagged `since` to the current one, as JSON:
    /// `{"since":…,"pages":{…},"gone":[…],"files":[…],"unfiled":[…]}`. A later step wins.
    /// `None` when the log does not reach back to `since`, or when the changes would cost more
    /// than a quarter of the whole manifest. The current tag gets every list empty.
    pub fn since(&self, since: &str) -> Option<String> {
        let start = if since == self.tag {
            self.steps.len()
        } else {
            self.steps.iter().rposition(|s| s.from == since)?
        };
        let mut pages: BTreeMap<&str, Option<&str>> = BTreeMap::new();
        let mut files: BTreeMap<&str, bool> = BTreeMap::new();
        for step in self.steps.range(start..) {
            pages.extend(step.pages.iter().map(|(u, v)| (&**u, Some(&**v))));
            pages.extend(step.gone.iter().map(|u| (&**u, None)));
            files.extend(step.files.iter().map(|u| (&**u, true)));
            files.extend(step.unfiled.iter().map(|u| (&**u, false)));
        }
        // A tag is `W/"…"`, so its quotes are escaped.
        let mut json = format!(
            "{{\"since\":\"{}\",\"pages\":{{",
            since.replace('"', "\\\"")
        );
        let listed = pages.iter().filter_map(|(u, v)| Some((u, (*v)?)));
        for (i, (url, version)) in listed.enumerate() {
            let comma = if i > 0 { "," } else { "" };
            let _ = write!(json, "{comma}\"{url}\":\"{version}\"");
        }
        json.push('}');
        let lists = [
            (
                "gone",
                pages
                    .iter()
                    .filter(|(_, v)| v.is_none())
                    .map(|(u, _)| *u)
                    .collect(),
            ),
            (
                "files",
                files.iter().filter(|(_, f)| **f).map(|(u, _)| *u).collect(),
            ),
            (
                "unfiled",
                files
                    .iter()
                    .filter(|(_, f)| !**f)
                    .map(|(u, _)| *u)
                    .collect::<Vec<_>>(),
            ),
        ];
        for (name, urls) in lists {
            let _ = write!(json, ",\"{name}\":[");
            for (i, url) in urls.iter().enumerate() {
                let comma = if i > 0 { "," } else { "" };
                let _ = write!(json, "{comma}\"{url}\"");
            }
            json.push(']');
        }
        json.push('}');
        (start == self.steps.len() || json.len() * 4 <= self.text.len()).then_some(json)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn manifest(pages: &[(&str, &str)], files: &[&str]) -> Arc<String> {
        let pages: Vec<String> = pages
            .iter()
            .map(|(u, v)| format!("\"{u}\":\"{v}\""))
            .collect();
        let files: Vec<String> = files.iter().map(|u| format!("\"{u}\"")).collect();
        let pad: Vec<String> = (0..40).map(|i| format!("\"/pad{i}\":\"0\"")).collect();
        let pages = pad.into_iter().chain(pages).collect::<Vec<_>>().join(",");
        format!("{{\"pages\":{{{pages}}},\"files\":[{}]}}", files.join(",")).into()
    }

    /// A manifest from entries, in their order.
    fn listed(pages: &[(String, String)], files: &[String]) -> String {
        let pages: Vec<String> = pages
            .iter()
            .map(|(u, v)| format!("\"{u}\":\"{v}\""))
            .collect();
        let files: Vec<String> = files.iter().map(|u| format!("\"{u}\"")).collect();
        format!(
            "{{\"pages\":{{{}}},\"files\":[{}]}}",
            pages.join(","),
            files.join(",")
        )
    }

    /// Entries drawn from a few URLs that share prefixes, kept in order or shuffled.
    fn entries() -> impl proptest::strategy::Strategy<Value = (Vec<(String, String)>, Vec<String>)>
    {
        use proptest::prelude::*;
        let url = (0..30u8).prop_map(|i| format!("/n{}{i}", "/a".repeat(usize::from(i % 4))));
        let version = (0..3u8).prop_map(|v| format!("{v:08x}"));
        let pages = prop::collection::btree_map(url.clone(), version, 0..30);
        let files = prop::collection::btree_set(url, 0..10);
        (pages, files, any::<bool>()).prop_map(|(pages, files, shuffle)| {
            let mut pages: Vec<(String, String)> = pages.into_iter().collect();
            if shuffle {
                pages.reverse();
            }
            (pages, files.into_iter().collect())
        })
    }

    proptest::proptest! {
        #[test]
        fn a_diff_applied_to_the_old_manifest_gives_the_new((a, af) in entries(), (b, bf) in entries()) {
            let step = diff(&listed(&a, &af), &listed(&b, &bf)).unwrap();
            let mut pages: BTreeMap<String, String> = a.into_iter().collect();
            for (url, version) in &step.pages {
                proptest::prop_assert_ne!(pages.get(&**url).map(|v| &**v), Some(&**version));
            }
            for url in &step.gone {
                proptest::prop_assert!(pages.remove(&**url).is_some());
            }
            pages.extend(step.pages.iter().map(|(u, v)| (u.to_string(), v.to_string())));
            proptest::prop_assert_eq!(pages, b.into_iter().collect::<BTreeMap<_, _>>());
            let mut files: std::collections::BTreeSet<String> = af.into_iter().collect();
            for url in &step.unfiled {
                proptest::prop_assert!(files.remove(&**url));
            }
            files.extend(step.files.iter().map(|u| u.to_string()));
            proptest::prop_assert_eq!(files, bf.into_iter().collect());
        }
    }

    #[test]
    fn a_diff_lists_what_changed_and_what_went() {
        let old = manifest(&[("/a", "1"), ("/b", "1")], &["/p.png"]);
        let new = manifest(&[("/a", "2"), ("/c", "1")], &["/q.png"]);
        let step = diff(&old, &new).unwrap();
        assert_eq!(
            step.pages,
            [("/a".into(), "2".into()), ("/c".into(), "1".into())]
        );
        assert_eq!(step.gone, ["/b".into()]);
        assert_eq!(step.files, ["/q.png".into()]);
        assert_eq!(step.unfiled, ["/p.png".into()]);
    }

    #[test]
    fn an_empty_manifest_parses() {
        let step = diff("{\"pages\":{},\"files\":[]}", "{\"pages\":{},\"files\":[]}").unwrap();
        assert_eq!(step, Step::default());
    }

    #[test]
    fn a_manifest_back_to_an_earlier_one_answers_from_its_last_step() {
        let a = manifest(&[("/a", "1")], &[]);
        let b = manifest(&[("/a", "2")], &[]);
        let c = manifest(&[("/a", "3")], &[]);
        let mut log = Log::default();
        for (text, tag) in [(&a, "A"), (&b, "B"), (&a, "A"), (&c, "C")] {
            let step = diff(&log.current().0, text);
            log.record(text.clone(), tag.into(), step, LIMIT);
        }
        let delta = log.since("A").unwrap();
        assert!(delta.contains("\"/a\":\"3\""), "{delta}");
        assert_eq!(log.since("B").unwrap(), delta.replace("\"A\"", "\"B\""));
    }

    #[test]
    fn the_oldest_steps_go_past_the_limit() {
        let mut log = Log::default();
        log.record(manifest(&[("/a", "0")], &[]), "t0".into(), None, 50);
        for i in 1..10 {
            let text = manifest(&[("/a", &i.to_string())], &[]);
            let step = diff(&log.current().0, &text);
            log.record(text, format!("t{i}"), step, 50);
        }
        assert!(log.size <= 50);
        assert!(log.since("t0").is_none());
        assert!(log.since("t8").is_some());
    }
}
