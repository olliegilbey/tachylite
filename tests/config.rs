#![allow(clippy::unwrap_used, clippy::expect_used)]

use tachylite::config::Exclude;

fn with(globs: &[&str]) -> Exclude {
    Exclude::new(&globs.iter().map(|g| g.to_string()).collect::<Vec<_>>()).unwrap()
}

#[test]
fn a_folder_pattern_hides_everything_under_it() {
    let e = with(&["private"]);
    assert!(e.path("private/a.md"));
    assert!(e.path("private/x/b.png"));
    assert!(!e.path("privateer.md"));
}

#[test]
fn star_stays_in_one_segment() {
    let e = with(&["*.md"]);
    assert!(e.path("a.md"));
    assert!(!e.path("x/a.md"));
    assert!(with(&["**/*.md"]).path("x/a.md"));
}

#[test]
fn built_in_exclusions_need_no_config() {
    let e = Exclude::default();
    for path in [
        ".git/x.md",
        "a/.draft.md",
        "target/a.md",
        "x/node_modules/r.md",
    ] {
        assert!(e.path(path), "{path}");
    }
    assert!(!e.path("notes/a.md"));
}

#[test]
fn a_bad_glob_is_an_error() {
    assert!(Exclude::new(&["a/***".to_string()]).is_err());
}
