#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn no_arguments_prints_usage_and_exits_2() {
    let out = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: tachylite build <vault> <out>"));
}
