#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn no_arguments_prints_usage_and_exits_2() {
    let out = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: tachylite build <vault> <out>"));
    assert!(stderr.contains("tachylite serve <vault> <out> [--port <port>]"));
}

#[test]
fn bad_port_prints_usage_and_exits_2() {
    let out = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args(["serve", "v", "o", "--port", "http"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for item in std::fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let target = to.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_dir(&item.path(), &target);
        } else {
            std::fs::copy(item.path(), target).unwrap();
        }
    }
}

#[test]
fn build_command_builds_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let out = dir.path().join("out");
    copy_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault"),
        &vault,
    );
    let run = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args(["build".as_ref(), vault.as_os_str(), out.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("built 4 notes, 3 files, 0 skipped"),
        "{stderr}"
    );
    assert!(
        stderr.contains("broken link in notes/links.md: nobody\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("broken link in notes/links.md: missing.md\n"),
        "{stderr}"
    );
    assert!(out.join("pages/welcome.html").is_file());
}

#[test]
fn build_error_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args([
            "build".as_ref(),
            dir.path().join("missing").as_os_str(),
            dir.path().join("out").as_os_str(),
        ])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&run.stderr).starts_with("error: "));
}
