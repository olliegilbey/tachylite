#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use proptest::prelude::*;
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
                classify(p).map(|kind| Entry {
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
    let delta = Site::new().apply(vec![sync_all(mem)], &mut source);
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
    let mut site = Site::new();
    site.apply(vec![sync_all(&mem)], &mut mem);
    let delta = site.apply(vec![touch("a.md")], &mut mem);
    assert!(delta.writes.is_empty() && delta.pushes.is_empty() && delta.deletes.is_empty());
}

#[test]
fn edited_note_is_written_and_pushed() {
    let mut mem = Mem::with(&[("a.md", "# A")]);
    let mut site = Site::new();
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
    let mut site = Site::new();
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
    let mut site = Site::new();
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
    let mut site = Site::new();
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
    let mut site = Site::new();
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
const CONTENTS: [&str; 3] = ["# one", "two *b*", "- [ ] three"];

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
    let mut site = Site::new();
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
    let mut site = Site::new();
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
    let mut site = Site::new();
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
        let mut site = Site::new();
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
