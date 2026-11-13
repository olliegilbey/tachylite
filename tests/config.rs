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

#[test]
fn gitignore_style_patterns_match_the_folder() {
    for glob in ["private", "private/", "/private", "./private", "/private/"] {
        let exclude = Exclude::new(&[glob.to_string()]).unwrap();
        assert!(exclude.path("private/a.md"), "{glob}");
        assert!(exclude.path("private"), "{glob}");
        assert!(!exclude.path("public/a.md"), "{glob}");
    }
}

#[test]
fn a_pattern_that_names_nothing_is_an_error() {
    for glob in ["/", "./", ""] {
        assert!(Exclude::new(&[glob.to_string()]).is_err(), "{glob:?}");
    }
}

use tachylite::config::{ConfigError, parse};

#[test]
fn exclusion_ignores_case_like_the_publish_list() {
    let e = with(&["journal/*.md", "Private"]);
    assert!(e.path("journal/Day.MD"));
    assert!(e.path("JOURNAL/day.md"));
    assert!(e.path("private/a.md"));
}

#[test]
fn an_unknown_key_is_refused() {
    for (text, key) in [
        ("excludes = [\"private\"]\n", "excludes"),
        ("Exclude = [\"private\"]\n", "Exclude"),
        ("[private]\n", "private"),
    ] {
        assert_eq!(
            parse(text).unwrap_err(),
            ConfigError::UnknownKey(key.into()),
            "{text}"
        );
    }
}

#[test]
fn the_designed_keys_are_accepted() {
    let text = "exclude = [\"private\"]\ntitle = \"Notes\"\npins = [\"[[a]]\"]\n\
                hosts = [\"box.example.ts.net\"]\n[daily]\nfolder = \"journal\"\n";
    let config = parse(text).unwrap();
    assert!(config.exclude.path("private/a.md"));
    assert_eq!(config.hosts, Some(vec!["box.example.ts.net".to_string()]));
    assert_eq!(parse("").unwrap().hosts, None);
}

#[test]
fn exclude_inside_a_table_is_refused() {
    for text in [
        "[daily]\nfolder = \"journal\"\n\nexclude = [\"private\"]\n",
        "daily = { exclude = [\"private\"] }\n",
        "[daily.deep]\nexclude = [\"private\"]\n",
    ] {
        assert!(
            matches!(parse(text), Err(ConfigError::Nested { .. })),
            "{text}"
        );
    }
}

#[test]
fn exclude_and_hosts_must_be_lists_of_strings() {
    for text in ["exclude = \"private\"\n", "hosts = [1]\n"] {
        assert!(
            matches!(parse(text), Err(ConfigError::NotStrings(_))),
            "{text}"
        );
    }
}

#[test]
fn a_pattern_with_a_parent_segment_is_an_error() {
    for glob in ["../vault/private", "a/../b"] {
        assert!(
            matches!(
                Exclude::new(&[glob.to_string()]),
                Err(ConfigError::ParentGlob(_))
            ),
            "{glob}"
        );
    }
}

#[test]
fn a_pattern_that_matched_nothing_is_named() {
    let e = with(&["private", " private"]);
    assert!(e.path("private/a.md"));
    assert!(!e.path("public/a.md"));
    assert_eq!(e.unmatched(), vec![" private"]);
}
