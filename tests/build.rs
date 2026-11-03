#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tachylite::Error;
use tachylite::build::{Summary, build};
use tachylite::vault::{Kind, classify};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let target = to.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_dir(&item.path(), &target);
        } else {
            fs::copy(item.path(), target).unwrap();
        }
    }
}

/// A temp dir holding a copy of the fixture vault at `vault/`, and a path `out/` beside it.
struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture(), &dir.path().join("vault"));
        Setup { dir }
    }
    fn vault(&self) -> PathBuf {
        self.dir.path().join("vault")
    }
    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for item in fs::read_dir(&dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn builds_fixture() {
    let s = Setup::new();
    let summary: Summary = build(&s.vault(), &s.out()).unwrap();
    assert_eq!((summary.notes, summary.files), (3, 3));
    for path in [
        "pages/welcome.html",
        "pages/people/Jane Doe.html",
        "pages/Upper.html",
        "files/maps/route.html",
        "files/img/dot.png",
        "files/docs/guide.pdf",
        ".tachylite-output",
    ] {
        assert!(s.out().join(path).is_file(), "missing {path}");
    }
}

#[test]
fn output_holds_only_published_files() {
    let s = Setup::new();
    build(&s.vault(), &s.out()).unwrap();
    for rel in files_under(&s.out()) {
        let rel_str = rel.to_str().unwrap();
        let ok = rel_str == ".tachylite-output"
            || (rel_str.starts_with("pages/") && rel_str.ends_with(".html"))
            || rel_str
                .strip_prefix("files/")
                .is_some_and(|p| matches!(classify(p), Some(Kind::Html) | Some(Kind::Asset)));
        assert!(ok, "unexpected output file {rel_str}");
    }
}

#[test]
fn html_pages_and_assets_are_byte_identical() {
    let s = Setup::new();
    build(&s.vault(), &s.out()).unwrap();
    let files = s.out().join("files");
    for rel in files_under(&files) {
        assert_eq!(
            fs::read(files.join(&rel)).unwrap(),
            fs::read(s.vault().join(&rel)).unwrap(),
            "{rel:?}"
        );
    }
}

fn dirs_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for item in fs::read_dir(&dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                found.push(path.strip_prefix(root).unwrap().to_path_buf());
                stack.push(path);
            }
        }
    }
    found.sort();
    found
}

fn state(root: &Path) -> Vec<(PathBuf, u64, SystemTime, Vec<u8>)> {
    files_under(root)
        .into_iter()
        .map(|rel| {
            let path = root.join(&rel);
            let meta = fs::metadata(&path).unwrap();
            (
                rel,
                meta.len(),
                meta.modified().unwrap(),
                fs::read(&path).unwrap(),
            )
        })
        .collect()
}

#[test]
fn vault_is_unchanged() {
    let s = Setup::new();
    let before = (state(&s.vault()), dirs_under(&s.vault()));
    build(&s.vault(), &s.out()).unwrap();
    assert_eq!((state(&s.vault()), dirs_under(&s.vault())), before);
}

#[test]
fn refuses_out_inside_vault() {
    let s = Setup::new();
    let out = s.vault().join("site");
    assert!(matches!(
        build(&s.vault(), &out),
        Err(Error::Overlap { .. })
    ));
    assert!(!out.exists());
}

#[test]
fn refuses_vault_inside_out() {
    let s = Setup::new();
    assert!(matches!(
        build(&s.vault(), s.dir.path()),
        Err(Error::Overlap { .. })
    ));
}

#[test]
fn refuses_out_inside_vault_through_a_symlink() {
    let s = Setup::new();
    fs::create_dir(s.vault().join("site")).unwrap();
    symlink(s.vault().join("site"), s.out()).unwrap();
    assert!(matches!(
        build(&s.vault(), &s.out()),
        Err(Error::Overlap { .. })
    ));
}

#[test]
fn refuses_dot_dot_after_missing_dir() {
    let s = Setup::new();
    let out = s.dir.path().join("nope/../vault/site");
    assert!(matches!(
        build(&s.vault(), &out),
        Err(Error::OutNotClean(_))
    ));
    assert!(!s.dir.path().join("nope").exists());
    assert!(!s.vault().join("site").exists());
}

#[test]
fn refuses_foreign_non_empty_out() {
    let s = Setup::new();
    fs::create_dir(s.out()).unwrap();
    fs::write(s.out().join("keep.txt"), "mine").unwrap();
    assert!(matches!(
        build(&s.vault(), &s.out()),
        Err(Error::OutNotOurs(_))
    ));
    assert_eq!(
        fs::read_to_string(s.out().join("keep.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn rebuild_drops_deleted_note() {
    let s = Setup::new();
    build(&s.vault(), &s.out()).unwrap();
    let page = s.out().join("pages/people/Jane Doe.html");
    assert!(page.is_file());
    fs::remove_file(s.vault().join("people/Jane Doe.md")).unwrap();
    build(&s.vault(), &s.out()).unwrap();
    assert!(!page.exists());
}

#[test]
fn invalid_utf8_note_still_renders() {
    let s = Setup::new();
    fs::write(s.vault().join("broken.md"), b"ok \xff ok").unwrap();
    build(&s.vault(), &s.out()).unwrap();
    assert!(
        fs::read_to_string(s.out().join("pages/broken.html"))
            .unwrap()
            .contains("ok")
    );
}

#[test]
fn unreadable_note_is_skipped_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let s = Setup::new();
    let note = s.vault().join("welcome.md");
    fs::set_permissions(&note, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&note).is_ok() {
        return; // Running as root: permissions cannot make a file unreadable.
    }
    let result = build(&s.vault(), &s.out());
    fs::set_permissions(&note, fs::Permissions::from_mode(0o644)).unwrap();
    let summary = result.unwrap();
    assert_eq!(summary.notes, 2);
    assert!(
        summary
            .skipped
            .iter()
            .any(|k| k.path == Path::new("welcome.md")
                && k.reason
                    == tachylite::vault::SkipReason::Unreadable(
                        std::io::ErrorKind::PermissionDenied
                    ))
    );
    assert!(!s.out().join("pages/welcome.html").exists());
}

#[test]
fn failed_scan_keeps_previous_output() {
    use std::os::unix::fs::PermissionsExt;
    let s = Setup::new();
    build(&s.vault(), &s.out()).unwrap();
    fs::set_permissions(s.vault(), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(s.vault()).is_ok() {
        fs::set_permissions(s.vault(), fs::Permissions::from_mode(0o755)).unwrap();
        return; // Running as root: permissions cannot make a folder unreadable.
    }
    let result = build(&s.vault(), &s.out());
    fs::set_permissions(s.vault(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert!(s.out().join("pages/welcome.html").is_file());
}

#[test]
fn upper_case_asset_extension_is_published() {
    let s = Setup::new();
    fs::copy(
        s.vault().join("img/dot.png"),
        s.vault().join("img/photo.PNG"),
    )
    .unwrap();
    let summary = build(&s.vault(), &s.out()).unwrap();
    assert_eq!(summary.files, 4);
    assert!(s.out().join("files/img/photo.PNG").is_file());
}
