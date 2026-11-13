#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use common::fixture;
use tachylite::config::Exclude;
use tachylite::vault::{Entry, Kind, SkipReason, Skipped, scan};

fn entry(path: &str, kind: Kind) -> Entry {
    Entry {
        path: path.to_string(),
        kind,
    }
}

#[test]
fn fixture_vault_has_exclusion_cases() {
    for path in [
        ".obsidian/app.json",
        ".env",
        ".draft.md",
        "scripts/tool.py",
        "data/table.json",
        "target/stale.md",
        "node_modules/pkg/readme.md",
        "notes/.hidden/secret.md",
    ] {
        assert!(fixture().join(path).is_file(), "fixture lost {path}");
    }
}

#[test]
fn fixture_vault_entries() {
    let scan = scan(&fixture(), &Exclude::default()).unwrap();
    assert_eq!(
        scan.entries,
        vec![
            entry("Upper.MD", Kind::Note),
            entry("docs/guide.pdf", Kind::Asset),
            entry("img/dot.png", Kind::Asset),
            entry("maps/route.html", Kind::Html),
            entry("notes/links.md", Kind::Note),
            entry("notes/syntax.md", Kind::Note),
            entry("people/Jane Doe.md", Kind::Note),
            entry("welcome.md", Kind::Note),
        ]
    );
    assert_eq!(scan.skipped, vec![]);
}

#[test]
fn symlinks_are_skipped_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&vault).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("x.md"), "outside").unwrap();
    fs::write(vault.join("real.md"), "inside").unwrap();
    symlink(outside.join("x.md"), vault.join("link.md")).unwrap();
    symlink(&outside, vault.join("out")).unwrap();

    let scan = scan(&vault, &Exclude::default()).unwrap();
    assert_eq!(scan.entries, vec![entry("real.md", Kind::Note)]);
    assert_eq!(
        scan.skipped,
        vec![
            Skipped {
                path: PathBuf::from("link.md"),
                reason: SkipReason::Symlink
            },
            Skipped {
                path: PathBuf::from("out"),
                reason: SkipReason::Symlink
            },
        ]
    );
}

#[test]
fn case_variants_are_both_listed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.MD"), "upper").unwrap();
    fs::write(dir.path().join("a.md"), "lower").unwrap();

    let scan = scan(dir.path(), &Exclude::default()).unwrap();
    assert_eq!(
        scan.entries,
        vec![entry("a.MD", Kind::Note), entry("a.md", Kind::Note)]
    );
    assert!(scan.skipped.is_empty());
}

#[test]
fn non_utf8_names_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let name = OsStr::from_bytes(b"bad\xff.md");
    fs::write(dir.path().join(name), "x").unwrap();

    let scan = scan(dir.path(), &Exclude::default()).unwrap();
    assert_eq!(scan.entries, vec![]);
    assert_eq!(
        scan.skipped,
        vec![Skipped {
            path: PathBuf::from(name),
            reason: SkipReason::NonUtf8Name
        }]
    );
}

#[test]
fn unreadable_folder_is_skipped_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ok.md"), "fine").unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("x.md"), "hidden").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        fs::read_dir(&locked).is_err(),
        "run as non-root: root reads any folder"
    );

    let result = scan(dir.path(), &Exclude::default());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let scan = result.unwrap();
    assert_eq!(scan.entries, vec![entry("ok.md", Kind::Note)]);
    assert_eq!(
        scan.skipped,
        vec![Skipped {
            path: PathBuf::from("locked"),
            reason: SkipReason::Unreadable(std::io::ErrorKind::PermissionDenied)
        }]
    );
}

#[cfg(unix)]
#[test]
fn fifo_named_like_a_note_is_not_published() {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(dir.path().join("pipe.md"))
        .status()
        .unwrap();
    assert!(status.success());
    let scan = scan(dir.path(), &Exclude::default()).unwrap();
    assert_eq!(scan.entries, vec![]);
}
