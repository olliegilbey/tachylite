#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use proptest::prelude::*;
use tachylite::config::Exclude;
use tachylite::render::Broken;
use tachylite::site::{Change, Delta, Note, Push, Sink, Site, Source, Write};
use tachylite::vault::{Entry, ReadError, SkipReason, Skipped, classify};

/// An in-memory vault: path to bytes, and the modified times of the notes that set one.
/// Folders are implied by paths.
#[derive(Default, Clone)]
struct Mem(BTreeMap<String, Vec<u8>>, BTreeMap<String, i64>);

impl Source for Mem {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        let bytes = self.0.get(path).cloned().ok_or(ReadError::Missing)?;
        let modified = self.1.get(path).copied().unwrap_or_default();
        Ok(Note {
            bytes,
            modified,
            nanos: 0,
        })
    }
    fn file(&mut self, path: &str) -> Result<i64, ReadError> {
        let modified = self.1.get(path).copied().unwrap_or_default();
        self.0.get(path).map(|_| modified).ok_or(ReadError::Missing)
    }
}

impl Mem {
    fn with(files: &[(&str, &str)]) -> Mem {
        Mem(
            files
                .iter()
                .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
                .collect(),
            BTreeMap::new(),
        )
    }

    fn entries_under(&self, under: &str) -> Vec<Entry> {
        self.0
            .keys()
            .filter(|p| under.is_empty() || p.starts_with(&format!("{under}/")))
            .filter_map(|p| {
                classify(p, &Exclude::default()).map(|kind| Entry {
                    path: p.clone(),
                    kind,
                })
            })
            .collect()
    }
}

type Out = BTreeMap<String, Vec<u8>>;

fn fold(out: &mut Out, delta: &Delta, mem: &Mem) {
    for path in &delta.deletes {
        out.remove(path);
    }
    for write in &delta.writes {
        match write {
            Write::Page { out: path, html } => {
                out.insert(path.clone(), html.as_bytes().to_vec());
            }
            Write::Copy { out: path, from } => {
                out.insert(path.clone(), mem.0[from].clone());
            }
            Write::Html {
                out: path, bytes, ..
            } => {
                out.insert(path.clone(), bytes.clone());
            }
            Write::Manifest { json } => {
                out.insert(tachylite::site::MANIFEST.into(), json.as_bytes().to_vec());
            }
        }
    }
}

const HOME: &str = "pages/.tachylite/home.html";

/// Where the search shards are written.
const SEARCH: &str = "files/.tachylite/search/";

/// The URL key of a page under `pages/`; the inverse of `page_out`.
fn page_key(out: &str) -> &str {
    if out == HOME {
        return "";
    }
    let page = &out["pages/".len()..];
    page.strip_suffix("/.index.html")
        .unwrap_or(&page[..page.len() - ".html".len()])
}

/// Whether any write or push holds an excluded note's path or text.
fn leaks(delta: &Delta) -> bool {
    let bad = |text: &str| {
        ["private", "secret", HUSHED]
            .iter()
            .any(|w| text.contains(w))
    };
    let writes = delta.writes.iter().any(|w| match w {
        Write::Page { out, html } => bad(out) || bad(html),
        Write::Copy { out, from } => bad(out) || bad(from),
        // A published html page may hold any word but the marker.
        Write::Html {
            out, from, bytes, ..
        } => bad(out) || bad(from) || String::from_utf8_lossy(bytes).contains(HUSHED),
        Write::Manifest { json } => bad(json),
    });
    let pushes = delta.pushes.iter().any(|p| match p {
        Push::Page { key, html, .. } => bad(key) || bad(html),
        Push::Gone { key } => bad(key),
    });
    writes || pushes
}

/// The page URLs the written manifest lists.
fn manifest_urls(out: &Out) -> std::collections::BTreeSet<String> {
    let json = String::from_utf8_lossy(&out[tachylite::site::MANIFEST]).into_owned();
    let end = json.find(r#"},"files":["#).unwrap_or(json.len() - 2);
    let inner = &json[r#"{"pages":{"#.len()..end];
    inner
        .split('"')
        .skip(1)
        .step_by(4)
        .map(String::from)
        .collect()
}

/// The URLs a sync downloads: every file under `pages/`, the switcher list, the search
/// shards, and every html page. The model vault holds none past the size limit.
fn page_urls(out: &Out) -> std::collections::BTreeSet<String> {
    let listed = |p: &&String| p.ends_with("/notes.json") || p.starts_with(SEARCH);
    let files = out.keys().filter(|p| p.starts_with("files/.tachylite/"));
    let html = out.keys().filter_map(|p| {
        let path = p.strip_prefix("files/").filter(|_| !generated(p))?;
        let html = classify(path, &Exclude::default()) == Some(tachylite::vault::Kind::Html);
        html.then(|| format!("/{}", tachylite::render::encode(path)))
    });
    out.keys()
        .filter(|p| p.starts_with("pages/"))
        .map(|p| format!("/{}", tachylite::render::encode(page_key(p))))
        .chain(files.filter(listed).map(|p| p["files".len()..].to_string()))
        .chain(html)
        .collect()
}

/// Where a pushed page's key is written.
fn page_out(key: &str) -> String {
    if key.is_empty() {
        HOME.into()
    } else if key.starts_with(".tachylite/folders/") || key.starts_with(".tachylite/recent/") {
        format!("pages/{key}/.index.html")
    } else {
        format!("pages/{key}.html")
    }
}

/// Whether an output path is generated: an index page, the manifest or the switcher list.
fn generated(out: &str) -> bool {
    out.starts_with("pages/.tachylite/") || out.starts_with("files/.tachylite/")
}

/// The note page, html page and asset writes, without generated output.
fn files(delta: &Delta) -> Vec<&Write> {
    delta
        .writes
        .iter()
        .filter(|w| match w {
            Write::Page { out, .. } | Write::Copy { out, .. } | Write::Html { out, .. } => {
                !generated(out)
            }
            Write::Manifest { .. } => false,
        })
        .collect()
}

/// The output paths, without generated output.
fn published(out: &Out) -> Vec<&String> {
    out.keys().filter(|k| !generated(k)).collect()
}

fn sync_all(mem: &Mem) -> Change {
    Change::Sync {
        under: String::new(),
        entries: mem.entries_under(""),
    }
}

fn full(mem: &Mem) -> (Out, Delta) {
    full_with(mem, Exclude::default())
}

fn full_with(mem: &Mem, exclude: Exclude) -> (Out, Delta) {
    let mut out = Out::new();
    let changes = vec![sync_all(mem)];
    let delta = apply_kept(&mut Site::new(exclude), changes, &mut mem.clone(), &mut out);
    (out, delta)
}

/// A sink that reads pages back from the output earlier applies left, as the output
/// directory does, so a page whose backlinks alone change takes a new footer unrendered,
/// and search shards are built.
struct Kept<'a> {
    delta: Delta,
    out: &'a Out,
}

impl Sink for Kept<'_> {
    fn delete(&mut self, out: String) {
        self.delta.delete(out);
    }
    fn write(&mut self, write: Write) {
        self.delta.write(write);
    }
    fn push(&mut self, push: Push) {
        self.delta.push(push);
    }
    fn page(&mut self, out: &str) -> Option<String> {
        let gone = self.delta.deletes.iter().any(|d| d == out);
        self.delta.page(out).or_else(|| {
            let page = self.out.get(out).filter(|_| !gone)?;
            Some(String::from_utf8_lossy(page).into_owned())
        })
    }
    fn keeps(&self) -> bool {
        true
    }
}

/// Applies changes from `source` through a `Kept` over `out`, and derives.
fn kept(site: &mut Site, changes: Vec<Change>, source: &mut impl Source, out: &Out) -> Delta {
    let mut kept = Kept {
        delta: Delta::default(),
        out,
    };
    let report = site.apply_to(changes, source, &mut kept);
    site.derive_to(&mut kept);
    let mut delta = kept.delta;
    delta.skipped = report.skipped;
    delta.broken = report.broken;
    delta.notices = report.notices;
    delta
}

/// Applies changes through a `Kept` over `out`, derives, and folds the delta into `out`.
fn apply_kept(site: &mut Site, changes: Vec<Change>, mem: &mut Mem, out: &mut Out) -> Delta {
    let delta = kept(site, changes, &mut mem.clone(), out);
    fold(out, &delta, mem);
    delta
}

fn touch(path: &str) -> Change {
    Change::Touch(path.to_string())
}

fn clash(path: &str) -> Skipped {
    Skipped {
        path: PathBuf::from(path),
        reason: SkipReason::UrlClash,
    }
}

#[test]
fn full_sync_writes_every_page() {
    let mem = Mem::with(&[("a.md", "# A"), ("d/b.md", "# B"), ("img/x.png", "png")]);
    let (out, delta) = full(&mem);
    assert_eq!(
        published(&out),
        ["files/img/x.png", "pages/a.html", "pages/d/b.html"]
    );
    let pages = files(&delta)
        .into_iter()
        .filter(|w| matches!(w, Write::Page { .. }))
        .count();
    let copies = files(&delta)
        .into_iter()
        .filter(|w| matches!(w, Write::Copy { .. }))
        .count();
    assert_eq!((pages, copies), (2, 1));
    assert_eq!(pushed(&delta), ["a", "d/b"]);
    assert!(!delta.pushes.iter().any(|p| matches!(p, Push::Gone { .. })));
    assert!(delta.deletes.is_empty());
    assert!(delta.skipped.is_empty());
}

#[test]
fn unchanged_touch_writes_and_pushes_nothing() {
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert!(files(&delta).is_empty() && delta.pushes.is_empty() && delta.deletes.is_empty());
}

#[test]
fn edited_note_is_written_and_pushed() {
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    mem.0.insert("a.md".into(), b"# Edited".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(files(&delta).len(), 1);
    match &delta.pushes[..] {
        [Push::Page { key, html, version }] => {
            assert_eq!(key, "a");
            assert!(html.contains("Edited"));
            assert!(html.contains(version.as_str()));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn deleted_note_is_deleted_and_gone() {
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    mem.0.remove("a.md");
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(delta.deletes, ["pages/a.html"]);
    let gone: Vec<_> = delta
        .pushes
        .iter()
        .filter(|p| matches!(p, Push::Gone { .. }))
        .collect();
    assert!(matches!(&gone[..], [Push::Gone { key }] if key == "a"));
}

#[test]
fn html_page_beats_note_with_its_url() {
    let mem = Mem::with(&[("x.html", "<p>page</p>"), ("x.html.md", "# note")]);
    let (out, delta) = full(&mem);
    assert_eq!(published(&out), ["files/x.html"]);
    assert_eq!(delta.skipped, [clash("x.html.md")]);
}

#[test]
fn folder_page_clash() {
    let mem = Mem::with(&[("x.md", "# x"), ("x.html/y.md", "# y")]);
    let (out, delta) = full(&mem);
    assert_eq!(published(&out), ["pages/x.html/y.html"]);
    assert_eq!(delta.skipped, [clash("x.md")]);
}

#[test]
fn a_name_too_long_for_its_page_leaves_no_trace() {
    let long = format!("{}.md", "n".repeat(251));
    let link = format!("[[{0}]] ![[{0}]]", "n".repeat(251));
    let mut mem = Mem::with(&[("a.md", &link)]);
    let mut site = Site::new(Exclude::default());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    // Create it, rename it to a short name, then rename it back.
    let steps = [
        (None, &*long),
        (Some(&*long), "s.md"),
        (Some("s.md"), &*long),
    ];
    for (from, to) in steps {
        let bytes = match from {
            Some(from) => mem.0.remove(from).unwrap(),
            None => b"# toolong-marker".to_vec(),
        };
        mem.0.insert(to.to_string(), bytes);
        let mut changes: Vec<Change> = from.into_iter().map(touch).collect();
        changes.push(touch(to));
        let delta = apply_kept(&mut site, changes, &mut mem, &mut out);
        assert_eq!(out, full(&mem).0, "after {to}");
        let skipped: Vec<Skipped> = (to == long)
            .then(|| Skipped {
                path: PathBuf::from(to),
                reason: SkipReason::NameTooLong,
            })
            .into_iter()
            .collect();
        assert_eq!(delta.skipped, skipped);
    }
    let text: String = out.values().map(|b| String::from_utf8_lossy(b)).collect();
    assert!(!text.contains("toolong-marker"));
    assert!(!out.keys().any(|k| k.contains("nnnn")));
}

#[test]
fn deleting_the_winner_promotes_the_loser() {
    let mut mem = Mem::with(&[("a.MD", "# upper"), ("a.md", "# lower")]);
    let mut site = Site::new(Exclude::default());
    let first = site.apply(vec![sync_all(&mem)], &mut mem);
    assert_eq!(first.skipped, [clash("a.md")]);
    mem.0.remove("a.MD");
    let delta = site.apply(vec![touch("a.MD")], &mut mem);
    let mut out = Out::new();
    fold(&mut out, &first, &mem);
    fold(&mut out, &delta, &mem);
    assert!(String::from_utf8_lossy(&out["pages/a.html"]).contains("lower"));
    assert!(
        !delta.pushes.iter().any(|p| matches!(p, Push::Gone { .. })),
        "the key stays live, so no gone: {:?}",
        delta.pushes
    );
}

#[test]
fn excluded_touch_is_ignored() {
    let mut mem = Mem::with(&[(".obsidian/x.md", "# hidden")]);
    let mut site = Site::new(Exclude::default());
    let delta = site.apply(vec![touch(".obsidian/x.md")], &mut mem);
    assert!(files(&delta).is_empty() && pushed(&delta).is_empty() && delta.deletes.is_empty());
}

#[test]
fn unreadable_note_is_skipped_and_unpublished() {
    struct Denied;
    impl Source for Denied {
        fn note(&mut self, _: &str) -> Result<Note, ReadError> {
            Err(ReadError::Skip(SkipReason::Symlink))
        }
        fn file(&mut self, _: &str) -> Result<i64, ReadError> {
            Err(ReadError::Skip(SkipReason::Symlink))
        }
    }
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    let delta = site.apply(vec![touch("a.md")], &mut Denied);
    assert_eq!(delta.deletes, ["pages/a.html"]);
    assert_eq!(
        delta.skipped,
        [Skipped {
            path: PathBuf::from("a.md"),
            reason: SkipReason::Symlink
        }]
    );
}

/// Whether a push key is an index page's: the home page, or one under the reserved prefix.
fn index_key(key: &str) -> bool {
    key.is_empty() || key.starts_with(".tachylite/")
}

/// The note pages pushed.
fn pushed(delta: &Delta) -> Vec<&str> {
    delta
        .pushes
        .iter()
        .filter_map(|p| match p {
            Push::Page { key, .. } if !index_key(key) => Some(key.as_str()),
            _ => None,
        })
        .collect()
}

/// The index pages pushed.
fn indexed(delta: &Delta) -> Vec<&str> {
    delta
        .pushes
        .iter()
        .filter_map(|p| match p {
            Push::Page { key, .. } if index_key(key) => Some(key.as_str()),
            _ => None,
        })
        .collect()
}

/// The last page pushed for `key`: the one a client ends on.
fn html<'a>(delta: &'a Delta, key: &str) -> &'a str {
    delta
        .pushes
        .iter()
        .rev()
        .find_map(|p| match p {
            Push::Page { key: k, html, .. } if k == key => Some(&**html),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no page pushed for {key}"))
}

fn broken(from: &str, target: &str, matches: usize) -> Broken {
    Broken {
        from: from.into(),
        target: target.into(),
        matches,
    }
}

fn synced(mem: &mut Mem, exclude: Exclude) -> (Site, Delta) {
    let mut site = Site::new(exclude);
    let delta = site.apply(vec![sync_all(mem)], mem);
    (site, delta)
}

#[test]
fn an_apply_says_when_it_unpublished_a_page() {
    let mut mem = Mem::with(&[("a.md", "# a"), ("d/n.md", "# n")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("a.md".into(), b"# a2".to_vec());
    let mut delta = Delta::default();
    let report = site.apply_to(vec![touch("a.md")], &mut mem.clone(), &mut delta);
    assert!(!report.unpublished, "an edit unpublishes nothing");
    site.set_exclude(Exclude::new(&["d".to_string()]).unwrap());
    let report = site.apply_to(vec![sync_all(&mem)], &mut mem.clone(), &mut delta);
    assert!(report.unpublished);
}

#[test]
fn link_adds_backlink() {
    let mut mem = Mem::with(&[("a.md", "# A"), ("b.md", "# B")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(!html(&first, "b").contains("backlinks"));
    mem.0.insert("a.md".into(), b"[[b]]".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(pushed(&delta), ["a", "b"]);
    assert_eq!(files(&delta).len(), 2);
    assert!(html(&delta, "a").contains("<a href=\"/b\">b</a>"));
    assert!(
        html(&delta, "b").contains("<h2>Backlinks</h2><ul><li><a href=\"/a\">a</a></li></ul>"),
        "{}",
        html(&delta, "b")
    );
}

#[test]
fn title_names_page_and_backlinks() {
    let mut mem = Mem::with(&[
        ("a.md", "---\ntitle: First <A>\n---\n[[b]]"),
        ("b.md", "# B"),
    ]);
    let (_, delta) = synced(&mut mem, Exclude::default());
    assert!(html(&delta, "a").contains("<title>First &lt;A&gt;</title>"));
    assert!(html(&delta, "b").contains("<title>b</title>"));
    assert!(html(&delta, "b").contains("<li><a href=\"/a\">First &lt;A&gt;</a></li>"));
}

#[test]
fn retitled_note_updates_backlink_text() {
    let mut mem = Mem::with(&[("a.md", "---\ntitle: One\n---\n[[b]]"), ("b.md", "# B")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0
        .insert("a.md".into(), b"---\ntitle: Two\n---\n[[b]]".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(pushed(&delta), ["a", "b"]);
    assert!(html(&delta, "b").contains("<li><a href=\"/a\">Two</a></li>"));
    // Dropping the title falls back to the file name.
    mem.0.insert("a.md".into(), b"[[b]]".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert!(html(&delta, "b").contains("<li><a href=\"/a\">a</a></li>"));
}

#[test]
fn deleting_the_linker_removes_the_backlink() {
    let mut mem = Mem::with(&[("a.md", "[[b]]"), ("b.md", "# B")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(html(&first, "b").contains("backlinks"));
    mem.0.remove("a.md");
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(pushed(&delta), ["b"]);
    assert!(!html(&delta, "b").contains("backlinks"));
}

#[test]
fn rename_target_breaks_links() {
    let mut mem = Mem::with(&[("a.md", "[[b]]"), ("b.md", "# B")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(first.broken.is_empty());
    let bytes = mem.0.remove("b.md").unwrap();
    mem.0.insert("c.md".into(), bytes);
    let delta = site.apply(vec![touch("b.md"), touch("c.md")], &mut mem);
    assert_eq!(pushed(&delta), ["c", "a"]);
    assert!(html(&delta, "a").contains("<a class=\"broken-link\">b</a>"));
    assert_eq!(delta.broken, [broken("a.md", "b", 0)]);
}

#[test]
fn create_fixes_broken_link() {
    let mut mem = Mem::with(&[("a.md", "[[b]]")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert_eq!(first.broken, [broken("a.md", "b", 0)]);
    mem.0.insert("b.md".into(), b"# B".to_vec());
    let delta = site.apply(vec![touch("b.md")], &mut mem);
    // The new note goes first, then again with the backlink its linker's render adds.
    assert_eq!(pushed(&delta), ["b", "a", "b"]);
    assert!(html(&delta, "a").contains("<a href=\"/b\">b</a>"));
    assert!(html(&delta, "b").contains("<li><a href=\"/a\">a</a></li>"));
    assert!(delta.broken.is_empty());
}

#[test]
fn create_makes_link_ambiguous() {
    let mut mem = Mem::with(&[("x/r.md", "# r"), ("y/a.md", "[[r]]")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(html(&first, "y/a").contains("<a href=\"/x/r\">r</a>"));
    mem.0.insert("z/r.md".into(), b"# r".to_vec());
    let delta = site.apply(vec![touch("z/r.md")], &mut mem);
    assert_eq!(pushed(&delta), ["z/r", "y/a", "x/r"]);
    assert!(html(&delta, "y/a").contains("<a class=\"broken-link\">r</a>"));
    assert!(!html(&delta, "x/r").contains("backlinks"));
    assert_eq!(delta.broken, [broken("y/a.md", "r", 2)]);
}

#[test]
fn excluded_note_has_no_backlink_and_breaks_links() {
    let mut mem = Mem::with(&[
        ("a.md", "[[s]] and [[b]]"),
        ("b.md", "# B"),
        ("private/s.md", "[[b]]"),
    ]);
    let (_, delta) = synced(&mut mem, Exclude::new(&["private".to_string()]).unwrap());
    assert_eq!(pushed(&delta), ["a", "b"]);
    assert!(html(&delta, "a").contains("<a class=\"broken-link\">s</a>"));
    assert!(html(&delta, "b").contains("<ul><li><a href=\"/a\">a</a></li></ul>"));
    assert_eq!(delta.broken, [broken("a.md", "s", 0)]);
    for push in &delta.pushes {
        if let Push::Page { html, .. } = push {
            assert!(!html.contains("private"), "{html}");
        }
    }
}

#[test]
fn adding_alias_fixes_broken_link() {
    let mut mem = Mem::with(&[("a.md", "[[JD]]"), ("p/jane.md", "# Jane")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert_eq!(first.broken, [broken("a.md", "JD", 0)]);
    mem.0.insert(
        "p/jane.md".into(),
        b"---\naliases: [jd]\n---\n# Jane".to_vec(),
    );
    let delta = site.apply(vec![touch("p/jane.md")], &mut mem);
    assert_eq!(pushed(&delta), ["p/jane", "a", "p/jane"]);
    assert!(html(&delta, "a").contains("<a href=\"/p/jane\">JD</a>"));
    assert!(html(&delta, "p/jane").contains("<li><a href=\"/a\">a</a></li>"));
    assert!(delta.broken.is_empty());
}

#[test]
fn removing_alias_breaks_link() {
    let mut mem = Mem::with(&[("a.md", "[[JD]]"), ("p/jane.md", "---\naliases: JD\n---\n")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(html(&first, "a").contains("<a href=\"/p/jane\">JD</a>"));
    mem.0.insert("p/jane.md".into(), b"# Jane".to_vec());
    let delta = site.apply(vec![touch("p/jane.md")], &mut mem);
    assert_eq!(pushed(&delta), ["p/jane", "a", "p/jane"]);
    assert!(html(&delta, "a").contains("<a class=\"broken-link\">JD</a>"));
    assert!(!html(&delta, "p/jane").contains("backlinks"));
}

#[test]
fn deleting_aliased_note_breaks_link() {
    let mut mem = Mem::with(&[("a.md", "[[JD]]"), ("p/jane.md", "---\naliases: JD\n---\n")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.remove("p/jane.md");
    let delta = site.apply(vec![touch("p/jane.md")], &mut mem);
    assert!(html(&delta, "a").contains("<a class=\"broken-link\">JD</a>"));
}

#[test]
fn new_file_named_like_alias_takes_the_link() {
    let mut mem = Mem::with(&[("a.md", "[[JD]]"), ("p/jane.md", "---\naliases: JD\n---\n")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("q/jd.md".into(), b"# other".to_vec());
    let delta = site.apply(vec![touch("q/jd.md")], &mut mem);
    assert_eq!(pushed(&delta), ["q/jd", "a", "q/jd", "p/jane"]);
    assert!(html(&delta, "a").contains("<a href=\"/q/jd\">JD</a>"));
    assert!(!html(&delta, "p/jane").contains("backlinks"));
}

#[test]
fn excluded_note_alias_does_not_resolve() {
    let mut mem = Mem::with(&[
        ("a.md", "[[JD]]"),
        ("private/jane.md", "---\naliases: JD\n---\n"),
    ]);
    let (_, delta) = synced(&mut mem, Exclude::new(&["private".to_string()]).unwrap());
    assert!(html(&delta, "a").contains("<a class=\"broken-link\">JD</a>"));
}

#[test]
fn a_downward_link_without_dot_slash_resolves_from_its_note() {
    let mut mem = Mem::with(&[
        ("proj/plan.md", "[[sub/detail]] and [md](sub/detail.md)"),
        ("proj/sub/detail.md", "# Detail"),
    ]);
    let (_, delta) = synced(&mut mem, Exclude::default());
    let plan = html(&delta, "proj/plan");
    assert_eq!(
        plan.matches("href=\"/proj/sub/detail\"").count(),
        2,
        "{plan}"
    );
    assert!(delta.broken.is_empty(), "{:?}", delta.broken);
}

#[test]
fn unchanged_backlinks_render_nothing() {
    let mut mem = Mem::with(&[("a.md", "[[b]] one"), ("b.md", "# B")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("a.md".into(), b"[[b]] two".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(pushed(&delta), ["a"]);
}

const PATHS: [&str; 15] = [
    "a.md",
    "a.MD",
    "x.html",
    "private/h.html",
    "x.html.md",
    "x.md",
    "x.html/y.md",
    "x/z.md",
    "d/n.md",
    "d/p.png",
    "d/e/m.md",
    ".h.md",
    "private/s.md",
    "d/k.secret.md",
    "d/2026-01-03.md",
];

/// What the exclusion property excludes: `private/s.md`, `private/h.html` and
/// `d/k.secret.md`, wherever a move takes it.
fn hush() -> Exclude {
    Exclude::new(&["private".to_string(), "**/*.secret.md".to_string()]).unwrap()
}

/// Text only an excluded note holds.
const HUSHED: &str = "hush-marker";

const CONTENTS: [&str; 19] = [
    "[[e/m]] [down](e/m.md)",
    "# one",
    "two *b*",
    "- [ ] three",
    "[[a]]",
    "[[d/n]] and [[../x]]",
    "[[n]]",
    "[[m]] [[y]] [[p.png]]",
    "---\naliases: [n, q]\ntitle: T1\n---\n[[q]]",
    "---\naliases: m\n---\n[[a]]",
    "---\ntitle: T2\n---\n[[q]] [[m]]",
    "two ![[a]] ![[q]]",
    "![[n]] ![[d/e/m]] ![[p.png]] ![[x.html]]",
    "| t |\n|---|\n| ![[a\\|x]] ![[n\\|y]] |",
    "[[s|ess]] ![[k.secret|kay]] ![[private/s|pee]] [[z]]",
    "#a #A/b x",
    "---\ntags: [a/b, Über]\ntitle: T3\n---\n#c",
    "- [/] dig [[a]] #t\n- [x] mow\n- [-] cut\n- [ ] sow [[n]]",
    "- [ ] a\n- [ ] b\n- [/] c\n- [ ] d\n- [ ] e\n- [ ] f [[m]]",
];

/// A source where every file is mid-save.
struct MidSave;

impl Source for MidSave {
    fn note(&mut self, _: &str) -> Result<Note, ReadError> {
        Err(ReadError::MidSave)
    }
    fn file(&mut self, _: &str) -> Result<i64, ReadError> {
        Err(ReadError::MidSave)
    }
}

#[test]
fn mid_save_keeps_published_pages() {
    let mut mem = Mem::with(&[("a.md", "# A"), ("i.png", "png")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    let delta = site.apply(vec![touch("a.md"), touch("i.png")], &mut MidSave);
    assert_eq!(delta, Delta::default());
    mem.0.insert("a.md".into(), b"# B".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(
        files(&delta).len(),
        1,
        "the save after a mid-save is published"
    );
}

#[test]
fn mid_save_new_file_waits_for_its_save() {
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    let delta = site.apply(vec![touch("b.md"), touch("j.png")], &mut MidSave);
    assert_eq!(delta, Delta::default());
    mem.0.insert("b.md".into(), b"# B".to_vec());
    let delta = site.apply(vec![touch("b.md")], &mut mem);
    assert_eq!(files(&delta).len(), 1);
}

#[test]
fn mid_save_loser_does_not_block_promotion() {
    let mut mem = Mem::with(&[("a.MD", "# upper"), ("a.md", "# lower")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    // a.MD wins; deleting it promotes a.md, which is mid-save.
    struct Mixed;
    impl Source for Mixed {
        fn note(&mut self, path: &str) -> Result<Note, ReadError> {
            match path {
                "a.md" => Err(ReadError::MidSave),
                _ => Err(ReadError::Missing),
            }
        }
        fn file(&mut self, _: &str) -> Result<i64, ReadError> {
            Err(ReadError::Missing)
        }
    }
    let delta = site.apply(vec![touch("a.MD")], &mut Mixed);
    assert_eq!(delta.deletes, ["pages/a.html"]);
    assert!(files(&delta).is_empty());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(files(&delta).len(), 1, "its save publishes it");
}

/// Daily notes in the folder the moves take away, so they come and go one by one and at once.
const DAYS: [&str; 4] = [
    "d/2026-01-02.md",
    "d/2026-01-03.md",
    "d/2026-01-05.md",
    "d/2026-02-01.md",
];

#[derive(Debug, Clone)]
enum Op {
    Write(usize, usize),
    Delete(usize),
    DeleteDir,
    MoveDir,
    /// Writes a daily note, or deletes it.
    Day(usize, bool),
    /// Sets the daily folder to `d`, or to none. A sync of the root follows, as after a
    /// config save.
    Daily(bool),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..PATHS.len(), 0..CONTENTS.len()).prop_map(|(p, c)| Op::Write(p, c)),
        2 => (0..PATHS.len()).prop_map(Op::Delete),
        1 => Just(Op::DeleteDir),
        1 => Just(Op::MoveDir),
        3 => (0..DAYS.len(), any::<bool>()).prop_map(|(d, write)| Op::Day(d, write)),
        1 => any::<bool>().prop_map(Op::Daily),
    ]
}

/// The header config with the daily folder `d`, or with none.
fn daily_nav(on: bool) -> tachylite::config::Nav {
    nav(if on { "daily = { folder = \"d\" }" } else { "" })
}

/// Applies an op to the model vault and returns the changes the watcher would report.
/// Each write gets the next modified time, so recent order varies.
fn apply_op(mem: &mut Mem, op: &Op) -> Vec<Change> {
    match op {
        Op::Write(p, c) => {
            let path = PATHS[*p].to_string();
            let mut bytes = CONTENTS[*c].as_bytes().to_vec();
            if hush().path(&path) {
                bytes.extend_from_slice(format!(" #{HUSHED}").as_bytes());
            }
            let time = mem.1.values().max().map_or(1, |t| t + 1);
            mem.1.insert(path.clone(), time);
            mem.0.insert(path.clone(), bytes);
            vec![touch(&path)]
        }
        Op::Delete(p) => {
            mem.0.remove(PATHS[*p]);
            vec![touch(PATHS[*p])]
        }
        Op::Day(d, true) => {
            let time = mem.1.values().max().map_or(1, |t| t + 1);
            mem.1.insert(DAYS[*d].to_string(), time);
            mem.0.insert(DAYS[*d].to_string(), b"day".to_vec());
            vec![touch(DAYS[*d])]
        }
        Op::Day(d, false) => {
            mem.0.remove(DAYS[*d]);
            vec![touch(DAYS[*d])]
        }
        Op::Daily(_) => vec![sync_all(mem)],
        Op::DeleteDir => {
            mem.0.retain(|k, _| !k.starts_with("d/"));
            vec![Change::Sync {
                under: "d".into(),
                entries: vec![],
            }]
        }
        Op::MoveDir => {
            if mem.0.keys().any(|k| k.starts_with("d2/")) {
                return vec![];
            }
            let moved: Vec<_> = mem
                .0
                .iter()
                .filter(|(k, _)| k.starts_with("d/"))
                .map(|(k, v)| (format!("d2/{}", &k[2..]), v.clone()))
                .collect();
            let times: Vec<_> = mem
                .1
                .iter()
                .filter(|(k, _)| k.starts_with("d/"))
                .map(|(k, t)| (format!("d2/{}", &k[2..]), *t))
                .collect();
            mem.0.retain(|k, _| !k.starts_with("d/"));
            mem.1.retain(|k, _| !k.starts_with("d/"));
            mem.0.extend(moved);
            mem.1.extend(times);
            vec![
                Change::Sync {
                    under: "d".into(),
                    entries: vec![],
                },
                Change::Sync {
                    under: "d2".into(),
                    entries: mem.entries_under("d2"),
                },
            ]
        }
    }
}

proptest! {
    #[test]
    fn incremental_equals_full(
        ops in prop::collection::vec((op(), any::<bool>()), 1..30),
        batch in 1usize..4,
    ) {
        let mut mem = Mem::default();
        let mut site = Site::new(hush());
        let mut daily = true;
        site.set_nav(daily_nav(daily));
        let mut out = Out::new();
        let mut last_push: BTreeMap<String, Push> = BTreeMap::new();

        // A chunk derives only if its last op says so, as a watcher derives only once its
        // queue is empty; the last chunk always does.
        let chunks: Vec<_> = ops.chunks(batch).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let mut changes = Vec::new();
            for (op, _) in *chunk {
                if let Op::Daily(on) = op {
                    daily = *on;
                    site.set_nav(daily_nav(daily));
                }
                changes.extend(apply_op(&mut mem, op));
            }
            let mut source = mem.clone();
            let mut kept = Kept { delta: Delta::default(), out: &out };
            let report = site.apply_to(changes, &mut source, &mut kept);
            if chunk[chunk.len() - 1].1 || i == chunks.len() - 1 {
                site.derive_to(&mut kept);
            }
            let mut delta = kept.delta;
            delta.skipped = report.skipped;
            fold(&mut out, &delta, &mem);
            prop_assert!(!leaks(&delta), "an excluded note reached the output");
            for push in delta.pushes {
                let key = match &push {
                    Push::Page { key, .. } | Push::Gone { key } => key.clone(),
                };
                last_push.insert(key, push);
            }
        }

        let mut fresh = Site::new(hush());
        fresh.set_nav(daily_nav(daily));
        let mut expected = Out::new();
        apply_kept(&mut fresh, vec![sync_all(&mem)], &mut mem.clone(), &mut expected);
        prop_assert_eq!(&out, &expected);
        prop_assert_eq!(manifest_urls(&out), page_urls(&out));
        for path in out.keys() {
            let file = format!("{path}/");
            prop_assert!(!out.keys().any(|p| p.starts_with(&file)), "{path} is also a folder");
        }

        for (key, push) in &last_push {
            let page = expected.get(&page_out(key));
            match push {
                Push::Page { html, .. } => {
                    prop_assert_eq!(page.map(|b| b.as_slice()), Some(html.as_bytes()));
                }
                Push::Gone { .. } => prop_assert!(page.is_none(), "{key} is gone but published"),
            }
        }
        for path in expected.keys().filter(|p| p.starts_with("pages/")) {
            let key = page_key(path);
            prop_assert!(last_push.contains_key(key), "{key} was published without a push");
        }
    }
}

/// 300 notes cross the 256-note batch edge and the threshold for threads.
/// Each group of 16 touches renders on one thread.
#[test]
fn threaded_full_build_equals_small_applies() {
    let notes = tachylite::budget_vault::generate(7, 300);
    let pairs: Vec<(&str, &str)> = notes
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let mem = Mem::with(&pairs);
    let (want, delta) = full(&mem);
    assert!(delta.broken.windows(2).all(|w| w[0].from <= w[1].from));

    let mut site = Site::new(Exclude::default());
    let mut mem = mem;
    let mut got = Out::new();
    for group in notes.chunks(16) {
        let changes = group.iter().map(|(p, _)| touch(p)).collect();
        apply_kept(&mut site, changes, &mut mem, &mut got);
    }
    assert_eq!(got.len(), want.len());
    for (path, html) in &want {
        assert!(got.get(path) == Some(html), "{path} differs");
    }
}

#[test]
fn full_build_transcludes() {
    let mem = Mem::with(&[("a.md", "![[b]]"), ("b.md", "bee ![[c]]"), ("c.md", "sea")]);
    let (out, delta) = full(&mem);
    let a = String::from_utf8_lossy(&out["pages/a.html"]).to_string();
    assert!(a.contains("bee") && a.contains("sea"), "{a}");
    assert!(
        html(&delta, "b").contains("Backlinks"),
        "an embed is a backlink"
    );
}

#[test]
fn editing_an_embedded_note_re_renders_its_embedders() {
    let mut mem = Mem::with(&[("a.md", "![[b]]"), ("b.md", "![[c]]"), ("c.md", "old")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("c.md".into(), b"new".to_vec());
    let delta = site.apply(vec![touch("c.md")], &mut mem);
    // The saved note's own push waits for no embedder.
    assert_eq!(pushed(&delta), ["c", "a", "b"]);
    assert!(html(&delta, "a").contains("new"));
}

#[test]
fn creating_and_deleting_an_embedded_note() {
    let mut mem = Mem::with(&[("a.md", "![[b]]")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("b.md".into(), b"bee".to_vec());
    let delta = site.apply(vec![touch("b.md")], &mut mem);
    assert!(html(&delta, "a").contains("bee"));
    mem.0.remove("b.md");
    let delta = site.apply(vec![touch("b.md")], &mut mem);
    assert!(!html(&delta, "a").contains("bee"));
    assert!(html(&delta, "a").contains("broken-link"));
}

#[test]
fn a_change_inside_an_embed_reaches_the_embedder() {
    let mut mem = Mem::with(&[("a.md", "![[b]]"), ("b.md", "see [[c]]")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("c.md".into(), b"sea".to_vec());
    let delta = site.apply(vec![touch("c.md")], &mut mem);
    assert!(html(&delta, "a").contains("<a href=\"/c\">c</a>"));
}

#[test]
fn embed_cycle_renders_and_updates() {
    let mut mem = Mem::with(&[("a.md", "aye ![[b]]"), ("b.md", "bee ![[a]]")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(html(&first, "a").contains("bee"));
    mem.0.insert("a.md".into(), b"new ![[b]]".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(pushed(&delta), ["a", "b"]);
    assert!(html(&delta, "b").contains("new"));
}

#[test]
fn an_embed_the_scan_misses_still_renders_alone() {
    let mut mem = Mem::with(&[
        ("a.md", "aye"),
        ("b.md", "![[a]]"),
        ("n.md", "| t |\n|---|\n| ![[a\\|x]] |"),
    ]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(html(&first, "n").contains("aye"));
    mem.0
        .insert("n.md".into(), b"| t |\n|---|\n| ![[a\\|x]] two |".to_vec());
    let delta = site.apply(vec![touch("n.md")], &mut mem);
    assert!(html(&delta, "n").contains("aye"), "{}", html(&delta, "n"));
    mem.0.insert("a.md".into(), b"new".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert!(html(&delta, "n").contains("new"));
}

fn manifest(delta: &Delta) -> Option<&str> {
    delta.writes.iter().find_map(|w| match w {
        Write::Manifest { json } => Some(json.as_str()),
        _ => None,
    })
}

/// The manifest's note and html page entries, as `{"pages":{…}}`, without the generated
/// pages.
fn note_manifest(delta: &Delta) -> Option<String> {
    let json = manifest(delta)?;
    let inner = &json[r#"{"pages":{"#.len()..json.find(r#"},"files":["#)?];
    let quoted: Vec<&str> = inner.split('"').skip(1).step_by(2).collect();
    let notes: Vec<String> = quoted
        .chunks(2)
        .filter(|kv| kv[0] != "/" && !kv[0].starts_with("/.tachylite/"))
        .map(|kv| format!(r#""{}":"{}""#, kv[0], kv[1]))
        .collect();
    Some(format!(r#"{{"pages":{{{}}}}}"#, notes.join(",")))
}

fn version(delta: &Delta, key: &str) -> String {
    delta
        .pushes
        .iter()
        .find_map(|p| match p {
            Push::Page {
                key: k, version, ..
            } if k == key => Some(version.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no page pushed for {key}"))
}

/// The version of an html page with these bytes: their hash, as a page's version is.
fn html_version(bytes: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bytes.as_bytes().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[test]
fn the_manifest_lists_every_note_page_and_html_page_with_its_version() {
    let mem = Mem::with(&[
        ("b.md", "bee"),
        ("a.md", "aye"),
        ("sub/c d.md", "sea"),
        ("img.png", "png"),
        ("page.html", "<p>page</p>"),
        ("sub/e f.html", "<p>eff</p>"),
    ]);
    let (out, delta) = full(&mem);
    let want = format!(
        r#"{{"pages":{{"/a":"{}","/b":"{}","/sub/c%20d":"{}","/page.html":"{}","/sub/e%20f.html":"{}"}}}}"#,
        version(&delta, "a"),
        version(&delta, "b"),
        version(&delta, "sub/c d"),
        html_version("<p>page</p>"),
        html_version("<p>eff</p>"),
    );
    assert_eq!(note_manifest(&delta).as_deref(), Some(want.as_str()));
    assert_eq!(
        out.get(tachylite::site::MANIFEST).map(Vec::as_slice),
        manifest(&delta).map(str::as_bytes)
    );
    let last = delta.writes.last();
    assert!(matches!(last, Some(Write::Manifest { .. })), "{last:?}");
}

/// The manifest's `files` list, as written.
fn file_list(delta: &Delta) -> Option<&str> {
    let json = manifest(delta)?;
    let at = json.find(r#""files":"#)? + r#""files":"#.len();
    Some(&json[at..json.len() - 1])
}

#[test]
fn the_manifest_lists_every_html_page_and_asset_as_a_file() {
    let mem = Mem::with(&[
        ("a.md", "aye"),
        ("page.html", "<p>page</p>"),
        ("img.png", "png"),
        ("sub/c d.pdf", "pdf"),
        ("private/x.png", "png"),
    ]);
    let mut source = mem.clone();
    let delta = Site::new(Exclude::new(&["private".to_string()]).unwrap())
        .apply(vec![sync_all(&mem)], &mut source);
    assert_eq!(
        file_list(&delta),
        Some(r#"["/img.png","/page.html","/sub/c%20d.pdf"]"#)
    );
}

#[test]
fn the_manifest_follows_a_file_added_and_removed() {
    let mut mem = Mem::with(&[("a.md", "aye")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert_eq!(file_list(&first), Some("[]"));
    mem.0.insert("img.png".into(), b"png".to_vec());
    let delta = site.apply(vec![touch("img.png")], &mut mem);
    assert_eq!(file_list(&delta), Some(r#"["/img.png"]"#));
    mem.0.remove("img.png");
    let delta = site.apply(vec![touch("img.png")], &mut mem);
    assert_eq!(file_list(&delta), Some("[]"));
}

#[test]
fn an_empty_vault_lists_only_generated_pages() {
    let (_, delta) = full(&Mem::default());
    assert_eq!(note_manifest(&delta).as_deref(), Some(r#"{"pages":{}}"#));
    let json = manifest(&delta).unwrap();
    for url in [
        "\"/\"",
        "\"/.tachylite/recent\"",
        "\"/.tachylite/notes.json\"",
    ] {
        assert!(json.contains(url), "{json}");
    }
}

#[test]
fn the_manifest_follows_edits_and_deletes() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("b.md", "bee")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    let b = version(&first, "b");
    mem.0.insert("a.md".into(), b"new".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    let a = version(&delta, "a");
    let want = format!(r#"{{"pages":{{"/a":"{a}","/b":"{b}"}}}}"#);
    assert_eq!(note_manifest(&delta).as_deref(), Some(want.as_str()));
    mem.0.remove("a.md");
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    let want = format!(r#"{{"pages":{{"/b":"{b}"}}}}"#);
    assert_eq!(note_manifest(&delta).as_deref(), Some(want.as_str()));
}

#[test]
fn a_change_no_page_shows_writes_no_manifest() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("img.png", "png")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("img.png".into(), b"new png".to_vec());
    let delta = site.apply(vec![touch("img.png")], &mut mem);
    assert_eq!(manifest(&delta), None);
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(manifest(&delta), None, "an unchanged note");
}

#[test]
fn the_manifest_follows_an_html_page_edited_saved_unchanged_and_removed() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("p.html", "<p>one</p>")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    let a = version(&first, "a");
    let listed = |html: &str| {
        let html = html_version(html);
        format!(r#"{{"pages":{{"/a":"{a}","/p.html":"{html}"}}}}"#)
    };
    assert_eq!(note_manifest(&first), Some(listed("<p>one</p>")));
    let delta = site.apply(vec![touch("p.html")], &mut mem);
    assert_eq!(manifest(&delta), None, "an unchanged html page");
    assert!(delta.pushes.is_empty(), "{:?}", delta.pushes);
    mem.0.insert("p.html".into(), b"<p>two</p>".to_vec());
    let delta = site.apply(vec![touch("p.html")], &mut mem);
    assert_eq!(note_manifest(&delta), Some(listed("<p>two</p>")));
    assert!(
        delta.pushes.is_empty(),
        "no index page shows it: {:?}",
        delta.pushes
    );
    mem.0.remove("p.html");
    let delta = site.apply(vec![touch("p.html")], &mut mem);
    assert_eq!(delta.deletes[0], "files/p.html");
    assert_eq!(
        note_manifest(&delta),
        Some(format!(r#"{{"pages":{{"/a":"{a}"}}}}"#))
    );
    assert_eq!(file_list(&delta), Some("[]"));
}

/// An in-memory vault whose notes carry modified times.
struct Timed(BTreeMap<String, (Vec<u8>, i64)>);

impl Source for Timed {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        self.0
            .get(path)
            .map(|(bytes, modified)| Note {
                bytes: bytes.clone(),
                modified: *modified,
                nanos: 0,
            })
            .ok_or(ReadError::Missing)
    }
    fn file(&mut self, path: &str) -> Result<i64, ReadError> {
        let file = self.0.get(path).ok_or(ReadError::Missing)?;
        Ok(file.1)
    }
}

fn written<'a>(delta: &'a Delta, out: &str) -> &'a str {
    delta
        .writes
        .iter()
        .find_map(|w| match w {
            Write::Page { out: o, html } if o == out => Some(&**html),
            _ => None,
        })
        .unwrap_or_else(|| panic!("nothing written to {out}"))
}

#[test]
fn the_home_page_lists_recent_notes_and_the_root() {
    let (_, delta) = full(&Mem::with(&[
        ("a.md", "aye"),
        ("d/b.md", "bee"),
        ("p.html", "<p>"),
    ]));
    let home = html(&delta, "");
    assert!(home.contains("<a href=\"/a\">a</a>"), "{home}");
    assert!(home.contains("<a href=\"/p.html\">p.html</a>"), "{home}");
    assert!(
        home.contains("<a href=\"/.tachylite/folders/d\">d</a>"),
        "{home}"
    );
    assert!(home.contains("<a href=\"/d/b\">b</a>"), "recent: {home}");
    // Recent is a tab, so the page links to it from the header alone.
    let main = &home[home.find("<main>").unwrap()..];
    assert!(!main.contains("/.tachylite/recent"), "{main}");
    assert_eq!(written(&delta, "pages/.tachylite/home.html"), home);
}

#[test]
fn a_folder_index_lists_its_children_by_title() {
    let (_, delta) = full(&Mem::with(&[
        ("d/zed.md", "---\ntitle: Apple\n---\n"),
        ("d/m.md", "em"),
        ("d/e/x.md", "ex"),
        ("d/img.png", "png"),
    ]));
    let page = html(&delta, ".tachylite/folders/d");
    let apple = page.find("<a href=\"/d/zed\">Apple</a>").expect(page);
    let m = page.find("<a href=\"/d/m\">m</a>").expect(page);
    assert!(apple < m, "{page}");
    assert!(
        page.contains("<a href=\"/.tachylite/folders/d/e\">e</a>"),
        "{page}"
    );
    assert!(!page.contains("img.png"), "assets are not listed: {page}");
    assert!(html(&delta, ".tachylite/folders/d/e").contains("<a href=\"/d/e/x\">x</a>"));
}

#[test]
fn dated_names_list_newest_first_above_the_rest() {
    let (_, delta) = full(&Mem::with(&[
        ("d/2026-11-08.md", "a"),
        ("d/2026-11-10 call.md", "b"),
        ("d/2026-11-09.md", "c"),
        ("d/Apple.md", "d"),
        ("d/2025-01-01/x.md", "e"),
        ("d/notes/y.md", "f"),
    ]));
    let page = html(&delta, ".tachylite/folders/d");
    let at = |text: &str| page.find(&format!(">{text}</a>")).expect(text);
    assert!(at("2026-11-10 call") < at("2026-11-09"), "{page}");
    assert!(at("2026-11-09") < at("2026-11-08"), "{page}");
    assert!(at("2026-11-08") < at("Apple"), "{page}");
    assert!(at("2025-01-01") < at("notes"), "{page}");
}

#[test]
fn subfolders_sort_ignoring_case() {
    let (_, delta) = full(&Mem::with(&[("Zeta/a.md", "a"), ("alpha/b.md", "b")]));
    let page = html(&delta, "");
    let alpha = page.find(">alpha</a>").expect(page);
    let zeta = page.find(">Zeta</a>").expect(page);
    assert!(alpha < zeta, "{page}");
}

#[test]
fn creating_a_note_re_renders_its_folder_index() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("d/b.md", "bee")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("d/c.md".into(), b"sea".to_vec());
    let delta = site.apply(vec![touch("d/c.md")], &mut mem);
    assert!(
        indexed(&delta).contains(&".tachylite/folders/d"),
        "{:?}",
        indexed(&delta)
    );
    assert!(html(&delta, ".tachylite/folders/d").contains("/d/c"));
}

#[test]
fn deleting_a_folders_last_note_deletes_its_index() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("d/b.md", "bee")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.remove("d/b.md");
    let delta = site.apply(vec![touch("d/b.md")], &mut mem);
    assert!(
        delta
            .deletes
            .contains(&"pages/.tachylite/folders/d/.index.html".to_string())
    );
    assert!(delta.pushes.contains(&Push::Gone {
        key: ".tachylite/folders/d".into()
    }));
    assert!(!html(&delta, "").contains("folders/d"));
}

#[test]
fn a_title_change_re_renders_the_folder_index() {
    let mut mem = Mem::with(&[("d/b.md", "---\ntitle: Old\n---\n")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0
        .insert("d/b.md".into(), b"---\ntitle: New\n---\n".to_vec());
    let delta = site.apply(vec![touch("d/b.md")], &mut mem);
    assert!(html(&delta, ".tachylite/folders/d").contains(">New</a>"));
}

#[test]
fn an_edit_that_keeps_titles_and_times_re_renders_no_index() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("d/b.md", "bee")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("d/b.md".into(), b"new body".to_vec());
    let delta = site.apply(vec![touch("d/b.md")], &mut mem);
    assert_eq!(pushed(&delta), ["d/b"]);
    assert!(indexed(&delta).is_empty(), "{:?}", indexed(&delta));
}

#[test]
fn recent_orders_by_modified_time_then_path() {
    let notes = [("a.md", 10), ("b.md", 30), ("c.md", 30)];
    let mut source = Timed(
        notes
            .iter()
            .map(|(p, t)| (p.to_string(), (b"x".to_vec(), *t)))
            .collect(),
    );
    let entries = notes
        .iter()
        .map(|(p, _)| Entry {
            path: p.to_string(),
            kind: classify(p, &Exclude::default()).unwrap(),
        })
        .collect();
    let delta = Site::new(Exclude::default()).apply(
        vec![Change::Sync {
            under: String::new(),
            entries,
        }],
        &mut source,
    );
    let page = html(&delta, ".tachylite/recent");
    let at = |url: &str| page.find(&format!("href=\"{url}\"")).expect(url);
    assert!(at("/b") < at("/c") && at("/c") < at("/a"), "{page}");
    assert!(page.contains("1970-01-01"), "{page}");
}

#[test]
fn an_edit_moves_its_note_to_the_top_of_recent() {
    let mut source = Timed(
        [("a.md", 10), ("b.md", 20)]
            .iter()
            .map(|(p, t)| (p.to_string(), (b"x".to_vec(), *t)))
            .collect(),
    );
    let entries = ["a.md", "b.md"]
        .iter()
        .map(|p| Entry {
            path: p.to_string(),
            kind: classify(p, &Exclude::default()).unwrap(),
        })
        .collect();
    let mut site = Site::new(Exclude::default());
    let first = site.apply(
        vec![Change::Sync {
            under: String::new(),
            entries,
        }],
        &mut source,
    );
    let page = html(&first, ".tachylite/recent");
    assert!(page.find("\"/b\"") < page.find("\"/a\""), "{page}");
    source.0.insert("a.md".into(), (b"edited".to_vec(), 30));
    let delta = site.apply(vec![touch("a.md")], &mut source);
    let page = html(&delta, ".tachylite/recent");
    assert!(page.find("\"/a\"") < page.find("\"/b\""), "{page}");
}

#[test]
fn recent_ties_sort_by_path_not_url() {
    // Path order puts `z` before `é`; URL order (`%C3%A9`) would not.
    let notes = [("é.md", 5), ("z.md", 5)];
    let mut source = Timed(
        notes
            .iter()
            .map(|(p, t)| (p.to_string(), (b"x".to_vec(), *t)))
            .collect(),
    );
    let entries = notes
        .iter()
        .map(|(p, _)| Entry {
            path: p.to_string(),
            kind: classify(p, &Exclude::default()).unwrap(),
        })
        .collect();
    let delta = Site::new(Exclude::default()).apply(
        vec![Change::Sync {
            under: String::new(),
            entries,
        }],
        &mut source,
    );
    let page = html(&delta, ".tachylite/recent");
    let at = |url: &str| page.find(&format!("href=\"{url}\"")).expect(url);
    assert!(at("/z") < at("/%C3%A9"), "{page}");
}

#[test]
fn the_switcher_list_names_every_note_with_its_aliases() {
    let (_, delta) = full(&Mem::with(&[
        ("q.md", "---\ntitle: Say \"hi\"\naliases: [Greeting]\n---\n"),
        ("d/b.md", "bee"),
    ]));
    let json = written(&delta, "files/.tachylite/notes.json");
    assert_eq!(
        json,
        r#"[["b","/d/b","d/b"],["Say \"hi\"","/q","q","Greeting"]]"#
    );
}

#[test]
fn the_manifest_lists_every_index_page() {
    let (_, delta) = full(&Mem::with(&[("a.md", "aye"), ("d/b.md", "bee")]));
    let manifest = manifest(&delta).unwrap();
    for url in [
        "/",
        "/.tachylite/recent",
        "/.tachylite/folders/d",
        "/.tachylite/notes.json",
    ] {
        assert!(
            manifest.contains(&format!("\"{url}\":\"")),
            "{url} in {manifest}"
        );
    }
}

#[test]
fn an_excluded_note_is_on_no_index_page() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("private/zebra.md", "secret")]);
    let (_, delta) = synced(&mut mem, Exclude::new(&["private".to_string()]).unwrap());
    for w in &delta.writes {
        let text = match w {
            Write::Page { html, .. } => html.to_string(),
            Write::Manifest { json } => json.clone(),
            Write::Copy { .. } | Write::Html { .. } => continue,
        };
        assert!(
            !text.contains("zebra") && !text.contains("private"),
            "{text}"
        );
    }
}

/// A source where some paths cannot be read right now: their events are still to come.
struct Flaky<'a> {
    mem: &'a Mem,
    failing: &'a BTreeMap<String, ReadError>,
}

impl Source for Flaky<'_> {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        match self.failing.get(path) {
            Some(e) => Err(*e),
            None => self.mem.clone().note(path),
        }
    }
    fn file(&mut self, path: &str) -> Result<i64, ReadError> {
        match self.failing.get(path) {
            Some(e) => Err(*e),
            None => self.mem.clone().file(path),
        }
    }
}

/// `a` embeds `b`. `a` is saved while `b` cannot be read, then `b`'s save lands.
fn embed_after_failed_read(fail: ReadError, touch_b: bool) -> Delta {
    let mut mem = Mem::with(&[("a.md", "aye ![[b]]"), ("b.md", "bee one")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert!(html(&first, "a").contains("bee one"));
    mem.0.insert("a.md".into(), b"aye edited ![[b]]".to_vec());
    let failing = BTreeMap::from([("b.md".to_string(), fail)]);
    let mut changes = vec![touch("a.md")];
    if touch_b {
        changes.push(touch("b.md"));
    }
    site.apply(
        changes,
        &mut Flaky {
            mem: &mem,
            failing: &failing,
        },
    );
    mem.0.insert("b.md".into(), b"bee two".to_vec());
    site.apply(vec![touch("b.md")], &mut mem)
}

#[test]
fn an_embed_mid_save_while_its_embedder_renders_still_reaches_it() {
    let delta = embed_after_failed_read(ReadError::MidSave, true);
    assert!(html(&delta, "a").contains("bee two"));
}

#[test]
fn an_embed_missing_while_its_embedder_renders_still_reaches_it() {
    let delta = embed_after_failed_read(ReadError::Missing, false);
    assert!(html(&delta, "a").contains("bee two"));
}

proptest! {
    #[test]
    fn failed_reads_converge_to_the_full_build(
        ops in prop::collection::vec(
            (op(), prop::collection::vec((0..PATHS.len(), any::<bool>()), 0..3)),
            1..20,
        ),
    ) {
        let mut mem = Mem::default();
        let mut site = Site::new(Exclude::default());
        site.set_nav(daily_nav(true));
        let mut out = Out::new();
        for (op, flux) in &ops {
            let mut changes = apply_op(&mut mem, op);
            // A mid-save path has an event in this batch; a missing one does not.
            let mut failing = BTreeMap::new();
            for &(p, mid_save) in flux {
                let path = PATHS[p].to_string();
                if mid_save {
                    changes.push(touch(&path));
                    failing.insert(path, ReadError::MidSave);
                } else if !changes.contains(&touch(&path)) {
                    failing.insert(path, ReadError::Missing);
                }
            }
            let mut flaky = Flaky { mem: &mem, failing: &failing };
            let delta = kept(&mut site, changes, &mut flaky, &out);
            fold(&mut out, &delta, &mem);
            // Each path in flux is then saved, and its event arrives.
            let follow: Vec<Change> = failing.keys().map(|p| touch(p)).collect();
            if !follow.is_empty() {
                apply_kept(&mut site, follow, &mut mem, &mut out);
            }
        }
        let mut fresh = Site::new(Exclude::default());
        fresh.set_nav(daily_nav(true));
        let mut expected = Out::new();
        apply_kept(&mut fresh, vec![sync_all(&mem)], &mut mem.clone(), &mut expected);
        prop_assert_eq!(&out, &expected);
    }
}

// Tag pages.

const TAG_A: &str = ".tachylite/tags/a";

/// Whether a delta deletes a tag page and pushes its gone.
fn tag_gone(delta: &Delta, tag: &str) -> bool {
    let key = format!(".tachylite/tags/{tag}");
    delta.deletes.contains(&format!("pages/{key}.html"))
        && delta.pushes.contains(&Push::Gone { key })
}

#[test]
fn a_tag_page_lists_the_notes_with_the_tag_in_any_case_and_from_frontmatter() {
    let (out, delta) = full(&Mem::with(&[
        ("x.md", "body #A here"),
        ("d/y.md", "---\ntitle: Why\ntags: [a]\n---\n"),
        ("z.md", "no tags"),
    ]));
    let page = html(&delta, TAG_A);
    assert!(page.contains("<h1>#a</h1>"), "{page}");
    assert!(page.contains("<a href=\"/x\">x</a>"), "{page}");
    assert!(page.contains("<a href=\"/d/y\">Why</a>"), "{page}");
    assert!(!page.contains("/z\""), "{page}");
    assert!(out.contains_key("pages/.tachylite/tags/a.html"));
    let manifest = manifest(&delta).unwrap();
    let version = version(&delta, TAG_A);
    assert!(
        manifest.contains(&format!("\"/{TAG_A}\":\"{version}\"")),
        "{manifest}"
    );
}

#[test]
fn a_parent_tag_page_lists_its_nested_tags_notes() {
    let (out, delta) = full(&Mem::with(&[
        ("x.md", "#a/b"),
        ("y.md", "#a"),
        ("z.md", "#ab #a-b"),
    ]));
    let parent = html(&delta, TAG_A);
    assert!(
        parent.contains("\"/x\"") && parent.contains("\"/y\""),
        "{parent}"
    );
    assert!(!parent.contains("\"/z\""), "{parent}");
    let nested = html(&delta, ".tachylite/tags/a/b");
    assert!(
        nested.contains("\"/x\"") && !nested.contains("\"/y\""),
        "{nested}"
    );
    assert!(out.contains_key("pages/.tachylite/tags/a.html"));
    assert!(out.contains_key("pages/.tachylite/tags/a/b.html"));
}

#[test]
fn a_non_ascii_tag_page_has_the_url_its_links_use() {
    let (out, delta) = full(&Mem::with(&[("x.md", "#Über")]));
    let note = html(&delta, "x");
    assert!(
        note.contains("href=\"/.tachylite/tags/%C3%BCber\""),
        "{note}"
    );
    assert!(out.contains_key("pages/.tachylite/tags/über.html"));
    assert!(
        manifest(&delta)
            .unwrap()
            .contains("\"/.tachylite/tags/%C3%BCber\":")
    );
}

#[test]
fn adding_and_removing_a_tag_updates_its_page() {
    let mut mem = Mem::with(&[("x.md", "#a"), ("y.md", "plain")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("y.md".into(), b"now #a".to_vec());
    let delta = site.apply(vec![touch("y.md")], &mut mem);
    assert!(html(&delta, TAG_A).contains("\"/y\""));
    mem.0.insert("y.md".into(), b"plain again".to_vec());
    let delta = site.apply(vec![touch("y.md")], &mut mem);
    let page = html(&delta, TAG_A);
    assert!(
        page.contains("\"/x\"") && !page.contains("\"/y\""),
        "{page}"
    );
}

#[test]
fn removing_a_tags_last_note_deletes_its_page() {
    let mut mem = Mem::with(&[("x.md", "#a/b"), ("y.md", "#c")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.remove("x.md");
    let delta = site.apply(vec![touch("x.md")], &mut mem);
    assert!(
        tag_gone(&delta, "a") && tag_gone(&delta, "a/b"),
        "{delta:?}"
    );
    assert!(!manifest(&delta).unwrap().contains("tags/a"));
    mem.0.insert("y.md".into(), b"untagged".to_vec());
    let delta = site.apply(vec![touch("y.md")], &mut mem);
    assert!(tag_gone(&delta, "c"), "{delta:?}");
}

#[test]
fn renaming_a_tagged_note_moves_it_on_its_tag_page() {
    let mut mem = Mem::with(&[("x.md", "#a"), ("y.md", "#a")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    let bytes = mem.0.remove("x.md").unwrap();
    mem.0.insert("w.md".into(), bytes);
    let delta = site.apply(vec![touch("x.md"), touch("w.md")], &mut mem);
    let page = html(&delta, TAG_A);
    assert!(
        page.contains("\"/w\"") && !page.contains("\"/x\""),
        "{page}"
    );
}

#[test]
fn a_retitled_note_rewrites_its_tag_pages_only() {
    let mut mem = Mem::with(&[("x.md", "---\ntitle: Old\n---\n#a"), ("y.md", "#b")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0
        .insert("x.md".into(), b"---\ntitle: New\n---\n#a".to_vec());
    let delta = site.apply(vec![touch("x.md")], &mut mem);
    assert!(html(&delta, TAG_A).contains(">New</a>"));
    let tags: Vec<&str> = indexed(&delta)
        .into_iter()
        .filter(|k| k.starts_with(".tachylite/tags/"))
        .collect();
    assert_eq!(tags, [TAG_A]);
}

#[test]
fn an_edit_that_keeps_tags_and_title_writes_no_tag_page() {
    let mut mem = Mem::with(&[("x.md", "#a one"), ("y.md", "#a")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("x.md".into(), b"#a two".to_vec());
    let delta = site.apply(vec![touch("x.md")], &mut mem);
    assert_eq!(pushed(&delta), ["x"]);
    assert!(indexed(&delta).is_empty(), "{:?}", indexed(&delta));
}

#[test]
fn an_excluded_notes_tag_has_no_page_and_its_note_is_on_none() {
    let mut mem = Mem::with(&[
        ("a.md", "#shared"),
        ("private/zebra.md", "#shared #secretive"),
    ]);
    let (_, delta) = synced(&mut mem, Exclude::new(&["private".to_string()]).unwrap());
    assert!(!html(&delta, ".tachylite/tags/shared").contains("zebra"));
    assert!(!leaks(&delta));
    for w in &delta.writes {
        let (out, text) = match w {
            Write::Page { out, html } => (out.as_str(), html.to_string()),
            Write::Manifest { json } => ("", json.clone()),
            Write::Copy { .. } | Write::Html { .. } => continue,
        };
        assert!(
            !out.contains("secretive") && !text.contains("secretive"),
            "{out}"
        );
    }
}

#[test]
fn excluding_a_tags_only_note_removes_its_page() {
    let mut mem = Mem::with(&[("a.md", "x"), ("private/z.md", "#solo")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    site.set_exclude(Exclude::new(&["private".to_string()]).unwrap());
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    assert!(tag_gone(&delta, "solo"), "{delta:?}");
}

#[test]
fn a_tag_too_long_for_its_page_is_text_and_the_build_goes_on() {
    let long = "t".repeat(251);
    let fits = "f".repeat(250);
    let deep = ["d".repeat(100), "e".repeat(100), "g".repeat(100)].join("/");
    let body = format!("#{long} #{fits} #{deep} #ok");
    let (out, delta) = full(&Mem::with(&[("x.md", &body)]));
    let note = html(&delta, "x");
    assert!(
        !note.contains(&format!("tags/{long}")),
        "a link to a page with no file"
    );
    assert!(note.contains(&format!("#{long}")));
    assert!(
        !note.contains("tags/ddd"),
        "a nested tag too long for its page's path"
    );
    assert!(out.contains_key(&format!("pages/.tachylite/tags/{fits}.html")));
    assert!(out.contains_key("pages/.tachylite/tags/ok.html"));
}

#[test]
fn an_embedded_notes_tags_are_not_its_embedders() {
    let (_, delta) = full(&Mem::with(&[("a.md", "![[b]]"), ("b.md", "#t")]));
    assert!(
        html(&delta, "a").contains("tags/t"),
        "the embed shows the tag"
    );
    let page = html(&delta, ".tachylite/tags/t");
    assert!(
        page.contains("\"/b\"") && !page.contains("\"/a\""),
        "{page}"
    );
}

/// A source that records each note it reads.
struct Counted(Mem, Vec<String>);

impl Source for Counted {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        self.1.push(path.to_string());
        self.0.note(path)
    }
    fn file(&mut self, path: &str) -> Result<i64, ReadError> {
        self.0.file(path)
    }
}

#[test]
fn a_page_whose_backlinks_alone_change_is_not_rendered_again() {
    let mut mem = Mem::with(&[("b.md", "# b body"), ("c.md", "[[b]]")]);
    let mut site = Site::new(Exclude::default());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    // A new linker, a retitle, and a removed linker each change only `b`'s footer.
    let steps: [(&str, Option<&str>); 3] = [
        ("a.md", Some("[[b]]")),
        ("a.md", Some("---\ntitle: Ay\n---\n[[b]]")),
        ("c.md", None),
    ];
    for (path, text) in steps {
        match text {
            Some(text) => mem.0.insert(path.into(), text.as_bytes().to_vec()),
            None => mem.0.remove(path),
        };
        let mut source = Counted(mem.clone(), Vec::new());
        let delta = kept(&mut site, vec![touch(path)], &mut source, &out);
        fold(&mut out, &delta, &mem);
        assert!(!source.1.contains(&"b.md".to_string()), "after {path}");
        assert!(pushed(&delta).contains(&"b"), "after {path}");
        assert_eq!(out, full(&mem).0, "after {path}");
    }
}

#[test]
fn a_saved_note_pushes_again_only_when_a_dependent_changes_its_page() {
    // A new note that a dependent links to once it renders: its page gains the backlink.
    let mut mem = Mem::with(&[("a.md", "[[b]]")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("b.md".into(), b"# b".to_vec());
    let delta = site.apply(vec![touch("b.md")], &mut mem);
    assert_eq!(pushed(&delta), ["b", "a", "b"]);
    let pages: Vec<&str> = delta
        .pushes
        .iter()
        .filter_map(|p| match p {
            Push::Page { key, html, .. } if key == "b" => Some(&**html),
            _ => None,
        })
        .collect();
    assert!(!pages[0].contains("backlinks"), "{}", pages[0]);
    assert!(
        pages[1].contains("<li><a href=\"/a\">a</a></li>"),
        "{}",
        pages[1]
    );

    // A retitle whose shown title stays the same leaves its target's page as it was.
    let mut mem = Mem::with(&[("a.md", "---\ntitle: a\n---\n[[b]]"), ("b.md", "# b")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("a.md".into(), b"[[b]]".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(pushed(&delta), ["a"]);
}

#[test]
fn a_broken_link_is_reported_once_until_it_changes() {
    let mut mem = Mem::with(&[("a.md", "[[x]] [[y]]")]);
    let (mut site, first) = synced(&mut mem, Exclude::default());
    assert_eq!(
        first.broken,
        [broken("a.md", "x", 0), broken("a.md", "y", 0)]
    );
    // `y.md` re-renders `a`, whose link to `x` stays broken.
    mem.0.insert("y.md".into(), b"# y".to_vec());
    let delta = site.apply(vec![touch("y.md")], &mut mem);
    assert!(pushed(&delta).contains(&"a"));
    assert!(delta.broken.is_empty(), "{:?}", delta.broken);
    // A root sync of an unchanged vault reports none.
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    assert!(delta.broken.is_empty(), "{:?}", delta.broken);
    // Removing `y.md` breaks the link again.
    mem.0.remove("y.md");
    let delta = site.apply(vec![touch("y.md")], &mut mem);
    assert_eq!(delta.broken, [broken("a.md", "y", 0)]);
}

/// A sink that records the order of writes and pushes.
#[derive(Default)]
struct Ordered(Vec<String>);

impl Sink for Ordered {
    fn delete(&mut self, _: String) {}
    fn write(&mut self, write: Write) {
        if let Write::Page { out, .. } | Write::Copy { out, .. } | Write::Html { out, .. } = write {
            self.0.push(format!("write {out}"));
        }
    }
    fn push(&mut self, push: Push) {
        if let Push::Page { key, .. } = push {
            self.0.push(format!("push {key}"));
        }
    }
    fn page(&mut self, _: &str) -> Option<String> {
        None
    }
    fn keeps(&self) -> bool {
        false
    }
}

#[test]
fn a_pushed_page_links_to_no_page_of_its_apply_not_yet_written() {
    let mut mem = Mem::with(&[("old.md", "# old")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    // One batch saves a note that links to a new note and a new html page.
    mem.0
        .insert("a.md".into(), b"[[z]] [[f.html]] [[old]]".to_vec());
    mem.0.insert("z.md".into(), b"# z".to_vec());
    mem.0.insert("f.html".into(), b"<p>f</p>".to_vec());
    let mut sink = Ordered::default();
    let changes = ["a.md", "z.md", "f.html"].map(touch).to_vec();
    site.apply_to(changes, &mut mem.clone(), &mut sink);
    let at = |event: &str| sink.0.iter().position(|e| e == event).unwrap();
    assert!(at("write pages/z.html") < at("push a"), "{:?}", sink.0);
    assert!(at("write files/f.html") < at("push a"), "{:?}", sink.0);
}

/// 300 notes pass the bodies this site holds within their first batch of 256, so a full
/// sync writes its oldest pages early. The notes of the next batch link to them, so those
/// pages take their final footer from the page written.
#[test]
fn a_full_build_past_the_held_bodies_equals_small_applies() {
    let names: Vec<String> = (0..300).map(|i| format!("n{i:03}")).collect();
    let links: String = names[..16].iter().map(|n| format!("[[{n}]] ")).collect();
    let filler = "word ".repeat(100);
    let notes: Vec<(String, String)> = names
        .iter()
        .map(|n| {
            (
                format!("{n}.md"),
                format!("# {n}\n\n{links}\n\n{filler}{n}\n"),
            )
        })
        .collect();
    let pairs: Vec<(&str, &str)> = notes
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let mut mem = Mem::with(&pairs);
    let mut held = Site::new(Exclude::default());
    held.hold_at_most(64 * 1024);
    let mut want = Out::new();
    let delta = apply_kept(&mut held, vec![sync_all(&mem)], &mut mem, &mut want);
    // The first page was written before its backlinks were final, then again.
    let written = delta
        .writes
        .iter()
        .filter(|w| matches!(w, Write::Page { out, .. } if out == "pages/n000.html"));
    assert_eq!(written.count(), 2);
    let first = String::from_utf8_lossy(&want["pages/n000.html"]).into_owned();
    assert_eq!(first.matches("<li><a href=").count(), 299);

    let mut site = Site::new(Exclude::default());
    let mut got = Out::new();
    for (path, _) in &notes {
        apply_kept(&mut site, vec![touch(path)], &mut mem, &mut got);
    }
    assert_eq!(got.len(), want.len());
    for (path, html) in &want {
        assert!(got.get(path) == Some(html), "{path} differs");
    }
}

fn daily(folder: &str, format: &str) -> tachylite::config::Daily {
    tachylite::config::parse(&format!(
        "daily = {{ folder = {folder:?}, format = {format:?} }}"
    ))
    .unwrap()
    .nav
    .daily
    .unwrap()
}

fn nav(text: &str) -> tachylite::config::Nav {
    tachylite::config::parse(text).unwrap().nav
}

fn page_text(out: &Out, path: &str) -> String {
    String::from_utf8_lossy(out.get(path).map_or(&[][..], Vec::as_slice)).into_owned()
}

const TODAY: &str = "pages/.tachylite/today.html";

/// The dates the Today page lists, in its order.
fn days(out: &Out) -> Vec<String> {
    let html = page_text(out, TODAY);
    html.match_indices("data-date=\"")
        .map(|(at, m)| html[at + m.len()..at + m.len() + 10].to_string())
        .collect()
}

#[test]
fn the_today_page_lists_daily_notes_newest_first() {
    let mut mem = Mem::with(&[
        ("journal/2026/2026-11-02.md", "a"),
        ("journal/2026/2026-11-10.md", "b"),
        ("journal/2025/2025-12-31.md", "c"),
        ("journal/2026/2026-11-10 copy.md", "not a date"),
        ("journal/2026/2026-13-01.md", "no month 13"),
        ("journal/2026/private.md", "not a date"),
        ("2026-11-11.md", "outside the folder"),
    ]);
    let mut site = Site::new(Exclude::default());
    site.set_obsidian(Some(daily("journal", "YYYY/YYYY-MM-DD")));
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    assert_eq!(days(&out), ["2026-11-10", "2026-11-02", "2025-12-31"]);
    assert!(page_text(&out, TODAY).contains("href=\"/journal/2026/2026-11-10\""));
    assert!(manifest_urls(&out).contains("/.tachylite/today"));
    // Every page links to it.
    assert!(page_text(&out, "pages/journal/2026/2026-11-02.html").contains("/.tachylite/today"));

    // Create, rename and delete each move it.
    mem.0
        .insert("journal/2026/2026-11-12.md".into(), b"d".to_vec());
    fold(
        &mut out,
        &site.apply(vec![touch("journal/2026/2026-11-12.md")], &mut mem),
        &mem,
    );
    assert_eq!(days(&out)[0], "2026-11-12");
    let body = mem.0.remove("journal/2026/2026-11-12.md").unwrap();
    mem.0.insert("journal/2026/2026-11-13.md".into(), body);
    let changes = vec![
        touch("journal/2026/2026-11-12.md"),
        touch("journal/2026/2026-11-13.md"),
    ];
    fold(&mut out, &site.apply(changes, &mut mem), &mem);
    assert_eq!(days(&out)[..2], ["2026-11-13", "2026-11-10"]);
    mem.0.remove("journal/2026/2026-11-13.md");
    fold(
        &mut out,
        &site.apply(vec![touch("journal/2026/2026-11-13.md")], &mut mem),
        &mem,
    );
    assert_eq!(days(&out)[0], "2026-11-10");
}

#[test]
fn an_excluded_daily_note_is_not_on_the_today_page() {
    let mut mem = Mem::with(&[
        ("d/2026-11-02.md", "a"),
        ("d/2026-11-03.secret.md", HUSHED),
        ("private/2026-11-04.md", HUSHED),
    ]);
    let mut site = Site::new(hush());
    site.set_obsidian(Some(daily("", "YYYY-MM-DD")));
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    assert!(!leaks(&delta));
    let mut site = Site::new(hush());
    site.set_obsidian(Some(daily("private", "YYYY-MM-DD")));
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    assert!(!leaks(&delta));
}

#[test]
fn the_today_page_keeps_the_newest_sixty() {
    let files: Vec<(String, &str)> = (1..=28)
        .flat_map(|d| (1..=3).map(move |m| (format!("2026-{m:02}-{d:02}.md"), "x")))
        .collect();
    let files: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), *c)).collect();
    let mut mem = Mem::with(&files);
    let mut site = Site::new(Exclude::default());
    site.set_nav(nav("daily = { format = \"YYYY-MM-DD\" }"));
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    let listed = days(&out);
    assert_eq!(listed.len(), 60);
    assert_eq!(
        (listed[0].as_str(), listed[59].as_str()),
        ("2026-03-28", "2026-01-25")
    );
}

#[test]
fn no_daily_config_means_no_today_page_or_link() {
    let mut mem = Mem::with(&[("2026-11-02.md", "a")]);
    let (out, _) = full(&mem);
    assert!(!out.contains_key(TODAY));
    assert!(!page_text(&out, "pages/2026-11-02.html").contains("today"));

    // A format with a token Tachylite does not read is no config, and says why.
    let mut site = Site::new(Exclude::default());
    site.set_nav(nav("daily = { format = \"dddd, MMMM Do YYYY\" }"));
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    let mut out = Out::new();
    fold(&mut out, &delta, &mem);
    assert!(!out.contains_key(TODAY));
    assert_eq!(delta.notices.len(), 1, "{:?}", delta.notices);
    assert!(delta.notices[0].contains("\"dddd\""), "{:?}", delta.notices);
}

#[test]
fn an_excluded_daily_folder_means_no_today_page_or_link() {
    let mut mem = Mem::with(&[("a.md", "a"), ("private/2026-11-04.md", HUSHED)]);
    let mut site = Site::new(hush());
    site.set_obsidian(Some(daily("private", "YYYY-MM-DD")));
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    let mut out = Out::new();
    fold(&mut out, &delta, &mem);
    assert!(!out.contains_key(TODAY));
    assert!(!page_text(&out, "pages/a.html").contains("today"));
    assert_eq!(delta.notices.len(), 1, "{:?}", delta.notices);
    assert!(
        delta.notices[0].contains("\"private\" is excluded"),
        "{:?}",
        delta.notices
    );
}

#[test]
fn the_config_daily_overrides_obsidian_and_a_change_takes_effect() {
    let mut mem = Mem::with(&[("a/2026-11-02.md", "a"), ("b/02.11.2026.md", "b")]);
    let mut site = Site::new(Exclude::default());
    site.set_obsidian(Some(daily("a", "YYYY-MM-DD")));
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    assert!(page_text(&out, TODAY).contains("/a/2026-11-02"));
    site.set_nav(nav("daily = { folder = \"b/\", format = \"DD.MM.YYYY\" }"));
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    assert!(page_text(&out, TODAY).contains("/b/02.11.2026"));
    assert!(!page_text(&out, TODAY).contains("/a/"));
    // Losing the daily config takes the page and every page's link with it.
    site.set_nav(nav(""));
    site.set_obsidian(None);
    let mut kept = Kept {
        delta: Delta::default(),
        out: &out,
    };
    site.apply_to(vec![sync_all(&mem)], &mut mem.clone(), &mut kept);
    site.derive_to(&mut kept.delta);
    let delta = kept.delta;
    fold(&mut out, &delta, &mem);
    assert!(!out.contains_key(TODAY));
    assert!(
        out.values()
            .all(|page| !String::from_utf8_lossy(page).contains("/.tachylite/today"))
    );
    assert!(
        delta
            .pushes
            .iter()
            .any(|p| matches!(p, Push::Gone { key } if key == ".tachylite/today"))
    );
}

#[test]
fn a_folder_recent_page_lists_its_newest_notes_at_any_depth() {
    let mut mem = Mem::with(&[
        ("m/a.md", "a"),
        ("m/x/b.md", "b"),
        ("m/x/y/c.md", "c"),
        ("other/d.md", "d"),
        ("m/x/e.secret.md", HUSHED),
    ]);
    for (i, path) in ["m/a.md", "m/x/b.md", "m/x/y/c.md", "other/d.md"]
        .iter()
        .enumerate()
    {
        mem.1.insert(path.to_string(), i as i64 * 100);
    }
    let mut site = Site::new(hush());
    let mut out = Out::new();
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    assert!(!leaks(&delta));
    fold(&mut out, &delta, &mem);
    let order = |out: &Out, page: &str| -> Vec<String> {
        let html = page_text(out, page);
        let main = &html[html.find("<ul class=\"recent\">").unwrap()..];
        main.match_indices("<li><a href=\"")
            .map(|(at, m)| main[at + m.len()..].split('"').next().unwrap().to_string())
            .collect()
    };
    let m = "pages/.tachylite/recent/m/.index.html";
    assert_eq!(order(&out, m), ["/m/x/y/c", "/m/x/b", "/m/a"]);
    assert_eq!(
        order(&out, "pages/.tachylite/recent/m/x/.index.html"),
        ["/m/x/y/c", "/m/x/b"]
    );
    assert!(manifest_urls(&out).contains("/.tachylite/recent/m/x/y"));
    assert!(
        page_text(&out, "pages/.tachylite/folders/m/.index.html")
            .contains("href=\"/.tachylite/recent/m\"")
    );

    // A save moves its note to the top of each folder above it, and of no other.
    mem.1.insert("m/a.md".into(), 1000);
    mem.0.insert("m/a.md".into(), b"a2".to_vec());
    let delta = site.apply(vec![touch("m/a.md")], &mut mem);
    fold(&mut out, &delta, &mem);
    assert_eq!(order(&out, m), ["/m/a", "/m/x/y/c", "/m/x/b"]);
    let pushed: Vec<&str> = delta
        .pushes
        .iter()
        .filter_map(|p| match p {
            Push::Page { key, .. } => Some(key.as_str()),
            Push::Gone { .. } => None,
        })
        .collect();
    assert!(pushed.contains(&".tachylite/recent/m"), "{pushed:?}");
    assert!(
        !pushed
            .iter()
            .any(|k| k.starts_with(".tachylite/recent/m/x")),
        "{pushed:?}"
    );

    // A folder with no note left loses its page.
    mem.0.remove("other/d.md");
    let delta = site.apply(vec![touch("other/d.md")], &mut mem);
    assert!(
        delta
            .deletes
            .contains(&"pages/.tachylite/recent/other/.index.html".to_string())
    );
}

#[test]
fn a_folder_recent_page_keeps_the_newest_fifty() {
    let files: Vec<String> = (0..120).map(|i| format!("f/{i:03}.md")).collect();
    let pairs: Vec<(&str, &str)> = files.iter().map(|p| (p.as_str(), "x")).collect();
    let mut mem = Mem::with(&pairs);
    for (i, path) in files.iter().enumerate() {
        mem.1.insert(path.clone(), i as i64);
    }
    let (out, _) = full(&mem);
    let html = page_text(&out, "pages/.tachylite/recent/f/.index.html");
    assert_eq!(html.matches("<li>").count(), 50);
    assert!(html.contains("/f/119\"") && html.contains("/f/070\"") && !html.contains("/f/069\""));
}

#[test]
fn pins_come_from_config_alone() {
    let mut mem = Mem::with(&[
        ("people/Jane Doe.md", "---\ntitle: Jane\n---\nx"),
        ("maps/route.html", "<p>map</p>"),
        ("a.md", "a"),
        ("img/x.png", "png"),
        ("private/p.md", HUSHED),
    ]);
    let mut site = Site::new(hush());
    site.set_nav(nav(
        "title = \"Notes & co\"\npins = [\"people/Jane Doe.md\", \"people/|Team\", \"maps/route.html\", \"private/p.md\", \"gone.md\", \"img\"]",
    ));
    let mut out = Out::new();
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    assert!(!leaks(&delta), "an excluded pin reached the output");
    fold(&mut out, &delta, &mem);
    let page = page_text(&out, "pages/a.html");
    assert!(!page.contains("Notes &amp; co"), "{page}");
    assert!(
        page.contains(
            "<nav class=\"pins\"><a href=\"/people/Jane%20Doe\">Jane Doe</a>\
         <a href=\"/.tachylite/folders/people\">Team</a>\
         <a href=\"/maps/route.html\">route</a><a href=\"/gone\">gone</a>\
         <a href=\"/.tachylite/folders/img\">img</a></nav>"
        ),
        "{page}"
    );
    assert!(page_text(&out, HOME).contains("<title>Notes &amp; co</title>"));
    let notices = delta.notices.join("\n");
    assert!(
        notices.contains("\"private/p.md\" is excluded"),
        "{notices}"
    );
    assert!(
        notices.contains("\"gone.md\" matches no published"),
        "{notices}"
    );
    assert!(
        notices.contains("\"img\" matches no published"),
        "{notices}"
    );
    assert_eq!(delta.notices.len(), 3, "{notices}");

    // Retitling a pinned note changes its own page, and no other note's.
    mem.0.insert(
        "people/Jane Doe.md".into(),
        b"---\ntitle: J. Doe\n---\nx".to_vec(),
    );
    let delta = site.apply(vec![touch("people/Jane Doe.md")], &mut mem);
    let notes: Vec<_> = pushed(&delta)
        .into_iter()
        .filter(|k| !k.is_empty() && !k.starts_with(".tachylite"))
        .collect();
    assert_eq!(notes, ["people/Jane Doe"]);
    assert!(delta.notices.is_empty());
}

#[test]
fn a_nav_change_rewrites_every_page_with_the_new_header() {
    let mem = Mem::with(&[("a.md", "#t [[b]]"), ("d/b.md", "#t/u b")]);
    let mut site = Site::new(Exclude::default());
    let mut out = Out::new();
    fold(
        &mut out,
        &site.apply(vec![sync_all(&mem)], &mut mem.clone()),
        &mem,
    );
    let pages_before = out.keys().filter(|k| k.starts_with("pages/")).count();
    site.set_nav(nav("pins = [\"d\"]"));
    let mut kept = Kept {
        delta: Delta::default(),
        out: &out,
    };
    site.apply_to(vec![sync_all(&mem)], &mut mem.clone(), &mut kept);
    site.derive_to(&mut kept.delta);
    let delta = kept.delta;
    fold(&mut out, &delta, &mem);
    let written = delta
        .writes
        .iter()
        .filter(|w| matches!(w, Write::Page { out, .. } if out.starts_with("pages/")))
        .count();
    assert_eq!(written, pages_before);
    let mut expected_site = Site::new(Exclude::default());
    expected_site.set_nav(nav("pins = [\"d\"]"));
    let mut expected = Out::new();
    fold(
        &mut expected,
        &expected_site.apply(vec![sync_all(&mem)], &mut mem.clone()),
        &mem,
    );
    assert_eq!(out, expected);
}

const RECENT: &str = "pages/.tachylite/recent.html";

const TASKS: &str = "pages/.tachylite/tasks.html";

/// A page's header, as written.
fn header_of(out: &Out, path: &str) -> String {
    let html = page_text(out, path);
    let start = html.find("<header>").expect(path);
    let end = html.find("</header>").expect(path);
    html[start..end].to_string()
}

#[test]
fn a_tab_page_marks_its_own_tab_and_no_other_page_marks_any() {
    let mut mem = Mem::with(&[("a.md", "#t a"), ("d/2026-11-02.md", "b")]);
    let mut site = Site::new(Exclude::default());
    site.set_nav(nav(
        "title = \"Notes\"\ndaily = { folder = \"d\" }\npins = [\"a.md\"]",
    ));
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);

    let plain = "<header><a href=\"/\">Home</a><a href=\"/.tachylite/today\">Today</a>\
                 <a href=\"/.tachylite/recent\">Recent</a><a href=\"/.tachylite/tasks\">Tasks</a>\
                 <button type=\"button\" class=\"find\" title=\"Find a note (Ctrl-K)\">Find</button>\
                 <nav class=\"pins\"><a href=\"/a\">a</a></nav>";
    let tabs = [
        (HOME, "/"),
        (TODAY, "/.tachylite/today"),
        (RECENT, "/.tachylite/recent"),
        (TASKS, "/.tachylite/tasks"),
    ];
    for (page, url) in tabs {
        let link = format!("<a href=\"{url}\">");
        let marked = format!("<a href=\"{url}\" aria-current=\"page\">");
        assert_eq!(
            header_of(&out, page),
            plain.replacen(&link, &marked, 1),
            "{page}"
        );
    }
    let others: Vec<&String> = out
        .keys()
        .filter(|p| p.starts_with("pages/") && !tabs.iter().any(|(tab, _)| tab == p))
        .collect();
    assert_eq!(
        others.len(),
        5,
        "a folder index, a folder recent page, a tag page, two notes"
    );
    for page in others {
        assert_eq!(header_of(&out, page), plain, "{page}");
    }

    // The title names the home page, and no header.
    let home = page_text(&out, HOME);
    assert!(home.contains("<title>Notes</title>"), "{home}");
    assert!(home.contains("<h1 class=\"sr\">Notes</h1>"), "{home}");
    assert!(!header_of(&out, HOME).contains("Notes"));
}

#[test]
fn with_no_daily_settings_the_tab_bar_has_no_today() {
    let (out, _) = full(&Mem::with(&[("a.md", "a")]));
    assert_eq!(
        header_of(&out, "pages/a.html"),
        "<header><a href=\"/\">Home</a><a href=\"/.tachylite/recent\">Recent</a>\
         <a href=\"/.tachylite/tasks\">Tasks</a>\
         <button type=\"button\" class=\"find\" title=\"Find a note (Ctrl-K)\">Find</button>"
    );
    assert!(header_of(&out, HOME).contains("<a href=\"/\" aria-current=\"page\">Home</a>"));
}

#[test]
fn a_save_a_retitle_and_a_new_daily_note_change_no_header() {
    let mut mem = Mem::with(&[("a.md", "[[b]]"), ("b.md", "b"), ("d/2026-11-02.md", "x")]);
    let mut site = Site::new(Exclude::default());
    site.set_nav(nav("daily = { folder = \"d\" }\npins = [\"b.md\"]"));
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    let headers = |out: &Out| -> BTreeMap<String, String> {
        out.keys()
            .filter(|p| p.starts_with("pages/"))
            .map(|p| (p.clone(), header_of(out, p)))
            .collect()
    };
    let mut before = headers(&out);

    mem.0.insert("a.md".into(), b"edited [[b]]".to_vec());
    mem.0
        .insert("b.md".into(), b"---\ntitle: Bee\n---\nb".to_vec());
    mem.0.insert("d/2026-11-03.md".into(), b"y".to_vec());
    let changes = vec![touch("a.md"), touch("b.md"), touch("d/2026-11-03.md")];
    apply_kept(&mut site, changes, &mut mem, &mut out);
    let after = headers(&out);
    before.insert(
        "pages/d/2026-11-03.html".into(),
        before["pages/a.html"].clone(),
    );
    assert_eq!(after, before);
}

/// Each block of a page's `<main>`: its `<h2>` and the URLs its list links to, in order.
fn blocks(html: &str) -> Vec<(String, Vec<String>)> {
    let main = &html[html.find("<main>").unwrap()..];
    main.split("<h2>")
        .skip(1)
        .map(|block| {
            let (heading, rest) = block.split_once("</h2>").unwrap();
            let list = rest.split("</ul>").next().unwrap();
            let urls = list
                .match_indices("<li><a href=\"")
                .map(|(at, m)| list[at + m.len()..].split('"').next().unwrap().to_string())
                .collect();
            (heading.to_string(), urls)
        })
        .collect()
}

/// A vault of five daily notes in `d` and the notes `n1` to `n7`, with modified times. The
/// oldest daily note by date was modified last, and `n7` is the newest of the rest.
fn daily_vault() -> Mem {
    let mut mem = Mem::default();
    let days = [
        ("d/2026-11-01.md", 200),
        ("d/2026-11-02.md", 101),
        ("d/2026-11-03.md", 102),
        ("d/2026-11-04.md", 103),
        ("d/2026-11-05.md", 104),
    ];
    let notes = (1..=7).map(|i| (format!("n{i}.md"), 10 + i));
    for (path, time) in days.iter().map(|(p, t)| (p.to_string(), *t)).chain(notes) {
        mem.0.insert(path.clone(), b"x".to_vec());
        mem.1.insert(path, time);
    }
    mem
}

fn daily_site(exclude: Exclude) -> Site {
    let mut site = Site::new(exclude);
    site.set_nav(nav("daily = { folder = \"d\" }"));
    site
}

#[test]
fn the_home_page_lists_five_recent_notes_without_the_three_newest_daily_notes() {
    let mut mem = daily_vault();
    let mut site = daily_site(Exclude::default());
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    let home = page_text(&out, HOME);
    let urls = ["/d/2026-11-01", "/d/2026-11-02", "/n7", "/n6", "/n5"];
    assert_eq!(
        blocks(&home),
        [("Recent".to_string(), urls.map(String::from).to_vec())]
    );
    assert!(!home.contains("All recent notes"), "{home}");
    // The vault root follows.
    assert!(home.contains("<ul class=\"folders\"><li><a href=\"/.tachylite/folders/d\">"));
    assert!(
        home.contains("<ul class=\"notes\"><li><a href=\"/n1\">"),
        "{home}"
    );

    // A new daily note takes a place among the three, and the oldest of them joins the list.
    mem.0.insert("d/2026-11-06.md".into(), b"x".to_vec());
    mem.1.insert("d/2026-11-06.md".into(), 300);
    let delta = apply_kept(
        &mut site,
        vec![touch("d/2026-11-06.md")],
        &mut mem,
        &mut out,
    );
    let urls = [
        "/d/2026-11-01",
        "/d/2026-11-03",
        "/d/2026-11-02",
        "/n7",
        "/n6",
    ];
    assert_eq!(blocks(&page_text(&out, HOME))[0].1, urls);
    assert!(indexed(&delta).contains(&""), "{:?}", indexed(&delta));
}

#[test]
fn with_no_daily_settings_the_home_page_lists_the_five_most_recent_notes() {
    let (out, _) = full(&daily_vault());
    let urls = [
        "/d/2026-11-01",
        "/d/2026-11-05",
        "/d/2026-11-04",
        "/d/2026-11-03",
        "/d/2026-11-02",
    ];
    assert_eq!(blocks(&page_text(&out, HOME))[0].1, urls);
}

#[test]
fn the_recent_page_lists_the_three_newest_daily_notes_then_the_other_notes() {
    let mut mem = daily_vault();
    let mut site = daily_site(Exclude::default());
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    let urls = |urls: &[&str]| urls.iter().map(|u| u.to_string()).collect::<Vec<_>>();
    let recent = [
        "/d/2026-11-01",
        "/d/2026-11-02",
        "/n7",
        "/n6",
        "/n5",
        "/n4",
        "/n3",
        "/n2",
        "/n1",
    ];
    assert_eq!(
        blocks(&page_text(&out, RECENT)),
        [
            (
                "Daily".to_string(),
                urls(&["/d/2026-11-05", "/d/2026-11-04", "/d/2026-11-03"])
            ),
            ("Recent".to_string(), urls(&recent)),
        ]
    );
    // A row reads the same in both blocks: the title, then the folder and the date.
    assert!(page_text(&out, RECENT).contains(
        "<li><a href=\"/d/2026-11-05\">2026-11-05</a> <span class=\"meta\">d · 1970-01-01</span></li>"
    ));

    // Create, rename and delete each move the blocks. The create shows the push and the
    // manifest's new version.
    let listed = |out: &Out| page_text(out, tachylite::site::MANIFEST);
    let before = listed(&out);
    mem.0.insert("d/2026-11-06.md".into(), b"x".to_vec());
    let delta = apply_kept(
        &mut site,
        vec![touch("d/2026-11-06.md")],
        &mut mem,
        &mut out,
    );
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(
        page[0].1,
        ["/d/2026-11-06", "/d/2026-11-05", "/d/2026-11-04"]
    );
    assert_eq!(
        page[1].1[..3],
        ["/d/2026-11-01", "/d/2026-11-03", "/d/2026-11-02"]
    );
    assert!(indexed(&delta).contains(&".tachylite/recent"));
    assert_ne!(listed(&out), before);

    let body = mem.0.remove("d/2026-11-06.md").unwrap();
    mem.0.insert("d/2026-10-31.md".into(), body);
    let changes = vec![touch("d/2026-11-06.md"), touch("d/2026-10-31.md")];
    apply_kept(&mut site, changes, &mut mem, &mut out);
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(
        page[0].1,
        ["/d/2026-11-05", "/d/2026-11-04", "/d/2026-11-03"]
    );
    assert!(page[1].1.contains(&"/d/2026-10-31".to_string()));

    for day in ["05", "04", "03", "02"] {
        mem.0.remove(&format!("d/2026-11-{day}.md"));
    }
    let changes = ["05", "04", "03", "02"].map(|day| touch(&format!("d/2026-11-{day}.md")));
    apply_kept(&mut site, changes.to_vec(), &mut mem, &mut out);
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(page[0].1, ["/d/2026-11-01", "/d/2026-10-31"]);
    assert_eq!(page[1].1[0], "/n7");

    // With no other note, the page has no Recent block.
    let rest: Vec<String> = (1..=7).map(|i| format!("n{i}.md")).collect();
    for path in &rest {
        mem.0.remove(path);
    }
    let changes = rest.iter().map(|path| touch(path)).collect();
    apply_kept(&mut site, changes, &mut mem, &mut out);
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(page.len(), 1, "{page:?}");
    assert_eq!(page[0].0, "Daily");
}

#[test]
fn the_recent_page_has_no_daily_block_without_daily_settings_or_daily_notes() {
    let (out, _) = full(&daily_vault());
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(page.len(), 1, "{page:?}");
    assert_eq!(page[0].0, "Recent");
    assert_eq!(page[0].1[0], "/d/2026-11-01");

    let mut mem = Mem::with(&[("a.md", "a"), ("d/not a date.md", "b")]);
    let mut site = daily_site(Exclude::default());
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(page.len(), 1, "{page:?}");
    assert_eq!(page[0].0, "Recent");
}

#[test]
fn the_recent_page_keeps_fifty_notes_besides_the_daily_block() {
    let mut mem = daily_vault();
    for i in 0..60 {
        mem.0.insert(format!("m/{i:02}.md"), b"x".to_vec());
        mem.1.insert(format!("m/{i:02}.md"), 1000 + i);
    }
    let mut site = daily_site(Exclude::default());
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(page[0].1.len(), 3);
    assert_eq!(page[1].1.len(), 50);
    assert_eq!(
        (page[1].1[0].as_str(), page[1].1[49].as_str()),
        ("/m/59", "/m/10")
    );
}

#[test]
fn an_excluded_daily_note_is_in_no_daily_block_and_nowhere_on_the_home_page() {
    let mut mem = daily_vault();
    mem.0
        .insert("d/2026-11-06.md".into(), HUSHED.as_bytes().to_vec());
    mem.1.insert("d/2026-11-06.md".into(), 500);
    let exclude = || Exclude::new(&["d/2026-11-06.md".to_string()]).unwrap();
    let mut site = daily_site(exclude());
    let mut out = Out::new();
    let delta = site.apply(vec![sync_all(&mem)], &mut mem);
    fold(&mut out, &delta, &mem);
    assert!(!leaks(&delta));
    assert!(
        out.values()
            .all(|page| !String::from_utf8_lossy(page).contains("2026-11-06"))
    );
    let page = blocks(&page_text(&out, RECENT));
    assert_eq!(
        page[0].1,
        ["/d/2026-11-05", "/d/2026-11-04", "/d/2026-11-03"]
    );
    assert_eq!(
        blocks(&page_text(&out, HOME))[0].1[..2],
        ["/d/2026-11-01", "/d/2026-11-02"]
    );
}

/// The daily notes a page links to as its previous and its next, each as `url date`.
fn neighbours(out: &Out, path: &str) -> [Option<String>; 2] {
    let html = page_text(out, path);
    ["prev", "next"].map(|rel| {
        let open = format!("<a rel=\"{rel}\" href=\"");
        let link = &html[html.find(&open)? + open.len()..];
        let (url, rest) = link.split_once("\" aria-label")?;
        let (_, rest) = rest.split_once("\">")?;
        Some(format!("{url} {}", rest.split_once("</a>")?.0))
    })
}

fn day(day: &str) -> Option<String> {
    Some(format!("/d/2026-11-{day} 2026-11-{day}"))
}

fn day_page(day: &str) -> String {
    format!("pages/d/2026-11-{day}.html")
}

#[test]
fn a_daily_note_page_links_to_the_previous_and_the_next_daily_note() {
    let mut mem = daily_vault();
    mem.0.insert("d/sub/n.md".into(), b"x".to_vec());
    let mut site = daily_site(Exclude::default());
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    assert_eq!(neighbours(&out, &day_page("01")), [None, day("02")]);
    assert_eq!(neighbours(&out, &day_page("03")), [day("02"), day("04")]);
    assert_eq!(neighbours(&out, &day_page("05")), [day("04"), None]);
    // The line sits under the breadcrumbs, above the note.
    let page = page_text(&out, &day_page("03"));
    assert!(
        page.contains(
            "<main>\n<nav class=\"crumbs\"><a href=\"/.tachylite/folders/d\">d</a></nav>\n\
             <nav class=\"days\" aria-label=\"Daily notes\">\
             <a rel=\"prev\" href=\"/d/2026-11-02\" aria-label=\"Previous: 2026-11-02\">2026-11-02</a>\
             <a rel=\"next\" href=\"/d/2026-11-04\" aria-label=\"Next: 2026-11-04\">2026-11-04</a></nav>\n<p>x</p>"
        ),
        "{page}"
    );
    // No other page has one.
    for path in ["pages/n1.html", "pages/d/sub/n.html", HOME, RECENT, TODAY] {
        assert!(!page_text(&out, path).contains("class=\"days\""), "{path}");
    }
}

#[test]
fn a_lone_daily_note_has_no_line_and_a_link_reads_as_an_iso_date() {
    let mut mem = Mem::with(&[("j/02.11.2026.md", "x")]);
    let mut site = Site::new(Exclude::default());
    site.set_nav(nav("daily = { folder = \"j\", format = \"DD.MM.YYYY\" }"));
    let mut out = Out::new();
    fold(&mut out, &site.apply(vec![sync_all(&mem)], &mut mem), &mem);
    assert!(!page_text(&out, "pages/j/02.11.2026.html").contains("class=\"days\""));

    mem.0.insert("j/30.10.2026.md".into(), b"y".to_vec());
    apply_kept(
        &mut site,
        vec![touch("j/30.10.2026.md")],
        &mut mem,
        &mut out,
    );
    assert_eq!(
        neighbours(&out, "pages/j/02.11.2026.html"),
        [Some("/j/30.10.2026 2026-10-30".to_string()), None]
    );
    assert_eq!(
        neighbours(&out, "pages/j/30.10.2026.html"),
        [None, Some("/j/02.11.2026 2026-11-02".to_string())]
    );
}

/// Applies `changes` and checks that exactly `notes` were pushed, that none of `unread` was
/// read again, that each pushed page has a new version in the manifest, and that the output
/// equals a fresh full build.
fn day_step(
    site: &mut Site,
    mem: &Mem,
    out: &mut Out,
    changes: Vec<Change>,
    notes: &[&str],
    unread: &[&str],
) {
    let listed = |out: &Out| -> BTreeMap<String, String> {
        let json = page_text(out, tachylite::site::MANIFEST);
        let pages = &json[r#"{"pages":{"#.len()..json.find(r#"},"files""#).unwrap()];
        pages
            .split(',')
            .filter_map(|pair| pair.split_once(':'))
            .map(|(url, version)| (url.to_string(), version.to_string()))
            .collect()
    };
    let before = listed(out);
    let mut source = Counted(mem.clone(), Vec::new());
    let delta = kept(site, changes, &mut source, out);
    fold(out, &delta, mem);
    let mut got = pushed(&delta);
    got.sort_unstable();
    assert_eq!(got, notes);
    for path in unread {
        assert!(!source.1.contains(&path.to_string()), "{path} was read");
    }
    let after = listed(out);
    for note in notes {
        let url = format!("\"/{note}\"");
        assert_ne!(after.get(&url), before.get(&url), "{note}");
        assert!(after.contains_key(&url), "{note}");
    }
    let mut fresh = daily_site(Exclude::default());
    let mut want = Out::new();
    apply_kept(&mut fresh, vec![sync_all(mem)], &mut mem.clone(), &mut want);
    assert_eq!(*out, want);
}

#[test]
fn a_daily_note_that_comes_or_goes_rewrites_its_two_neighbours_unrendered() {
    let mut mem = daily_vault();
    let mut site = daily_site(Exclude::default());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);

    // A new newest daily note: the one before it gains a next link.
    mem.0.insert("d/2026-11-07.md".into(), b"x".to_vec());
    let changes = vec![touch("d/2026-11-07.md")];
    let pushed = ["d/2026-11-05", "d/2026-11-07"];
    day_step(
        &mut site,
        &mem,
        &mut out,
        changes,
        &pushed,
        &["d/2026-11-05.md"],
    );
    assert_eq!(neighbours(&out, &day_page("05")), [day("04"), day("07")]);

    // A note created between two: both link to it, and no other page changes.
    mem.0.insert("d/2026-11-06.md".into(), b"x".to_vec());
    let changes = vec![touch("d/2026-11-06.md")];
    let pushed = ["d/2026-11-05", "d/2026-11-06", "d/2026-11-07"];
    let unread = ["d/2026-11-05.md", "d/2026-11-07.md"];
    day_step(&mut site, &mem, &mut out, changes, &pushed, &unread);
    assert_eq!(neighbours(&out, &day_page("06")), [day("05"), day("07")]);

    // A deleted note: its neighbours link to each other.
    mem.0.remove("d/2026-11-03.md");
    let changes = vec![touch("d/2026-11-03.md")];
    let pushed = ["d/2026-11-02", "d/2026-11-04"];
    let unread = ["d/2026-11-02.md", "d/2026-11-04.md"];
    day_step(&mut site, &mem, &mut out, changes, &pushed, &unread);
    assert_eq!(neighbours(&out, &day_page("02")), [day("01"), day("04")]);
    assert_eq!(neighbours(&out, &day_page("04")), [day("02"), day("05")]);

    // A renamed note: its old neighbours and its new ones.
    let body = mem.0.remove("d/2026-11-06.md").unwrap();
    mem.0.insert("d/2026-10-31.md".into(), body);
    let changes = vec![touch("d/2026-11-06.md"), touch("d/2026-10-31.md")];
    let pushed = [
        "d/2026-10-31",
        "d/2026-11-01",
        "d/2026-11-05",
        "d/2026-11-07",
    ];
    let unread = ["d/2026-11-01.md", "d/2026-11-05.md", "d/2026-11-07.md"];
    day_step(&mut site, &mem, &mut out, changes, &pushed, &unread);
    assert_eq!(
        neighbours(&out, &day_page("01")),
        [Some("/d/2026-10-31 2026-10-31".to_string()), day("02")]
    );

    // A save that keeps the name changes no neighbour.
    mem.0.insert("d/2026-11-04.md".into(), b"edited".to_vec());
    let changes = vec![touch("d/2026-11-04.md")];
    day_step(&mut site, &mem, &mut out, changes, &["d/2026-11-04"], &[]);
}

#[test]
fn an_excluded_daily_note_is_no_neighbour() {
    let mut mem = daily_vault();
    let exclude = || Exclude::new(&["d/2026-11-03.md".to_string()]).unwrap();
    let mut site = daily_site(exclude());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert_eq!(neighbours(&out, &day_page("02")), [day("01"), day("04")]);
    assert_eq!(neighbours(&out, &day_page("04")), [day("02"), day("05")]);
    assert!(
        out.values()
            .all(|page| !String::from_utf8_lossy(page).contains("2026-11-03"))
    );

    // Publishing it and excluding it again each move its neighbours' links.
    site.set_exclude(Exclude::default());
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert_eq!(neighbours(&out, &day_page("02")), [day("01"), day("03")]);
    assert_eq!(neighbours(&out, &day_page("04")), [day("03"), day("05")]);
    site.set_exclude(exclude());
    let delta = apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert_eq!(neighbours(&out, &day_page("02")), [day("01"), day("04")]);
    assert_eq!(neighbours(&out, &day_page("04")), [day("02"), day("05")]);
    assert!(pushed(&delta).contains(&"d/2026-11-02") && pushed(&delta).contains(&"d/2026-11-04"));
    assert!(
        out.values()
            .all(|page| !String::from_utf8_lossy(page).contains("2026-11-03"))
    );
}

#[test]
fn a_change_of_daily_settings_moves_every_daily_line() {
    let mut mem = Mem::with(&[
        ("a/2026-11-02.md", "x"),
        ("a/2026-11-03.md", "x"),
        ("b/02.11.2026.md", "x"),
        ("b/04.11.2026.md", "x"),
    ]);
    let mut site = Site::new(Exclude::default());
    site.set_obsidian(Some(daily("a", "YYYY-MM-DD")));
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert!(page_text(&out, "pages/a/2026-11-02.html").contains("class=\"days\""));
    assert!(!page_text(&out, "pages/b/02.11.2026.html").contains("class=\"days\""));

    site.set_nav(nav("daily = { folder = \"b\", format = \"DD.MM.YYYY\" }"));
    let delta = apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert!(!page_text(&out, "pages/a/2026-11-02.html").contains("class=\"days\""));
    assert_eq!(
        neighbours(&out, "pages/b/02.11.2026.html"),
        [None, Some("/b/04.11.2026 2026-11-04".to_string())]
    );
    let mut got = pushed(&delta);
    got.sort_unstable();
    let all = [
        "a/2026-11-02",
        "a/2026-11-03",
        "b/02.11.2026",
        "b/04.11.2026",
    ];
    assert_eq!(got, all);

    // With no daily settings, no page has a line.
    site.set_nav(nav(""));
    site.set_obsidian(None);
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert!(
        out.values()
            .all(|page| !String::from_utf8_lossy(page).contains("class=\"days\""))
    );
    assert_eq!(out, full(&mem).0);
}

// Search shards.

/// Where the search shard of the note at `path` is written.
fn shard_out(path: &str) -> String {
    let shard = tachylite::indexes::shard(path);
    format!("files/{}", tachylite::indexes::shard_key(shard))
}

/// The search shards a delta writes.
fn shard_writes(delta: &Delta) -> Vec<&str> {
    let shards = delta.writes.iter().filter_map(|w| match w {
        Write::Page { out, .. } if out.starts_with(SEARCH) => Some(out.as_str()),
        _ => None,
    });
    shards.collect()
}

/// Two note paths that share a search shard.
fn shard_mates() -> (String, String) {
    let mut seen = BTreeMap::new();
    let mate = |n: u32| {
        let path = format!("n{n}.md");
        let first = seen.insert(tachylite::indexes::shard(&path), path.clone());
        first.map(|first| (first, path))
    };
    (0..).find_map(mate).unwrap()
}

#[test]
fn a_delta_holds_no_search_shard_and_a_sink_that_keeps_pages_builds_them_all() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("b.md", "bee")]);
    let (mut site, delta) = synced(&mut mem, Exclude::default());
    let mut out = Out::new();
    fold(&mut out, &delta, &mem);
    assert!(!out.keys().any(|p| p.starts_with(SEARCH)), "{out:?}");
    assert_eq!(manifest_urls(&out), page_urls(&out));
    // The shards stay due, so the next derive through a sink that keeps pages builds each.
    mem.0.insert("c.md".into(), b"sea".to_vec());
    apply_kept(&mut site, vec![touch("c.md")], &mut mem, &mut out);
    assert_eq!(out, full(&mem).0);
}

#[test]
fn a_search_shard_follows_the_modified_time_and_an_unchanged_touch_writes_none() {
    let mut mem = Mem::with(&[("a.md", "aye"), ("b.md", "bee")]);
    mem.1.insert("a.md".into(), 100);
    let mut site = Site::new(Exclude::default());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert_eq!(out[&shard_out("a.md")], b"/a\t100\taye\n");

    let delta = apply_kept(&mut site, vec![touch("a.md")], &mut mem, &mut out);
    assert!(delta.writes.is_empty() && delta.deletes.is_empty());

    // A save that changes no byte still moves the note up the ranking.
    mem.1.insert("a.md".into(), 200);
    let delta = apply_kept(&mut site, vec![touch("a.md")], &mut mem, &mut out);
    assert_eq!(shard_writes(&delta), [shard_out("a.md")]);
    assert_eq!(out[&shard_out("a.md")], b"/a\t200\taye\n");
    assert_eq!(out, full(&mem).0);
}

#[test]
fn a_search_shard_drops_a_deleted_note_and_goes_with_its_last_note() {
    let (first, second) = shard_mates();
    let mut mem = Mem::with(&[(&first, "one"), (&second, "two"), ("other.md", "three")]);
    let mut site = Site::new(Exclude::default());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    let shard = shard_out(&first);
    assert_eq!(String::from_utf8_lossy(&out[&shard]).lines().count(), 2);

    mem.0.remove(&first);
    let delta = apply_kept(&mut site, vec![touch(&first)], &mut mem, &mut out);
    assert_eq!(shard_writes(&delta), [&shard]);
    let url = format!("/{}", second.trim_end_matches(".md"));
    assert_eq!(out[&shard], format!("{url}\t0\ttwo\n").as_bytes());

    mem.0.remove(&second);
    let delta = apply_kept(&mut site, vec![touch(&second)], &mut mem, &mut out);
    assert!(delta.deletes.contains(&shard), "{:?}", delta.deletes);
    assert!(shard_writes(&delta).is_empty());
    assert_eq!(manifest_urls(&out), page_urls(&out));
    assert_eq!(out, full(&mem).0);
}

/// The board as lines: `# title` for a task list, `status: text` for a task, and `(text)`
/// for a line of counts or the line of task lists not shown.
fn board(out: &Out) -> Vec<String> {
    let html = page_text(out, TASKS);
    let main = &html[html.find("<main>").expect("a board")..html.find("</main>").unwrap()];
    let text = |s: &str| {
        let mut t = String::new();
        let mut tag = false;
        for c in s.chars() {
            match c {
                '<' => tag = true,
                '>' => tag = false,
                c if !tag => t.push(c),
                _ => {}
            }
        }
        t.trim().to_string()
    };
    let mut lines = Vec::new();
    let mut rest = main;
    loop {
        let starts = [
            ("<h2>", "</h2>"),
            ("<li data-task=\"", "</li>"),
            ("<p", "</p>"),
        ];
        let Some((at, (open, close))) = starts
            .iter()
            .filter_map(|(o, c)| Some((rest.find(o)?, (*o, *c))))
            .min_by_key(|(at, _)| *at)
        else {
            break;
        };
        let end = at + rest[at..].find(close).unwrap();
        let inner = &rest[at + open.len()..end];
        lines.push(match open {
            "<h2>" => format!("# {}", text(inner)),
            "<p" => format!("({})", text(&format!("<p{inner}"))),
            _ => {
                let (status, line) = inner.split_once('"').unwrap();
                format!("{status}: {}", text(&format!("<{line}")))
            }
        });
        rest = &rest[end + close.len()..];
    }
    lines
}

/// A model vault whose notes have the modified times given, in seconds.
fn timed(files: &[(&str, &str, i64)]) -> Mem {
    let pairs: Vec<(&str, &str)> = files.iter().map(|(p, c, _)| (*p, *c)).collect();
    let mut mem = Mem::with(&pairs);
    for (path, _, time) in files {
        mem.1.insert(path.to_string(), *time);
    }
    mem
}

#[test]
fn the_board_puts_task_lists_with_a_doing_task_first_then_newest_first() {
    let mem = timed(&[
        ("old.md", "- [ ] sow\n- [/] dig", 1),
        ("new.md", "- [ ] weed", 5),
        (
            "mid.md",
            "---\ntitle: Middle\n---\n- [ ] water\n- [x] mow\n- [-] paint",
            3,
        ),
        ("also mid.md", "- [ ] rake", 3),
        ("closed.md", "- [x] all done", 9),
        ("none.md", "no tasks", 9),
    ]);
    let (out, _) = full(&mem);
    assert_eq!(
        board(&out),
        [
            "# old",
            "todo: sow",
            "doing: dig",
            "# new",
            "todo: weed",
            "# also mid",
            "todo: rake",
            "# Middle",
            "todo: water",
            "(1 done · 1 cancelled)",
        ]
    );
}

#[test]
fn a_task_list_shows_its_doing_tasks_then_todo_from_the_top_up_to_five() {
    let lines = |tasks: &[&str]| tasks.concat();
    let mem = Mem::with(&[
        (
            "a.md",
            &lines(&[
                "- [ ] t1\n",
                "- [ ] t2\n",
                "- [/] d1\n",
                "- [ ] t3\n",
                "- [ ] t4\n",
                "- [/] d2\n",
                "- [ ] t5\n",
                "- [x] x\n",
            ]),
        ),
        ("b.md", &"- [/] d\n".repeat(6)),
        ("c.md", "- [ ] t\n- [ ] u\n"),
    ]);
    let (out, _) = full(&mem);
    let board = board(&out);
    let a = board.iter().position(|l| l == "# a").unwrap();
    assert_eq!(
        board[a..a + 7],
        [
            "# a",
            "todo: t1",
            "todo: t2",
            "doing: d1",
            "todo: t3",
            "doing: d2",
            "(2 more · 1 done)",
        ]
    );
    let b = board.iter().position(|l| l == "# b").unwrap();
    // More than five doing tasks show whole, with no todo and no line of more.
    assert_eq!(board[b + 1..b + 7], ["doing: d"; 6]);
    assert_eq!(board[b + 7], "# c");
    let html = page_text(&out, TASKS);
    assert!(html.contains("<a href=\"/a\">2 more</a>"), "{html}");
}

#[test]
fn the_board_shows_fifty_task_lists_and_counts_the_rest() {
    let notes: Vec<(String, String, i64)> = (0..53)
        .map(|i| (format!("n{i:02}.md"), "- [ ] t".to_string(), i))
        .collect();
    let files: Vec<(&str, &str, i64)> = notes
        .iter()
        .map(|(p, c, t)| (p.as_str(), c.as_str(), *t))
        .collect();
    let (out, _) = full(&timed(&files));
    let board = board(&out);
    let lists: Vec<&String> = board.iter().filter(|l| l.starts_with('#')).collect();
    assert_eq!(lists.len(), 50);
    assert_eq!((lists[0].as_str(), lists[49].as_str()), ("# n52", "# n03"));
    assert_eq!(
        board.last().unwrap(),
        "(3 more task lists with open tasks not shown.)"
    );
}

#[test]
fn a_vault_with_no_open_task_still_has_a_board() {
    let (out, _) = full(&Mem::with(&[("a.md", "- [x] done")]));
    assert_eq!(board(&out), ["(No open tasks.)"]);
    let urls = manifest_urls(&out);
    assert!(urls.contains("/.tachylite/tasks"), "{urls:?}");
}

#[test]
fn each_edit_of_a_task_reaches_the_board() {
    let mut mem = timed(&[("a.md", "# A", 1), ("b.md", "- [ ] keep", 2)]);
    let mut site = Site::new(Exclude::default());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    let mut save = |mem: &mut Mem, path: &str, text: Option<&str>, out: &mut Out| {
        match text {
            Some(text) => {
                let time = mem.1.values().max().unwrap() + 1;
                mem.1.insert(path.into(), time);
                mem.0.insert(path.into(), text.as_bytes().to_vec());
            }
            None => {
                mem.0.remove(path);
            }
        }
        let delta = apply_kept(&mut site, vec![touch(path)], mem, out);
        indexed(&delta).contains(&".tachylite/tasks")
    };
    let steps: [(&str, Option<&str>, &[&str]); 7] = [
        (
            "a.md",
            Some("- [ ] sow"),
            &["# a", "todo: sow", "# b", "todo: keep"],
        ),
        (
            "a.md",
            Some("- [/] sow"),
            &["# a", "doing: sow", "# b", "todo: keep"],
        ),
        (
            "a.md",
            Some("- [/] sow *now*"),
            &["# a", "doing: sow now", "# b", "todo: keep"],
        ),
        (
            "b.md",
            Some("- [ ] keep\n- [x] done"),
            &["# a", "doing: sow now", "# b", "todo: keep", "(1 done)"],
        ),
        (
            "a.md",
            Some("- [x] sow"),
            &["# b", "todo: keep", "(1 done)"],
        ),
        ("b.md", None, &["(No open tasks.)"]),
        ("c.md", Some("- [ ] new"), &["# c", "todo: new"]),
    ];
    for (path, text, want) in steps {
        assert!(
            save(&mut mem, path, text, &mut out),
            "{path} {text:?} pushed no board"
        );
        assert_eq!(board(&out), want, "{path} {text:?}");
    }
    // A rename is a delete and a create.
    let bytes = mem.0.remove("c.md").unwrap();
    mem.0.insert("d/e.md".into(), bytes);
    mem.1.insert("d/e.md".into(), 99);
    apply_kept(
        &mut site,
        vec![touch("c.md"), touch("d/e.md")],
        &mut mem,
        &mut out,
    );
    assert_eq!(board(&out), ["# e", "todo: new"]);
    assert!(page_text(&out, TASKS).contains("href=\"/d/e\""));
}

#[test]
fn a_link_on_a_task_line_resolves_again_when_its_target_comes() {
    let mut mem = Mem::with(&[("a.md", "- [ ] see [[b]]")]);
    let mut site = Site::new(Exclude::default());
    let mut out = Out::new();
    apply_kept(&mut site, vec![sync_all(&mem)], &mut mem, &mut out);
    assert!(page_text(&out, TASKS).contains("<a class=\"broken-link\">b</a>"));
    mem.0.insert("b.md".into(), b"# B".to_vec());
    apply_kept(&mut site, vec![touch("b.md")], &mut mem, &mut out);
    assert!(page_text(&out, TASKS).contains("see <a href=\"/b\">b</a>"));
}

#[test]
fn raw_html_forges_no_task_on_the_board() {
    let (out, _) = full(&Mem::with(&[
        (
            "forged.md",
            "<ul><li data-task=\"doing\">forged</li></ul>\n",
        ),
        (
            "a.md",
            "- [ ] real <b>bold</b></li></ul><li data-task=\"doing\">x[^1]\n\n[^1]: note\n",
        ),
        ("b.md", "# Top\n\n- [ ] up to [[#Top]]\n"),
    ]));
    let html = page_text(&out, TASKS);
    assert!(!html.contains("forged") && !html.contains("<b>"), "{html}");
    assert_eq!(html.matches("data-task").count(), 2, "{html}");
    assert!(!html.contains("footnote"), "{html}");
    assert!(html.contains("real bold"), "{html}");
    // A link to a heading of the note itself leads into the note.
    assert!(html.contains("<a href=\"/b#top\">"), "{html}");
}
