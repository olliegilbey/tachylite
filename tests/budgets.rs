#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cmp::Reverse;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use tachylite::links::Index;
use tachylite::render::{Sources, render_body};

/// CI runs the timed budgets at `BUDGET_SCALE=3`; the reference host runs them at 1.
fn scale() -> f64 {
    std::env::var("BUDGET_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0)
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn cold_full_build_2000_notes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    tachylite::budget_vault::write(&vault, 1, 2000).unwrap();

    let start = Instant::now();
    let summary = tachylite::build::build(&vault, &dir.path().join("out")).unwrap();
    let elapsed = start.elapsed();

    let limit = Duration::from_secs_f64(1.0 * scale());
    println!(
        "cold full build, 2,000 notes: {} ms (limit {} ms)",
        elapsed.as_millis(),
        limit.as_millis()
    );
    assert_eq!(summary.notes, 2000);
    assert!(elapsed <= limit);
}

mod common;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use common::{Server, version_of};

/// `serve` on a fresh budget vault, and one note path in it.
fn budget_server(dir: &std::path::Path) -> (Server, String) {
    let vault = dir.join("vault");
    tachylite::budget_vault::write(&vault, 1, 2000).unwrap();
    let note = tachylite::budget_vault::generate(1, 2000)
        .into_iter()
        .map(|(path, _)| path)
        .find(|p| p.ends_with(".md"))
        .unwrap();
    (Server::start(&vault, &dir.join("out")), note)
}

fn url(note: &str) -> String {
    format!(
        "/{}",
        common::encode(&note[..note.len() - 3]).replace("%2F", "/")
    )
}

fn proc_field(pid: u32, file: &str, key: &str) -> u64 {
    let text = fs::read_to_string(format!("/proc/{pid}/{file}")).unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|v| v.split_whitespace().next())
        .unwrap()
        .parse()
        .unwrap()
}

/// `utime + stime`, in clock ticks.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}

/// Prints a timed budget's spread, then asserts its median within `limit` and its 90th
/// percentile within twice that: the tail absorbs a scheduler hiccup, not a regression.
fn assert_timed(what: &str, mut times: Vec<Duration>, limit: Duration) {
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    let (median, p90) = (at(0.5), at(0.9));
    println!(
        "{what}: min {} µs, q1 {} µs, median {} µs, p90 {} µs, max {} µs \
         (limit {} ms, p90 limit {} ms)",
        times[0].as_micros(),
        at(0.25).as_micros(),
        median.as_micros(),
        p90.as_micros(),
        times[times.len() - 1].as_micros(),
        limit.as_millis(),
        (2 * limit).as_millis()
    );
    assert!(median <= limit, "{what}: median over the limit");
    assert!(p90 <= 2 * limit, "{what}: p90 over twice the limit");
}

fn push_limit() -> Duration {
    Duration::from_secs_f64(0.010 * scale())
}

fn dependents_limit() -> Duration {
    Duration::from_secs_f64(0.250 * scale())
}

/// Waits until the manifest in `out` lists `url` at `version`. The manifest is written
/// last, so every page of its apply is written by then.
fn await_manifest(out: &Path, url: &str, version: &str) {
    let manifest = out.join("files/.tachylite/manifest.json");
    let needle = format!("\"{url}\":\"{version}\"");
    let deadline = Instant::now() + Duration::from_secs(10);
    // It is replaced by rename, so a new inode means a new manifest.
    let mut seen = None;
    loop {
        let ino = fs::metadata(&manifest).ok().map(|m| m.ino());
        if ino != seen {
            seen = ino;
            if fs::read_to_string(&manifest).is_ok_and(|m| m.contains(&needle)) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "the manifest never listed {url}");
        std::thread::yield_now();
    }
}

/// The page event's version.
fn version(event: &common::SseEvent) -> String {
    event.id.clone().expect("a page event has an id")
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn push_latency() {
    let dir = tempfile::tempdir().unwrap();
    let (server, note) = budget_server(dir.path());
    let path = dir.path().join("vault").join(&note);
    let base = fs::read_to_string(&path).unwrap();
    let page = url(&note);
    let mut events = server.subscribe(&page, &version_of(&server.get(&page).text()), None);

    let mut times = Vec::new();
    for i in 0..50 {
        fs::write(&path, format!("{base}\n\nedit {i}\n")).unwrap();
        let start = Instant::now();
        let event = events.expect("page");
        times.push(start.elapsed());
        assert!(event.data.contains(&format!("edit {i}")));
        // Each save starts once the last apply is done, as a person's next save would.
        await_manifest(&dir.path().join("out"), &page, &version(&event));
    }
    assert_timed("file event to push, budget vault", times, push_limit());
}

/// The budget vault's note with the most inbound links, by its links as the site resolves them.
fn most_linked() -> String {
    let notes = tachylite::budget_vault::generate(1, 2000);
    let index = Index::new(notes.iter().map(|(p, _)| p.as_str()));
    let mut inbound: HashMap<String, usize> = HashMap::new();
    for (path, text) in notes.iter().filter(|(p, _)| p.ends_with(".md")) {
        for target in render_body(path, text, &index, &Sources::new()).targets {
            if *target != **path && target.ends_with(".md") {
                *inbound.entry(target.to_string()).or_default() += 1;
            }
        }
    }
    inbound
        .into_iter()
        .max_by_key(|(path, n)| (*n, Reverse(path.clone())))
        .unwrap()
        .0
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn dependent_push_latency() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _) = budget_server(dir.path());
    let out = dir.path().join("out");
    let target = most_linked();
    let page = url(&target);
    let mut events = server.subscribe(&page, &version_of(&server.get(&page).text()), None);
    let linker = dir.path().join("vault/linker.md");
    let mut created = server.subscribe("/linker", "", None);
    fs::write(&linker, "no link yet").unwrap();
    await_manifest(&out, "/linker", &version(&created.expect("page")));

    let link = format!("[[{}]]", &target[..target.len() - ".md".len()]);
    let backlink = "<li><a href=\"/linker\">linker</a></li>";
    let mut times = Vec::new();
    for i in 0..20 {
        let adds = i % 2 == 0;
        let body = if adds {
            format!("{link} {i}")
        } else {
            format!("no link {i}")
        };
        fs::write(&linker, body).unwrap();
        let start = Instant::now();
        let event = events.expect("page");
        assert_eq!(event.data.contains(backlink), adds, "round {i}");
        await_manifest(&out, &page, &version(&event));
        times.push(start.elapsed());
    }
    assert_timed(
        "link edit to the target's push and manifest, budget vault",
        times,
        dependents_limit(),
    );
}

/// Every `step`th budget-vault note path, up to `n` of them.
fn spread(n: usize, step: usize) -> Vec<String> {
    tachylite::budget_vault::generate(1, 2000)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|p| p.ends_with(".md"))
        .step_by(step)
        .take(n)
        .collect()
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn embedded_push_latency() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let out = dir.path().join("out");
    tachylite::budget_vault::write(&vault, 1, 2000).unwrap();
    let embedded = vault.join("embedded.md");
    fs::write(&embedded, "# embedded").unwrap();
    // A hundred budget-vault notes, each at its own size, embed it.
    let embedders = spread(100, 19);
    for path in &embedders {
        let mut text = fs::read_to_string(vault.join(path)).unwrap();
        text.push_str("\n\n![[embedded]]\n");
        fs::write(vault.join(path), text).unwrap();
    }
    let server = Server::start(&vault, &out);
    let mut own = server.subscribe(
        "/embedded",
        &version_of(&server.get("/embedded").text()),
        None,
    );
    let last = url(embedders.iter().max().unwrap());
    let mut dependent = server.subscribe(&last, &version_of(&server.get(&last).text()), None);

    let (mut pushes, mut dependents) = (Vec::new(), Vec::new());
    for i in 0..30 {
        fs::write(&embedded, format!("# embedded\n\nedit {i}\n")).unwrap();
        let start = Instant::now();
        let event = own.expect("page");
        pushes.push(start.elapsed());
        assert!(event.data.contains(&format!("edit {i}")));
        let event = dependent.expect("page");
        assert!(event.data.contains(&format!("edit {i}")), "round {i}");
        await_manifest(&out, &last, &version(&event));
        dependents.push(start.elapsed());
    }
    assert_timed(
        "file event to push, note embedded by 100 notes",
        pushes,
        push_limit(),
    );
    assert_timed(
        "file event to the embedders' push and manifest, 100 embedders",
        dependents,
        dependents_limit(),
    );
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn fan_out_latency() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _) = budget_server(dir.path());
    let out = dir.path().join("out");
    let targets = spread(500, 4);
    let links: String = targets
        .iter()
        .map(|t| format!("[[{}]]\n", &t[..t.len() - ".md".len()]))
        .collect();
    let last = url(targets.iter().max().unwrap());
    let mut target = server.subscribe(&last, &version_of(&server.get(&last).text()), None);
    let fan = dir.path().join("vault/fan.md");

    // Each round creates the note, retitles it twice, then deletes it.
    let mut times = Vec::new();
    for i in 0..24 {
        let (body, backlink) = match i % 4 {
            3 => (None, None),
            r => {
                let title = format!("fan {i}");
                let body = match r {
                    0 => links.clone(),
                    _ => format!("---\ntitle: {title}\n---\n{links}"),
                };
                let shown = if r == 0 { "fan".to_string() } else { title };
                (Some(body), Some(format!("<a href=\"/fan\">{shown}</a>")))
            }
        };
        match &body {
            Some(body) => fs::write(&fan, body).unwrap(),
            None => fs::remove_file(&fan).unwrap(),
        }
        let start = Instant::now();
        let event = target.expect("page");
        match &backlink {
            Some(b) => assert!(event.data.contains(b), "round {i}"),
            None => assert!(!event.data.contains("href=\"/fan\""), "round {i}"),
        }
        await_manifest(&out, &last, &version(&event));
        times.push(start.elapsed());
    }
    assert_timed(
        "note with 500 links created, retitled or deleted, to the targets' push and manifest",
        times,
        dependents_limit(),
    );
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn burst_latency() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _) = budget_server(dir.path());
    let out = dir.path().join("out");
    let notes = spread(20, 97);
    let vault = dir.path().join("vault");
    let bases: Vec<String> = notes
        .iter()
        .map(|n| fs::read_to_string(vault.join(n)).unwrap())
        .collect();
    let last = url(&notes[notes.len() - 1]);
    let mut events = server.subscribe(&last, &version_of(&server.get(&last).text()), None);

    let mut times = Vec::new();
    for i in 0..10 {
        let start = Instant::now();
        for (note, base) in notes.iter().zip(&bases) {
            fs::write(vault.join(note), format!("{base}\n\nburst {i}\n")).unwrap();
        }
        let event = loop {
            let event = events.expect("page");
            if event.data.contains(&format!("burst {i}")) {
                break event;
            }
        };
        await_manifest(&out, &last, &version(&event));
        times.push(start.elapsed());
    }
    assert_timed(
        "burst of 20 saves to the last push and manifest",
        times,
        dependents_limit(),
    );
}

/// Context switches summed over every thread: each wakeup costs one.
fn wakeups(pid: u32) -> u64 {
    fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .map(|t| t.unwrap().file_name().into_string().unwrap())
        .map(|tid| {
            let file = format!("task/{tid}/status");
            proc_field(pid, &file, "voluntary_ctxt_switches:")
                + proc_field(pid, &file, "nonvoluntary_ctxt_switches:")
        })
        .sum()
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn idle_cpu() {
    let dir = tempfile::tempdir().unwrap();
    let (server, note) = budget_server(dir.path());
    let page = url(&note);
    let mut events = server.subscribe(&page, "0000000000000000", None);
    events.expect("page");

    let pid = server.child.id();
    let before = cpu_ticks(pid);
    let woke = wakeups(pid);
    // The idle window is the measurement: an open stream that must stay silent for 5 s.
    assert!(
        events.next(Duration::from_secs(5)).is_none(),
        "an idle server pushed"
    );
    let used = cpu_ticks(pid) - before;
    // A thread that exits takes its counts with it, so this can shrink.
    let woken = wakeups(pid).saturating_sub(woke);
    println!("idle CPU over 5 s: {used} ticks (limit 2), {woken} wakeups (limit 5)");
    assert!(used <= 2);
    assert!(woken <= 5, "an idle server woke {woken} times");
}

#[test]
#[ignore = "budget: run with make budget or make check"]
fn resident_memory() {
    let dir = tempfile::tempdir().unwrap();
    let (server, note) = budget_server(dir.path());
    let page = url(&note);
    let mut events = server.subscribe(&page, "0000000000000000", None);
    events.expect("page");
    let path = dir.path().join("vault").join(&note);
    fs::write(&path, "# edited").unwrap();
    events.expect("page");

    let peak_kb = proc_field(server.child.id(), "status", "VmHWM:");
    println!(
        "peak resident memory, budget vault: {} MB (limit 65 MB)",
        peak_kb / 1024
    );
    assert!(peak_kb <= 65 * 1024);
}

/// A deterministic budget, so it runs in every `cargo test`: the median budget-vault page
/// and the script every page loads, gzipped as the server sends them. Mermaid loads only on
/// a page with a diagram, so a typical page does not carry it.
#[test]
fn typical_page_weight() {
    use std::io::Write as _;
    use tachylite::site::{Change, Note, Push, Site, Source};
    use tachylite::vault::{Entry, ReadError, classify};

    struct Notes(HashMap<String, Vec<u8>>);
    impl Source for Notes {
        fn note(&mut self, path: &str) -> Result<Note, ReadError> {
            self.0
                .get(path)
                .cloned()
                .map(Note::from)
                .ok_or(ReadError::Missing)
        }
        fn file(&mut self, _: &str) -> Result<(), ReadError> {
            Err(ReadError::Missing)
        }
    }
    fn gzipped(bytes: &[u8]) -> usize {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap().len()
    }

    let notes = tachylite::budget_vault::generate(1, 500);
    let exclude = tachylite::config::Exclude::default();
    let entries = notes
        .iter()
        .map(|(path, _)| Entry {
            path: path.clone(),
            kind: classify(path, &exclude).unwrap(),
        })
        .collect();
    let mut source = Notes(
        notes
            .into_iter()
            .map(|(p, c)| (p, c.into_bytes()))
            .collect(),
    );
    let delta = Site::new(exclude).apply(
        vec![Change::Sync {
            under: String::new(),
            entries,
        }],
        &mut source,
    );
    let mut pages: Vec<usize> = delta
        .pushes
        .iter()
        .filter_map(|p| match p {
            Push::Page { html, .. } => Some(gzipped(html.as_bytes())),
            Push::Gone { .. } => None,
        })
        .collect();
    pages.sort_unstable();
    let page = pages[pages.len() / 2];
    let script = gzipped(include_bytes!("../src/live.js"));
    let style = gzipped(include_bytes!("../src/style.css"));
    let limit = 30 * 1024;
    let total = page + script + style;
    println!(
        "typical page weight: {total} B ({page} B page + {script} B script + {style} B style, \
         limit {limit} B)"
    );
    assert!(total <= limit);
}
