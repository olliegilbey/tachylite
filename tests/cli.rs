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
        stderr.contains("built 5 notes, 3 files, 0 skipped"),
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

fn build_with_config(config: &str) -> (std::process::Output, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(vault.join("private")).unwrap();
    std::fs::write(vault.join("private/secret.md"), "# secret").unwrap();
    std::fs::write(vault.join("journal.MD"), "# journal").unwrap();
    std::fs::write(vault.join("a.md"), "# a").unwrap();
    std::fs::write(vault.join("tachylite.toml"), config).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args([
            "build".as_ref(),
            vault.as_os_str(),
            dir.path().join("out").as_os_str(),
        ])
        .output()
        .unwrap();
    (run, dir)
}

#[test]
fn a_misplaced_or_mistyped_exclude_stops_the_build_with_one_line() {
    for config in [
        "excludes = [\"private\"]\n",
        "[daily]\nfolder = \"journal\"\n\nexclude = [\"private\"]\n",
        "exclude = [\"../vault/private\"]\n",
        "exclude = [\" private\"]\n",
    ] {
        let (run, dir) = build_with_config(config);
        assert_eq!(run.status.code(), Some(1), "{config}");
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert!(stderr.starts_with("error: tachylite.toml: "), "{stderr}");
        assert!(!dir.path().join("out").exists(), "{config}");
    }
}

#[test]
fn an_exclude_pattern_that_matches_nothing_is_named() {
    let (run, dir) = build_with_config("exclude = [\"private\", \"privte\", \"*.md\"]\n");
    assert_eq!(run.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("exclude \"privte\" matched no published path"),
        "{stderr}"
    );
    assert!(!stderr.contains("exclude \"private\" matched"), "{stderr}");
    // `*.md` ignores case, as the publish list does.
    assert!(!dir.path().join("out/pages/journal.html").exists());
}

#[test]
fn an_unreadable_config_names_the_reason_in_words() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(dir.path().join("real.toml"), "exclude = []\n").unwrap();
    std::os::unix::fs::symlink("../real.toml", vault.join("tachylite.toml")).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args([
            "build".as_ref(),
            vault.as_os_str(),
            dir.path().join("out").as_os_str(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stderr),
        "error: tachylite.toml: symlink\n"
    );
}
