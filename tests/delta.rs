//! The live seam for `manifest.json?since=<tag>`: a client that holds a manifest gets only
//! what changed since.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Dirs, Server, encode, listed_version, version_of};
use proptest::prelude::*;

const MANIFEST: &str = "/.tachylite/manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Manifest {
    pages: BTreeMap<String, String>,
    files: BTreeSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Delta {
    since: String,
    pages: BTreeMap<String, String>,
    gone: BTreeSet<String>,
    files: BTreeSet<String>,
    unfiled: BTreeSet<String>,
}

#[derive(Debug)]
enum Answer {
    Delta(Delta),
    Whole(Manifest),
}

/// The values between quotes in a JSON object or array of plain strings.
fn strings(json: &str) -> Vec<String> {
    json.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn pages(json: &str) -> BTreeMap<String, String> {
    let values = strings(json);
    values
        .chunks(2)
        .map(|kv| (kv[0].clone(), kv[1].clone()))
        .collect()
}

/// The text between `"<name>":` and the bracket that closes it.
fn field<'a>(json: &'a str, name: &str, close: char) -> &'a str {
    let key = format!("\"{name}\":");
    let start = json
        .find(&key)
        .unwrap_or_else(|| panic!("no {name} in {json}"))
        + key.len();
    let end = json[start..].find(close).unwrap() + start;
    &json[start..=end]
}

fn parse(json: &str) -> Manifest {
    Manifest {
        pages: pages(field(json, "pages", '}')),
        files: strings(field(json, "files", ']')).into_iter().collect(),
    }
}

fn parse_delta(json: &str) -> Delta {
    let rest = json.strip_prefix("{\"since\":\"").unwrap();
    let end = rest.find("\",\"pages\":").unwrap();
    let list = |name| strings(field(rest, name, ']')).into_iter().collect();
    Delta {
        since: rest[..end].replace("\\\"", "\""),
        pages: pages(field(rest, "pages", '}')),
        gone: list("gone"),
        files: list("files"),
        unfiled: list("unfiled"),
    }
}

fn apply(held: &Manifest, delta: &Delta) -> Manifest {
    let mut m = held.clone();
    for url in &delta.gone {
        m.pages.remove(url);
    }
    m.pages.extend(delta.pages.clone());
    for url in &delta.unfiled {
        m.files.remove(url);
    }
    m.files.extend(delta.files.clone());
    m
}

/// The current manifest and its tag.
fn whole(server: &Server) -> (String, Manifest, String) {
    let r = server.get(MANIFEST);
    assert_eq!(r.status, 200);
    let text = r.text();
    (r.header("etag").unwrap().to_string(), parse(&text), text)
}

/// The answer to `since`, with the response's tag.
fn ask(server: &Server, since: &str) -> (String, Answer, common::Response) {
    let r = server.get(&format!("{MANIFEST}?since={}", encode(since)));
    assert_eq!(r.status, 200);
    let text = r.text();
    let answer = if text.starts_with("{\"since\":") {
        Answer::Delta(parse_delta(&text))
    } else {
        Answer::Whole(parse(&text))
    };
    (r.header("etag").unwrap().to_string(), answer, r)
}

fn delta(answer: Answer) -> Delta {
    match answer {
        Answer::Delta(d) => d,
        Answer::Whole(_) => panic!("the whole manifest, where a delta was due"),
    }
}

/// Saves `note` (a vault path) with `body`, and waits until the manifest lists the page's
/// new version. Returns the new tag and manifest.
fn save(server: &Server, vault: &Path, note: &str, body: &str) -> (String, Manifest) {
    let url = format!("/{}", &note[..note.len() - 3]);
    let version = version_of(&server.get(&url).text());
    let mut events = server.subscribe(&url, &version, None);
    fs::write(vault.join(note), body).unwrap();
    // Another change may push the page first, so wait for this save's heading.
    let heading = format!(
        "\">{}</h1>",
        body.lines().next().unwrap().trim_start_matches("# ")
    );
    let now = loop {
        let event = events.expect("page");
        if event.data.contains(&heading) {
            break version_of(&event.data);
        }
    };
    await_listed(server, &url, &now)
}

fn await_listed(server: &Server, url: &str, version: &str) -> (String, Manifest) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (tag, m, text) = whole(server);
        if listed_version(&text, url).as_deref() == Some(version) {
            return (tag, m);
        }
        assert!(Instant::now() < deadline, "the manifest never listed {url}");
        std::thread::yield_now();
    }
}

/// The fixture vault with 100 more notes, so a small step falls under a quarter of the
/// manifest.
fn padded() -> Dirs {
    let d = Dirs::fixture();
    pad(&d.vault());
    d
}

fn pad(vault: &Path) {
    fs::create_dir_all(vault.join("pad")).unwrap();
    for i in 0..100 {
        fs::write(vault.join(format!("pad/n{i:03}.md")), "# pad").unwrap();
    }
}

#[test]
fn one_save_gets_only_what_changed() {
    let d = padded();
    let server = Server::start(&d.vault(), &d.out());
    let (a, held, text) = whole(&server);
    let (b, now) = save(&server, &d.vault(), "welcome.md", "# Welcome back");
    assert_ne!(a, b);
    let (tag, answer, r) = ask(&server, &a);
    assert_eq!(tag, b, "a delta carries the current tag");
    let delta = delta(answer);
    assert_eq!(delta.since, a);
    assert!(delta.pages.contains_key("/welcome"), "{delta:?}");
    assert_eq!(apply(&held, &delta), now);
    assert!(r.body.len() * 4 < text.len(), "{} bytes", r.body.len());
}

#[test]
fn the_current_tag_gets_an_empty_delta() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let (a, _, _) = whole(&server);
    let (tag, answer, _) = ask(&server, &a);
    assert_eq!(tag, a);
    let delta = delta(answer);
    assert_eq!(delta.since, a);
    assert!(delta.pages.is_empty() && delta.gone.is_empty());
    assert!(delta.files.is_empty() && delta.unfiled.is_empty());
}

#[test]
fn an_unknown_tag_gets_the_whole_manifest() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let (a, held, text) = whole(&server);
    let (tag, answer, r) = ask(&server, "W/\"0-0000000000000000\"");
    assert_eq!(tag, a);
    assert!(matches!(answer, Answer::Whole(m) if m == held));
    assert_eq!(r.text(), text);
    assert_eq!(r.header("cache-control"), Some("no-cache"));
}

#[test]
fn the_plain_manifest_still_revalidates() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let (a, _, _) = whole(&server);
    let again = common::get(server.addr, MANIFEST, &[("If-None-Match", &a)]);
    assert_eq!(again.status, 304);
    assert!(again.body.is_empty());
}

/// An answer to `since` depends on that manifest and the current one alone, so a cached
/// answer is current exactly when its tag is.
#[test]
fn a_cached_delta_revalidates_against_the_current_tag() {
    let d = padded();
    let server = Server::start(&d.vault(), &d.out());
    let (a, _, _) = whole(&server);
    let (b, _) = save(&server, &d.vault(), "welcome.md", "# Welcome back");
    let url = format!("{MANIFEST}?since={}", encode(&a));
    let first = server.get(&url);
    assert_eq!(first.header("cache-control"), Some("no-cache"));
    assert_eq!(first.header("etag"), Some(b.as_str()));
    assert_eq!(
        common::get(server.addr, &url, &[("If-None-Match", &b)]).status,
        304
    );
    let (c, _) = save(&server, &d.vault(), "welcome.md", "# Welcome again");
    let after = common::get(server.addr, &url, &[("If-None-Match", &b)]);
    assert_eq!(after.status, 200, "a delta to an older manifest was kept");
    assert_eq!(after.header("etag"), Some(c.as_str()));
}

#[test]
fn a_restart_answers_a_tag_from_before_it_with_the_whole_manifest() {
    let d = padded();
    let server = Server::start(&d.vault(), &d.out());
    let (a, _, _) = whole(&server);
    let (b, _) = save(&server, &d.vault(), "welcome.md", "# Welcome back");
    drop(server);
    let server = Server::start(&d.vault(), &d.out());
    let (c, now, _) = whole(&server);
    assert!(matches!(ask(&server, &a).1, Answer::Whole(m) if m == now));
    // The same content gets the same tag, so a client synced before the restart is current.
    if c == b {
        assert!(matches!(ask(&server, &b).1, Answer::Delta(d) if d.pages.is_empty()));
    }
}

#[test]
fn several_saves_merge_into_one_delta() {
    let d = padded();
    let server = Server::start(&d.vault(), &d.out());
    let (a, held, _) = whole(&server);
    fs::write(d.vault().join("brief.md"), "# Brief").unwrap();
    let (b, mid) = await_listing(&server, "/brief", true);
    save(&server, &d.vault(), "welcome.md", "# Welcome back");
    fs::remove_file(d.vault().join("brief.md")).unwrap();
    let (c, now) = await_listing(&server, "/brief", false);
    let (tag, answer, _) = ask(&server, &a);
    assert_eq!(tag, c);
    let merged = delta(answer);
    // Added and removed inside the range: gone, and a client that never held it drops nothing.
    assert!(merged.gone.contains("/brief"), "{merged:?}");
    assert!(!merged.pages.contains_key("/brief"));
    assert_eq!(apply(&held, &merged), now);
    assert_eq!(apply(&mid, &delta(ask(&server, &b).1)), now);
}

/// Waits until the manifest lists the page at `url`, at any version, or stops listing it.
fn await_listing(server: &Server, url: &str, listed: bool) -> (String, Manifest) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (tag, m, _) = whole(server);
        if m.pages.contains_key(url) == listed {
            return (tag, m);
        }
        assert!(Instant::now() < deadline, "the manifest never listed {url}");
        std::thread::yield_now();
    }
}

#[test]
fn a_note_excluded_later_appears_only_as_gone() {
    let d = padded();
    fs::create_dir(d.vault().join("later")).unwrap();
    fs::write(d.vault().join("later/n.md"), "# Later").unwrap();
    let server = Server::start(&d.vault(), &d.out());
    let (a, held, _) = whole(&server);
    assert!(held.pages.contains_key("/later/n"));
    // A note never published, saved beside one that is.
    fs::write(d.vault().join(".hidden.md"), "# Hidden").unwrap();
    let (b, _) = save(&server, &d.vault(), "welcome.md", "# Welcome back");
    let first = delta(ask(&server, &a).1);
    let all = |d: &Delta| -> Vec<String> {
        let urls = d
            .pages
            .keys()
            .chain(&d.gone)
            .chain(&d.files)
            .chain(&d.unfiled);
        urls.cloned().collect()
    };
    assert!(
        all(&first).iter().all(|u| !u.contains("hidden")),
        "{first:?}"
    );
    fs::write(d.vault().join("tachylite.toml"), "exclude = [\"later\"]\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let now = loop {
        let (_, m, _) = whole(&server);
        if !m.pages.contains_key("/later/n") {
            break m;
        }
        assert!(
            Instant::now() < deadline,
            "the exclusion never reached the manifest"
        );
        std::thread::yield_now();
    };
    let excluded = delta(ask(&server, &b).1);
    assert!(excluded.gone.contains("/later/n"), "{excluded:?}");
    assert!(!excluded.pages.keys().any(|u| u.starts_with("/later/")));
    assert!(all(&excluded).iter().all(|u| !u.contains("hidden")));
    assert_eq!(apply(&apply(&held, &first), &excluded), now);
}

#[test]
fn the_log_drops_its_oldest_steps_past_its_size() {
    let d = Dirs::empty();
    // Each save retitles a note with 30 long tags, so each step re-versions 30 tag pages with
    // long URLs. Long padding names keep one step under a quarter of the manifest.
    let tags: String = (0..30)
        .map(|i| format!(" #t{i:02}{}", "x".repeat(150)))
        .collect();
    for i in 0..300 {
        let name = format!("p{i:03}{}.md", "y".repeat(150));
        fs::write(d.vault().join(name), "# p").unwrap();
    }
    fs::write(d.vault().join("t.md"), format!("# T0\n{tags}")).unwrap();
    let server = Server::start(&d.vault(), &d.out());
    let (first, _, text) = whole(&server);
    let mut prev = first.clone();
    let mut steps = 0;
    let mut sent = 0;
    loop {
        steps += 1;
        let (tag, _) = save(&server, &d.vault(), "t.md", &format!("# T{steps}\n{tags}"));
        let step = ask(&server, &prev).2.body.len();
        assert!(
            step * 4 < text.len(),
            "a step of {step} bytes is too big for this test"
        );
        sent += step;
        prev = tag;
        match ask(&server, &first).1 {
            Answer::Delta(_) => assert!(sent <= 2 * tachylite::delta::LIMIT, "never dropped"),
            Answer::Whole(_) => break,
        }
    }
    assert!(
        sent > tachylite::delta::LIMIT / 2,
        "dropped after {sent} bytes"
    );
    // The newest steps are still kept.
    assert!(matches!(ask(&server, &prev).1, Answer::Delta(_)));
}

const PATHS: [&str; 8] = [
    "a.md", "b.md", "d/n.md", "d/e/m.md", "d/p.png", "x.html", ".h.md", "d/h.md",
];
const CONTENTS: [&str; 6] = [
    "# one",
    "two [[a]]",
    "- [ ] three #t",
    "![[b]] #t/u",
    "---\ntitle: T\naliases: [q]\n---\n[[q]]",
    "<p>h</p>",
];

#[derive(Debug, Clone)]
enum Op {
    Write(usize, usize),
    Delete(usize),
    Rename(usize, usize),
    /// Excludes `d`, or excludes nothing.
    Exclude(bool),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => (0..PATHS.len(), 0..CONTENTS.len()).prop_map(|(p, c)| Op::Write(p, c)),
        2 => (0..PATHS.len()).prop_map(Op::Delete),
        2 => (0..PATHS.len(), 0..PATHS.len()).prop_map(|(a, b)| Op::Rename(a, b)),
        1 => any::<bool>().prop_map(Op::Exclude),
    ]
}

fn run(vault: &Path, op: &Op) {
    let path = |i: usize| vault.join(PATHS[i]);
    let parent = |p: &Path| fs::create_dir_all(p.parent().unwrap()).is_ok();
    match op {
        Op::Write(p, c) => {
            if parent(&path(*p)) {
                let _ = fs::write(path(*p), CONTENTS[*c]);
            }
        }
        Op::Delete(p) => {
            let _ = fs::remove_file(path(*p));
        }
        Op::Rename(a, b) => {
            if parent(&path(*b)) {
                let _ = fs::rename(path(*a), path(*b));
            }
        }
        Op::Exclude(on) => {
            let text = if *on {
                "exclude = [\"d\"]\n"
            } else {
                "exclude = []\n"
            };
            let _ = fs::write(vault.join("tachylite.toml"), text);
        }
    }
}

fn cases() -> u32 {
    std::env::var("LIVE_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// After each step, every earlier tag gets a delta that turns its manifest into the
    /// current one, or the whole current manifest.
    #[test]
    fn a_delta_from_any_earlier_tag_gives_the_current_manifest(
        ops in prop::collection::vec(op(), 1..8)
    ) {
        let d = Dirs::empty();
        pad(&d.vault());
        fs::write(d.vault().join("zz.md"), "# start").unwrap();
        let server = Server::start_with(&d.vault(), &d.out(), &[("TACHYLITE_HOLD_MS", "20")]);
        let mut seen: Vec<(String, Manifest)> = vec![{
            let (tag, m, _) = whole(&server);
            (tag, m)
        }];
        // The last round saves the barrier alone, so its step is small.
        for i in 0..=ops.len() {
            if let Some(op) = ops.get(i) {
                run(&d.vault(), op);
            }
            let (tag, now) = save(&server, &d.vault(), "zz.md", &format!("# {i}"));
            for (old, held) in &seen {
                // A held path is read when its hold ends, so the manifest can move on. A
                // tag hashes the content, so equal tags mean equal manifests.
                let (answer, current) = loop {
                    let (at, answer, _) = ask(&server, old);
                    let (tag, current, _) = whole(&server);
                    if at == tag {
                        break (answer, current);
                    }
                };
                match answer {
                    Answer::Delta(delta) => {
                        prop_assert_eq!(&delta.since, old);
                        prop_assert_eq!(apply(held, &delta), current);
                    }
                    Answer::Whole(m) => prop_assert_eq!(m, current),
                }
            }
            if i == ops.len() {
                let previous = &seen.last().unwrap().0;
                let small = matches!(ask(&server, previous).1, Answer::Delta(_));
                prop_assert!(small, "one small save got the whole manifest");
            }
            seen.push((tag, now));
        }
    }
}
