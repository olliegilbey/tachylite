#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{Dirs, Server};
use proptest::prelude::*;

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
const CONTENTS: [&str; 13] = [
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
];

#[derive(Debug, Clone)]
enum Op {
    Write(usize, usize),
    Same(usize),
    Delete(usize),
    Rename(usize, usize),
    FolderWithNotes,
    RenameFolder,
    DeleteFolder,
    Burst,
    /// Writes `tachylite.toml`, excluding `d` or nothing.
    Config(bool),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => (0..PATHS.len(), 0..CONTENTS.len()).prop_map(|(p, c)| Op::Write(p, c)),
        1 => (0..PATHS.len()).prop_map(Op::Same),
        2 => (0..PATHS.len()).prop_map(Op::Delete),
        2 => (0..PATHS.len(), 0..PATHS.len()).prop_map(|(a, b)| Op::Rename(a, b)),
        1 => Just(Op::FolderWithNotes),
        1 => Just(Op::RenameFolder),
        1 => Just(Op::DeleteFolder),
        1 => Just(Op::Burst),
        1 => any::<bool>().prop_map(Op::Config),
    ]
}

/// Applies an op to a real vault. An op the filesystem refuses (a file where a folder
/// must go) is a no-op, as it would be for a user.
fn apply(vault: &Path, op: &Op) {
    let path = |i: usize| vault.join(PATHS[i]);
    let write = |p: PathBuf, body: &[u8]| {
        if let Some(parent) = p.parent()
            && fs::create_dir_all(parent).is_ok()
        {
            let _ = fs::write(&p, body);
        }
    };
    match op {
        Op::Write(p, c) => write(path(*p), CONTENTS[*c].as_bytes()),
        Op::Same(p) => {
            if let Ok(bytes) = fs::read(path(*p)) {
                write(path(*p), &bytes);
            }
        }
        Op::Delete(p) => {
            let _ = fs::remove_file(path(*p));
        }
        Op::Rename(a, b) => {
            if let Some(parent) = path(*b).parent()
                && fs::create_dir_all(parent).is_ok()
            {
                let _ = fs::rename(path(*a), path(*b));
            }
        }
        Op::FolderWithNotes => {
            write(vault.join("f/g/m.md"), b"# m");
            write(vault.join("f/g/n.md"), b"# n");
            write(vault.join("f/p.png"), b"png");
        }
        Op::RenameFolder => {
            if !vault.join("d2").exists() {
                let _ = fs::rename(vault.join("d"), vault.join("d2"));
            }
        }
        Op::DeleteFolder => {
            let _ = fs::remove_dir_all(vault.join("d"));
        }
        Op::Config(exclude) => {
            let list = if *exclude { "[\"d\"]" } else { "[]" };
            write(
                vault.join("tachylite.toml"),
                format!("exclude = {list}\n").as_bytes(),
            );
        }
        Op::Burst => {
            let _ = fs::create_dir_all(vault.join("burst"));
            for i in 0..200 {
                let _ = fs::write(vault.join(format!("burst/n{i:03}.md")), format!("# {i}"));
            }
        }
    }
}

/// Every file under `root` with its bytes. A file removed during the walk is left out.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(items) = fs::read_dir(&dir) else {
            continue;
        };
        for item in items.flatten() {
            let path = item.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                found.insert(rel, bytes);
            }
        }
    }
    found
}

fn cases() -> u32 {
    std::env::var("LIVE_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn live_output_equals_full_build(ops in prop::collection::vec((op(), any::<bool>()), 1..12)) {
        let d = Dirs::empty();
        fs::write(d.vault().join("zz-barrier.md"), "# 0").unwrap();
        // A short hold, so a case does not wait out a second for a note in a new folder.
        let server = Server::start_with(&d.vault(), &d.out(), &[("TACHYLITE_HOLD_MS", "20")]);
        let mut barrier = server.subscribe("/zz-barrier", "", None);
        barrier.expect("page");
        // inotify keeps order and batches apply in order, so once the barrier's push
        // arrives, every earlier change is applied.
        let mut settle = |mark: &str| {
            fs::write(d.vault().join("zz-barrier.md"), format!("# {mark}")).unwrap();
            let heading = format!("\">{mark}</h1>");
            loop {
                let event = barrier.expect("page");
                if event.data.contains(&heading) {
                    return;
                }
            }
        };

        // A settled op is applied before the next starts; the rest land in shared batches.
        for (i, (op, settled)) in ops.iter().enumerate() {
            apply(&d.vault(), op);
            if *settled {
                settle(&format!("s{i}"));
            }
        }
        settle("done");
        let fresh = d.tmp.path().join("fresh");
        tachylite::build::build(&d.vault(), &fresh).unwrap();
        // The last pushes may precede their writes, and a path held as mid-save is read
        // when its hold ends, so wait for the live output to match.
        let full = tree(&fresh);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut live = tree(&d.out());
        while live != full && std::time::Instant::now() < deadline {
            std::thread::yield_now();
            live = tree(&d.out());
        }
        prop_assert_eq!(
            live.keys().collect::<Vec<_>>(),
            full.keys().collect::<Vec<_>>()
        );
        prop_assert!(live == full, "same files, different bytes");
    }
}
