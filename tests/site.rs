#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use proptest::prelude::*;
use tachylite::config::Exclude;
use tachylite::render::Broken;
use tachylite::site::{Change, Delta, Push, Site, Source, Write};
use tachylite::vault::{Entry, ReadError, SkipReason, Skipped, classify};

/// An in-memory vault: path to bytes. Folders are implied by paths.
#[derive(Default, Clone)]
struct Mem(BTreeMap<String, Vec<u8>>);

impl Source for Mem {
    fn note(&mut self, path: &str) -> Result<Vec<u8>, ReadError> {
        self.0.get(path).cloned().ok_or(ReadError::Missing)
    }
    fn file(&mut self, path: &str) -> Result<(), ReadError> {
        self.0.get(path).map(|_| ()).ok_or(ReadError::Missing)
    }
}

impl Mem {
    fn with(files: &[(&str, &str)]) -> Mem {
        Mem(files
            .iter()
            .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
            .collect())
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
        }
    }
}

fn sync_all(mem: &Mem) -> Change {
    Change::Sync {
        under: String::new(),
        entries: mem.entries_under(""),
    }
}

fn full(mem: &Mem) -> (Out, Delta) {
    let mut source = mem.clone();
    let delta = Site::new(Exclude::default()).apply(vec![sync_all(mem)], &mut source);
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
        out.keys().collect::<Vec<_>>(),
        ["files/img/x.png", "pages/a.html", "pages/d/b.html"]
    );
    let pages = delta
        .writes
        .iter()
        .filter(|w| matches!(w, Write::Page { .. }))
        .count();
    let copies = delta
        .writes
        .iter()
        .filter(|w| matches!(w, Write::Copy { .. }))
        .count();
    assert_eq!((pages, copies), (2, 1));
    let keys: Vec<_> = delta
        .pushes
        .iter()
        .map(|p| match p {
            Push::Page { key, .. } => key.as_str(),
            Push::Gone { key } => panic!("unexpected gone for {key}"),
        })
        .collect();
    assert_eq!(keys, ["a", "d/b"]);
    assert!(delta.deletes.is_empty());
    assert!(delta.skipped.is_empty());
}

#[test]
fn unchanged_touch_writes_and_pushes_nothing() {
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert!(delta.writes.is_empty() && delta.pushes.is_empty() && delta.deletes.is_empty());
}

#[test]
fn edited_note_is_written_and_pushed() {
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    mem.0.insert("a.md".into(), b"# Edited".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(delta.writes.len(), 1);
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
    assert!(matches!(&delta.pushes[..], [Push::Gone { key }] if key == "a"));
}

#[test]
fn html_page_beats_note_with_its_url() {
    let mem = Mem::with(&[("x.html", "<p>page</p>"), ("x.html.md", "# note")]);
    let (out, delta) = full(&mem);
    assert_eq!(out.keys().collect::<Vec<_>>(), ["files/x.html"]);
    assert_eq!(delta.skipped, [clash("x.html.md")]);
}

#[test]
fn folder_page_clash() {
    let mem = Mem::with(&[("x.md", "# x"), ("x.html/y.md", "# y")]);
    let (out, delta) = full(&mem);
    assert_eq!(out.keys().collect::<Vec<_>>(), ["pages/x.html/y.html"]);
    assert_eq!(delta.skipped, [clash("x.md")]);
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
    assert!(delta.writes.is_empty() && delta.pushes.is_empty() && delta.deletes.is_empty());
}

#[test]
fn unreadable_note_is_skipped_and_unpublished() {
    struct Denied;
    impl Source for Denied {
        fn note(&mut self, _: &str) -> Result<Vec<u8>, ReadError> {
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

/// The keys of the pages a delta pushes, in order.
fn pushed(delta: &Delta) -> Vec<&str> {
    delta
        .pushes
        .iter()
        .filter_map(|p| match p {
            Push::Page { key, .. } => Some(key.as_str()),
            Push::Gone { .. } => None,
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
    assert_eq!(delta.writes.len(), 2);
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
fn unchanged_backlinks_render_nothing() {
    let mut mem = Mem::with(&[("a.md", "[[b]] one"), ("b.md", "# B")]);
    let (mut site, _) = synced(&mut mem, Exclude::default());
    mem.0.insert("a.md".into(), b"[[b]] two".to_vec());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(pushed(&delta), ["a"]);
}

const PATHS: [&str; 10] = [
    "a.md",
    "a.MD",
    "x.html",
    "x.html.md",
    "x.md",
    "x.html/y.md",
    "d/n.md",
    "d/p.png",
    "d/e/m.md",
    ".h.md",
];
const CONTENTS: [&str; 12] = [
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
];

/// A source where every file is mid-save.
struct MidSave;

impl Source for MidSave {
    fn note(&mut self, _: &str) -> Result<Vec<u8>, ReadError> {
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
        delta.writes.len(),
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
    assert_eq!(delta.writes.len(), 1);
}

#[test]
fn mid_save_loser_does_not_block_promotion() {
    let mut mem = Mem::with(&[("a.MD", "# upper"), ("a.md", "# lower")]);
    let mut site = Site::new(Exclude::default());
    site.apply(vec![sync_all(&mem)], &mut mem);
    // a.MD wins; deleting it promotes a.md, which is mid-save.
    struct Mixed;
    impl Source for Mixed {
        fn note(&mut self, path: &str) -> Result<Vec<u8>, ReadError> {
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
    assert!(delta.writes.is_empty());
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert_eq!(delta.writes.len(), 1, "its save publishes it");
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
fn apply_op(mem: &mut Mem, op: &Op) -> Vec<Change> {
    match op {
        Op::Write(p, c) => {
            mem.0
                .insert(PATHS[*p].to_string(), CONTENTS[*c].as_bytes().to_vec());
            vec![touch(PATHS[*p])]
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
            mem.0.retain(|k, _| !k.starts_with("d/"));
            mem.0.extend(moved);
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
        let mut site = Site::new(Exclude::default());
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
            for push in delta.pushes {
                let key = match &push {
                    Push::Page { key, .. } | Push::Gone { key } => key.clone(),
                };
                last_push.insert(key, push);
            }
        }

        let (expected, _) = full(&mem);
        prop_assert_eq!(&out, &expected);

        for (key, push) in &last_push {
            let page = expected.get(&format!("pages/{key}.html"));
            match push {
                Push::Page { html, .. } => {
                    prop_assert_eq!(page.map(|b| b.as_slice()), Some(html.as_bytes()));
                }
                Push::Gone { .. } => prop_assert!(page.is_none(), "{key} is gone but published"),
            }
        }
        for path in expected.keys().filter(|p| p.starts_with("pages/")) {
            let key = &path["pages/".len()..path.len() - ".html".len()];
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
