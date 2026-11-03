#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

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
