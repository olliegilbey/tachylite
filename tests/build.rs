#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tachylite::Error;
use tachylite::build::{Summary, build};
use tachylite::config::Exclude;
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
    assert_eq!((summary.notes, summary.files), (5, 3));
    for path in [
        "pages/welcome.html",
        "pages/people/Jane Doe.html",
        "pages/Upper.html",
        "pages/notes/links.html",
        "pages/notes/syntax.html",
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
            || tachylite::site::generated(rel_str)
            || (rel_str.starts_with("pages/") && rel_str.ends_with(".html"))
            || rel_str.strip_prefix("files/").is_some_and(|p| {
                matches!(
                    classify(p, &Exclude::default()),
                    Some(Kind::Html) | Some(Kind::Asset)
                )
            });
        assert!(ok, "unexpected output file {rel_str}");
    }
}

#[test]
fn html_pages_and_assets_are_byte_identical() {
    let s = Setup::new();
    build(&s.vault(), &s.out()).unwrap();
    let files = s.out().join("files");
    for rel in files_under(&files) {
        if rel.starts_with(".tachylite") {
            continue;
        }
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
    assert_eq!(summary.notes, 4);
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

fn clashes(summary: &Summary) -> Vec<&Path> {
    summary
        .skipped
        .iter()
        .filter(|k| k.reason == tachylite::vault::SkipReason::UrlClash)
        .map(|k| k.path.as_path())
        .collect()
}

#[test]
fn case_clash_keeps_first() {
    let s = Setup::new();
    fs::write(s.vault().join("a.MD"), "upper").unwrap();
    fs::write(s.vault().join("a.md"), "lower").unwrap();
    let summary = build(&s.vault(), &s.out()).unwrap();
    assert_eq!(clashes(&summary), [Path::new("a.md")]);
    assert!(
        fs::read_to_string(s.out().join("pages/a.html"))
            .unwrap()
            .contains("upper")
    );
}

#[test]
fn note_shadowing_html_page_is_a_clash() {
    let s = Setup::new();
    fs::write(s.vault().join("maps/route.html.md"), "# shadow").unwrap();
    let summary = build(&s.vault(), &s.out()).unwrap();
    assert_eq!(clashes(&summary), [Path::new("maps/route.html.md")]);
    assert_eq!(
        fs::read(s.out().join("files/maps/route.html")).unwrap(),
        fs::read(s.vault().join("maps/route.html")).unwrap()
    );
    assert!(!s.out().join("pages/maps/route.html.html").exists());
}

#[test]
fn note_and_folder_page_clash_is_skipped() {
    let s = Setup::new();
    fs::write(s.vault().join("x.md"), "# x").unwrap();
    fs::create_dir(s.vault().join("x.html")).unwrap();
    fs::write(s.vault().join("x.html/y.md"), "# y").unwrap();
    let summary = build(&s.vault(), &s.out()).unwrap();
    assert_eq!(clashes(&summary), [Path::new("x.md")]);
    assert!(s.out().join("pages/x.html/y.html").is_file());
}

#[test]
fn no_temp_files_remain() {
    let s = Setup::new();
    build(&s.vault(), &s.out()).unwrap();
    build(&s.vault(), &s.out()).unwrap();
    let hidden: Vec<_> = files_under(&s.out())
        .into_iter()
        .filter(|p| {
            p.components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
                && p != Path::new(".tachylite-output")
                && !tachylite::site::generated(p.to_str().unwrap())
        })
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");
    assert!(
        !files_under(&s.out())
            .iter()
            .any(|p| p.to_string_lossy().ends_with(".tmp"))
    );
}

/// A temp vault holding only `files`, and an output path beside it.
fn bare(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let path = dir.path().join("vault").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    dir
}

fn all_output_text(out: &Path) -> String {
    files_under(out)
        .iter()
        .map(|p| String::from_utf8_lossy(&fs::read(out.join(p)).unwrap()).into_owned())
        .collect()
}

#[test]
fn config_excludes_globs() {
    let dir = bare(&[
        (
            "tachylite.toml",
            "exclude = [\"private\", \"**/*.secret.md\"]\n",
        ),
        ("private/a.md", "# hidden-one"),
        ("notes/x.secret.md", "# hidden-two"),
        ("notes/y.md", "# shown"),
    ]);
    let (vault, out) = (dir.path().join("vault"), dir.path().join("out"));
    build(&vault, &out).unwrap();
    let pages: Vec<PathBuf> = files_under(&out.join("pages"))
        .into_iter()
        .filter(|p| !p.starts_with(".tachylite"))
        .collect();
    assert_eq!(pages, [PathBuf::from("notes/y.html")]);
    let text = all_output_text(&out);
    assert!(
        !text.contains("hidden-"),
        "an excluded note reached the output"
    );
}

#[test]
fn an_excluded_name_is_in_no_generated_file() {
    let dir = bare(&[
        (
            "tachylite.toml",
            "exclude = [\"secretdir\", \"**/secretnote.md\"]\n",
        ),
        ("secretdir/plain.md", "# plain"),
        ("notes/secretnote.md", "# plain"),
        ("notes/y.md", "# shown"),
    ]);
    let (vault, out) = (dir.path().join("vault"), dir.path().join("out"));
    build(&vault, &out).unwrap();
    let generated: Vec<_> = files_under(&out)
        .into_iter()
        .filter(|p| tachylite::site::generated(p.to_str().unwrap()))
        .collect();
    assert!(generated.len() >= 4, "{generated:?}");
    for rel in generated {
        let text = fs::read_to_string(out.join(&rel)).unwrap();
        assert!(!text.contains("secret"), "{rel:?} names an excluded note");
        assert!(
            text.contains("notes") || text.contains("{\"pages\""),
            "{rel:?}"
        );
    }
}

#[test]
fn bad_config_stops_the_build() {
    let dir = bare(&[("tachylite.toml", "exclude = [\n"), ("a.md", "# a")]);
    let err = build(&dir.path().join("vault"), &dir.path().join("out")).unwrap_err();
    assert!(matches!(err, Error::Config { .. }), "{err}");
    assert!(err.to_string().starts_with("tachylite.toml: "), "{err}");
}

#[test]
fn config_is_not_published() {
    let dir = bare(&[("tachylite.toml", "exclude = []\n"), ("a.md", "# a")]);
    let out = dir.path().join("out");
    build(&dir.path().join("vault"), &out).unwrap();
    assert!(!all_output_text(&out).contains("exclude"));
}

#[test]
fn comments_reach_no_output() {
    let vault = bare(&[
        ("target.md", "# Target"),
        (
            "a.md",
            "inline %%zebra [[target]]%% one\n\n%%\nzebra block [[target]]\n%%\n\n\
             wrap %%zebra\n[[target]]%% two\n\nacross %%zebra\n\n[[target]] zebra%% three\n\n\
             > [!note] %%zebra%%\n> four\n\nlast %%zebra [[target]]",
        ),
        // Footnote definitions move to the end of comrak's tree, so pairing must follow
        // the note's order.
        (
            "f1.md",
            "Text[^1]\n\n[^1]: note %% start\n\nzebra paragraph\n",
        ),
        (
            "f2.md",
            "Para[^1] one %% zebra\n\n[^1]: note %% shown\n\nPara two %% zebra two\n\n\
             zebra three %% shown three\n",
        ),
        // A block comment opened inside a container closes with the container in comrak.
        ("l1.md", "- %%\n  zebra one\nzebra two %%\n\nafter\n"),
        (
            "l2.md",
            "> %% zebra\nzebra lazy\n\nzebra para\n%%\n\nafter\n",
        ),
        // A comment that starts inside link text hides the link.
        (
            "k1.md",
            "[[target|visible %% zebra]]\nmore zebra %% after\n",
        ),
        (
            "k2.md",
            "[visible %% zebra](https://zebra.example/x) zebra %% after\n",
        ),
        ("k3.md", "![alt %% zebra](zebra.png) %% after\n"),
    ]);
    let out = vault.path().join("out");
    build(&vault.path().join("vault"), &out).unwrap();
    assert!(!all_output_text(&out).contains("zebra"));
    let target = fs::read_to_string(out.join("pages/target.html")).unwrap();
    assert!(!target.contains("backlinks"), "{target}");
    let a = fs::read_to_string(out.join("pages/a.html")).unwrap();
    for kept in ["one", "two", "three", "four"] {
        assert!(a.contains(kept), "{kept} not in {a}");
    }
}

/// Notes where the comment stripper and the parser can disagree about what is code.
#[test]
fn comments_the_stripper_misreads_reach_no_output() {
    let vault = bare(&[
        // An unclosed fence in a list ends with the list in the parser.
        (
            "u1.md",
            "- item\n\n  ```sh\n  echo hi\n\nAfter.\n\n<div>\n%%zebra one%%\n</div>\n",
        ),
        ("u2.md", "> ```\n> quoted\n\n<p>\n%%zebra two%%\n</p>\n"),
        // A fence indented 4 columns inside a fence is content, not a closer.
        (
            "u3.md",
            "```md\n1. Step\n\n    ```sh\n    echo hi\n    ```\n```\n\nPublic text.\n\n\
             <details>\n<summary>More</summary>\n%%zebra three%%\n</details>\n",
        ),
        // A fence line that only continues a paragraph.
        (
            "u4.md",
            "para\n    ```\n[x](https://example.com/ \"%%zebra four%%\")\n",
        ),
        (
            "u4b.md",
            "para\n    ```\n<span title=\"%%zebra five%%\">s</span>\n",
        ),
        (
            "u4c.md",
            "para\n    ```\n[y](<https://example.com/%%zebra%%>)\n",
        ),
        (
            "u5.md",
            "a <span title=\"`\"> <span title=\"%%zebra six%%\">x</span> <span title=\"`\"> b\n",
        ),
        ("u6.md", "para\n    ```\n[[%%zebra seven%%|label]]\n"),
        // Code the stripper misses must not shift which `%%` pairs with which.
        (
            "u7.md",
            "Intro.\n\n    printf(\"%%d\\n\", n);\n\nText %%zebra eight%% more\n",
        ),
        (
            "u8.md",
            "Use `printf(\"%%d\",\nn)` here %%zebra nine%% more\n",
        ),
        (
            "u9.md",
            "<div>\n```\n</div>\n\n%%zebra ten%%\n```\n\nText %%zebra%% more\n",
        ),
        // Code the stripper reads as text: its `%%` must not pair with a real comment's.
        ("s1.md", "- ```\n  code\n```\nx %%a\n%%zebra s1%%\n```\n"),
        ("s2.md", "> ```\n> code\n```\nx %%a\n%%zebra s2%%\n```\n"),
        ("s3.md", "para\n    ```\n```\nx %%a\n%%zebra s3%%\n```\n"),
        ("s5.md", "a `x\n    # h\n%%` b\n%%zebra s5%%\n"),
        ("s6.md", "a `x\n    ```\n    ```\n%%` b\n%%zebra s6%%\n"),
        ("s7.md", "para\n    - item\n\n        %%x\n%%zebra s7%%\n"),
        ("s8.md", "-\n\n  foo\n\n     %%x\n%%zebra s8%%\n"),
    ]);
    let out = vault.path().join("out");
    build(&vault.path().join("vault"), &out).unwrap();
    assert!(!all_output_text(&out).contains("zebra"));
    let u3 = fs::read_to_string(out.join("pages/u3.html")).unwrap();
    assert!(u3.contains("Public text."), "{u3}");
    // A link target is never published, but the build log names it.
    let u6 = fs::read_to_string(out.join("pages/u6.html")).unwrap();
    assert!(u6.contains("comment-error"), "{u6}");
    for (key, kept) in [("u7", "more"), ("u8", "here")] {
        let page = fs::read_to_string(out.join(format!("pages/{key}.html"))).unwrap();
        assert!(page.contains(kept), "{kept} not in {page}");
        assert!(!page.contains("comment-error"), "{page}");
    }
}

#[test]
fn an_embedded_excluded_note_reaches_no_output() {
    let dir = bare(&[
        ("tachylite.toml", "exclude = [\"private\"]\n"),
        ("private/secret.md", "# zebra-secret"),
        ("a.md", "![[secret]] ![[private/secret]] ![[b]]"),
        ("b.md", "bee ![[private/secret]]"),
    ]);
    let (vault, out) = (dir.path().join("vault"), dir.path().join("out"));
    build(&vault, &out).unwrap();
    let text = all_output_text(&out);
    assert!(text.contains("bee"), "the embed itself renders");
    assert!(
        !text.contains("zebra"),
        "an excluded note reached the output"
    );
}

#[test]
fn the_manifest_lists_no_excluded_note() {
    let dir = bare(&[
        ("tachylite.toml", "exclude = [\"private\"]\n"),
        ("private/zebra-secret.md", "# hidden"),
        ("a.md", "aye"),
    ]);
    let (vault, out) = (dir.path().join("vault"), dir.path().join("out"));
    build(&vault, &out).unwrap();
    let manifest = fs::read_to_string(out.join("files/.tachylite/manifest.json")).unwrap();
    assert!(manifest.contains("\"/a\""), "{manifest}");
    assert!(!manifest.contains("zebra"), "{manifest}");
}
