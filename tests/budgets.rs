#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cmp::Reverse;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use tachylite::links::Index;
use tachylite::render::render_body;

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
    }
    times.sort();
    let median = times[times.len() / 2];
    let limit = Duration::from_secs_f64(0.010 * scale());
    println!(
        "file event to push, budget vault: median {} µs, max {} µs (limit {} ms)",
        median.as_micros(),
        times[times.len() - 1].as_micros(),
        limit.as_millis()
    );
    assert!(median <= limit);
}

/// The budget vault's note with the most inbound links, by its links as the site resolves them.
fn most_linked() -> String {
    let notes = tachylite::budget_vault::generate(1, 2000);
    let index = Index::new(notes.iter().map(|(p, _)| p.as_str()));
    let mut inbound: HashMap<String, usize> = HashMap::new();
    for (path, text) in notes.iter().filter(|(p, _)| p.ends_with(".md")) {
        for target in render_body(path, text, &index).targets {
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
    let target = most_linked();
    let page = url(&target);
    let mut events = server.subscribe(&page, &version_of(&server.get(&page).text()), None);
    let linker = dir.path().join("vault/linker.md");
    let mut created = server.subscribe("/linker", "", None);
    fs::write(&linker, "no link yet").unwrap();
    created.expect("page");

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
        times.push(start.elapsed());
        assert_eq!(event.data.contains(backlink), adds, "round {i}");
    }
    times.sort();
    let median = times[times.len() / 2];
    let limit = Duration::from_secs_f64(0.250 * scale());
    println!(
        "link edit to the target's push, budget vault: median {} µs, max {} µs (limit {} ms)",
        median.as_micros(),
        times[times.len() - 1].as_micros(),
        limit.as_millis()
    );
    assert!(median <= limit);
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
    // The idle window is the measurement: an open stream that must stay silent for 5 s.
    assert!(
        events.next(Duration::from_secs(5)).is_none(),
        "an idle server pushed"
    );
    let used = cpu_ticks(pid) - before;
    println!("idle CPU over 5 s: {used} ticks (limit 2)");
    assert!(used <= 2);
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
