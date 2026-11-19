#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

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

#[test]
fn build_command_builds_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let out = dir.path().join("out");
    common::copy_dir(&common::fixture(), &vault);
    let run = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args(["build".as_ref(), vault.as_os_str(), out.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("built 7 notes, 3 files, 0 skipped"),
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

fn build(dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args([
            "build".as_ref(),
            dir.join("vault").as_os_str(),
            dir.join("out").as_os_str(),
        ])
        .output()
        .unwrap()
}

#[test]
fn a_config_named_in_another_case_is_named_and_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(vault.join("private")).unwrap();
    std::fs::write(vault.join("private/secret.md"), "# secret").unwrap();
    std::fs::write(vault.join("Tachylite.toml"), "exclude = [\"private\"]\n").unwrap();
    let run = build(dir.path());
    assert_eq!(run.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("Tachylite.toml is not read: the config must be named tachylite.toml\n"),
        "{stderr}"
    );
    assert!(dir.path().join("out/pages/private/secret.html").is_file());
}

#[test]
fn a_folder_named_like_the_config_stops_the_build() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(vault.join("tachylite.toml")).unwrap();
    std::fs::write(vault.join("a.md"), "# a").unwrap();
    let run = build(dir.path());
    assert_eq!(run.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&run.stderr),
        "error: tachylite.toml: not a file\n"
    );
    assert!(!dir.path().join("out").exists());
}

#[test]
fn a_pattern_inside_an_excluded_folder_is_not_named() {
    let (run, dir) = build_with_config("exclude = [\"private\", \"private/secret.md\"]\n");
    assert_eq!(run.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!stderr.contains("matched no published path"), "{stderr}");
    assert!(!dir.path().join("out/pages/private").exists());
}

const WAITING: &str =
    "tachylite.toml is empty and was just modified; waiting for its save to finish";

#[test]
fn a_build_waits_for_a_config_save_that_truncated_it() {
    use std::io::{BufRead, BufReader, Write};
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(vault.join("private")).unwrap();
    std::fs::write(vault.join("private/secret.md"), "# secret").unwrap();
    // Truncated before the build starts, and written only once the build is waiting.
    let mut config = std::fs::File::create(vault.join("tachylite.toml")).unwrap();
    let mut child = Kill(
        Command::new(env!("CARGO_BIN_EXE_tachylite"))
            .args([
                "build".as_ref(),
                vault.as_os_str(),
                dir.path().join("out").as_os_str(),
            ])
            .env("TACHYLITE_HOLD_MS", "600000")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stderr = BufReader::new(child.0.stderr.take().unwrap()).lines();
    let first = stderr.next().expect("no line").unwrap();
    assert_eq!(first, WAITING);
    config.write_all(b"exclude = [\"private\"]\n").unwrap();
    drop(config);
    assert!(child.0.wait().unwrap().success());
    assert!(dir.path().join("out/pages").is_dir());
    assert!(!dir.path().join("out/pages/private").exists());
}

#[test]
fn an_empty_config_saved_just_now_still_builds() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(vault.join("a.md"), "# a").unwrap();
    let config = std::fs::File::create(vault.join("tachylite.toml")).unwrap();
    // A time ahead of the clock counts as just now, however slowly the build starts.
    let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    config.set_modified(ahead).unwrap();
    drop(config);
    let run = Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .args([
            "build".as_ref(),
            vault.as_os_str(),
            dir.path().join("out").as_os_str(),
        ])
        .env("TACHYLITE_HOLD_MS", "200")
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.starts_with(WAITING), "{stderr}");
    assert!(dir.path().join("out/pages/a.html").is_file());
}

/// Kills its child on drop, so a failed assert leaves no process behind.
struct Kill(std::process::Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
