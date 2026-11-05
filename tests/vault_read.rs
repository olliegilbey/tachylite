#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;

use tachylite::vault::{ReadError, SkipReason, VaultDir};

struct Dirs {
    _tmp: tempfile::TempDir,
    vault: std::path::PathBuf,
    outside: std::path::PathBuf,
}

fn dirs() -> Dirs {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    let outside = tmp.path().join("outside");
    fs::create_dir(&vault).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.md"), "outside bytes").unwrap();
    Dirs {
        _tmp: tmp,
        vault,
        outside,
    }
}

#[test]
fn reads_a_note() {
    let d = dirs();
    fs::write(d.vault.join("a.md"), "# A").unwrap();
    let vault = VaultDir::open(&d.vault).unwrap();
    assert_eq!(vault.read("a.md").unwrap(), b"# A");
}

#[test]
fn missing_file_is_missing() {
    let d = dirs();
    let vault = VaultDir::open(&d.vault).unwrap();
    assert!(matches!(vault.read("nope.md"), Err(ReadError::Missing)));
    assert!(matches!(vault.read("no/dir.md"), Err(ReadError::Missing)));
}

#[test]
fn symlinked_note_is_skipped_as_symlink() {
    let d = dirs();
    symlink(d.outside.join("secret.md"), d.vault.join("link.md")).unwrap();
    let vault = VaultDir::open(&d.vault).unwrap();
    assert!(matches!(
        vault.read("link.md"),
        Err(ReadError::Skip(SkipReason::Symlink))
    ));
}

#[test]
fn symlinked_parent_is_skipped_as_symlink() {
    let d = dirs();
    symlink(&d.outside, d.vault.join("d")).unwrap();
    let vault = VaultDir::open(&d.vault).unwrap();
    assert!(matches!(
        vault.read("d/secret.md"),
        Err(ReadError::Skip(SkipReason::Symlink))
    ));
}

#[test]
fn dotdot_cannot_escape() {
    let d = dirs();
    let vault = VaultDir::open(&d.vault).unwrap();
    assert!(vault.read("../outside/secret.md").is_err());
}

#[test]
fn fifo_is_missing_and_does_not_hang() {
    let d = dirs();
    let status = Command::new("mkfifo")
        .arg(d.vault.join("pipe.md"))
        .status()
        .unwrap();
    assert!(status.success());
    let vault = VaultDir::open(&d.vault).unwrap();
    assert!(matches!(vault.read("pipe.md"), Err(ReadError::Missing)));
}

#[test]
fn a_folder_is_missing() {
    let d = dirs();
    fs::create_dir(d.vault.join("x.md")).unwrap();
    let vault = VaultDir::open(&d.vault).unwrap();
    assert!(matches!(vault.read("x.md"), Err(ReadError::Missing)));
}
