#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use proptest::prelude::*;
use tachylite::config::Exclude;
use tachylite::render::Broken;
use tachylite::site::{Change, Delta, Note, Push, Site, Source, Write};
use tachylite::vault::{Entry, ReadError, SkipReason, Skipped, classify};

/// An in-memory vault: path to bytes, and the modified times of the notes that set one.
/// Folders are implied by paths.
#[derive(Default, Clone)]
struct Mem(BTreeMap<String, Vec<u8>>, BTreeMap<String, i64>);

impl Source for Mem {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        let bytes = self.0.get(path).cloned().ok_or(ReadError::Missing)?;
        let modified = self.1.get(path).copied().unwrap_or_default();
        Ok(Note { bytes, modified })
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.0.get(path).map(|_| ()).ok_or(ReadError::Missing)
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
            Write::Manifest { json } => {
                out.insert(tachylite::site::MANIFEST.into(), json.as_bytes().to_vec());
            }
        }
    }
}

const HOME: &str = "pages/.tachylite/home.html";

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

/// The URLs of the pages written: every file under `pages/`, and the switcher list.
fn page_urls(out: &Out) -> std::collections::BTreeSet<String> {
    out.keys()
        .filter(|p| p.starts_with("pages/"))
        .map(|p| format!("/{}", tachylite::render::encode(page_key(p))))
        .chain(
            out.contains_key("files/.tachylite/notes.json")
                .then(|| "/.tachylite/notes.json".into()),
        )
        .collect()
}

/// Where a pushed page's key is written.
fn page_out(key: &str) -> String {
    if key.is_empty() {
        HOME.into()
    } else if key.starts_with(".tachylite/folders/") {
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
            Write::Page { out, .. } | Write::Copy { out, .. } => !generated(out),
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
    let mut source = mem.clone();
    let delta = Site::new(exclude).apply(vec![sync_all(mem)], &mut source);
    let mut out = Out::new();
    fold(&mut out, &delta, mem);
    (out, delta)
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
    let (mut site, first) = synced(&mut mem, Exclude::default());
    let mut out = Out::new();
    fold(&mut out, &first, &mem);
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
        let delta = site.apply(changes, &mut mem);
        fold(&mut out, &delta, &mem);
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
        fn file(&mut self, _: &str) -> Result<(), ReadError> {
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

fn html<'a>(delta: &'a Delta, key: &str) -> &'a str {
    delta
        .pushes
        .iter()
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
    assert_eq!(pushed(&delta), ["a", "c"]);
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
    assert_eq!(pushed(&delta), ["a", "b"]);
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
    assert_eq!(pushed(&delta), ["x/r", "y/a", "z/r"]);
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
    assert_eq!(pushed(&delta), ["a", "p/jane"]);
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
    assert_eq!(pushed(&delta), ["a", "p/jane"]);
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
    assert_eq!(pushed(&delta), ["a", "p/jane", "q/jd"]);
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

const PATHS: [&str; 13] = [
    "a.md",
    "a.MD",
    "x.html",
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
];

/// What the exclusion property excludes: `private/s.md` and `d/k.secret.md`, wherever
/// a move takes it.
fn hush() -> Exclude {
    Exclude::new(&["private".to_string(), "**/*.secret.md".to_string()]).unwrap()
}

/// Text only an excluded note holds.
const HUSHED: &str = "hush-marker";

const CONTENTS: [&str; 15] = [
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
];

/// A source where every file is mid-save.
struct MidSave;

impl Source for MidSave {
    fn note(&mut self, _: &str) -> Result<Note, ReadError> {
        Err(ReadError::MidSave)
    }
    fn file(&mut self, _: &str) -> Result<(), ReadError> {
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
        fn file(&mut self, _: &str) -> Result<(), ReadError> {
            Err(ReadError::Missing)
        }
    }
    let delta = site.apply(vec![touch("a.MD")], &mut Mixed);
    assert_eq!(delta.deletes, ["pages/a.html"]);
    assert!(files(&delta).is_empty());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(files(&delta).len(), 1, "its save publishes it");
}

#[derive(Debug, Clone)]
enum Op {
    Write(usize, usize),
    Delete(usize),
    DeleteDir,
    MoveDir,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..PATHS.len(), 0..CONTENTS.len()).prop_map(|(p, c)| Op::Write(p, c)),
        2 => (0..PATHS.len()).prop_map(Op::Delete),
        1 => Just(Op::DeleteDir),
        1 => Just(Op::MoveDir),
    ]
}

/// Applies an op to the model vault and returns the changes the watcher would report.
/// Each write gets the next modified time, so recent order varies.
fn apply_op(mem: &mut Mem, op: &Op) -> Vec<Change> {
    match op {
        Op::Write(p, c) => {
            let path = PATHS[*p].to_string();
            let mut bytes = CONTENTS[*c].as_bytes().to_vec();
            if hush().path(&path) {
                bytes.extend_from_slice(format!(" {HUSHED}").as_bytes());
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
    fn incremental_equals_full(ops in prop::collection::vec(op(), 1..30), batch in 1usize..4) {
        let mut mem = Mem::default();
        let mut site = Site::new(hush());
        let mut out = Out::new();
        let mut last_push: BTreeMap<String, Push> = BTreeMap::new();

        for chunk in ops.chunks(batch) {
            let mut changes = Vec::new();
            for op in chunk {
                changes.extend(apply_op(&mut mem, op));
            }
            let mut source = mem.clone();
            let delta = site.apply(changes, &mut source);
            fold(&mut out, &delta, &mem);
            prop_assert!(!leaks(&delta), "an excluded note reached the output");
            for push in delta.pushes {
                let key = match &push {
                    Push::Page { key, .. } | Push::Gone { key } => key.clone(),
                };
                last_push.insert(key, push);
            }
        }

        let (expected, _) = full_with(&mem, hush());
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
    let mut source = mem.clone();
    let mut got = Out::new();
    for group in notes.chunks(16) {
        let delta = site.apply(group.iter().map(|(p, _)| touch(p)).collect(), &mut source);
        fold(&mut got, &delta, &mem);
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
    assert_eq!(pushed(&delta), ["a", "b", "c"]);
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

/// The manifest's note entries, as `{"pages":{…}}`, without the generated pages.
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

#[test]
fn the_manifest_lists_every_note_page_with_its_version() {
    let mem = Mem::with(&[
        ("b.md", "bee"),
        ("a.md", "aye"),
        ("sub/c d.md", "sea"),
        ("img.png", "png"),
        ("page.html", "<p>page</p>"),
    ]);
    let (out, delta) = full(&mem);
    let want = format!(
        r#"{{"pages":{{"/a":"{}","/b":"{}","/sub/c%20d":"{}"}}}}"#,
        version(&delta, "a"),
        version(&delta, "b"),
        version(&delta, "sub/c d"),
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
fn the_manifest_lists_every_html_page_and_asset_without_a_version() {
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

/// An in-memory vault whose notes carry modified times.
struct Timed(BTreeMap<String, (Vec<u8>, i64)>);

impl Source for Timed {
    fn note(&mut self, path: &str) -> Result<Note, ReadError> {
        self.0
            .get(path)
            .map(|(bytes, modified)| Note {
                bytes: bytes.clone(),
                modified: *modified,
            })
            .ok_or(ReadError::Missing)
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.0.get(path).map(|_| ()).ok_or(ReadError::Missing)
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
    assert!(home.contains("<a href=\"/.tachylite/recent\">"), "{home}");
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
            Write::Copy { .. } => continue,
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
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
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
            let delta = site.apply(changes, &mut Flaky { mem: &mem, failing: &failing });
            fold(&mut out, &delta, &mem);
            // Each path in flux is then saved, and its event arrives.
            let follow: Vec<Change> = failing.keys().map(|p| touch(p)).collect();
            if !follow.is_empty() {
                let delta = site.apply(follow, &mut mem.clone());
                fold(&mut out, &delta, &mem);
            }
        }
        let (expected, _) = full(&mem);
        prop_assert_eq!(&out, &expected);
    }
}
