#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tachylite::render::{note_title, render_note};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vault")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn welcome() -> String {
    render_note("welcome", &fixture("welcome.md")).html
}

#[test]
fn welcome_snapshot() {
    insta::assert_snapshot!(welcome());
}

#[test]
fn dollar_is_literal() {
    assert!(welcome().contains("costs $5 or $10"));
}

#[test]
fn frontmatter_is_not_in_body() {
    let html = welcome();
    assert!(!html.contains("title:"));
    assert!(!html.contains("tags:"));
}

#[test]
fn raw_html_is_dropped() {
    let html = welcome();
    assert!(!html.contains("alert(1)"));
    assert_eq!(html.matches("<script").count(), 1, "only the live script");
}

#[test]
fn title_is_escaped() {
    let html = render_note("a<b & \"c\"", "x").html;
    assert!(
        html.contains("<title>a&lt;b &amp; &quot;c&quot;</title>"),
        "{html}"
    );
}

#[test]
fn gfm_renders() {
    let html = welcome();
    for needle in [
        "<table>",
        "type=\"checkbox\"",
        "<del>",
        "class=\"footnotes\"",
    ] {
        assert!(html.contains(needle), "missing {needle}");
    }
}

#[test]
fn note_title_is_file_stem() {
    assert_eq!(note_title("people/Jane Doe.md"), "Jane Doe");
    assert_eq!(note_title("Upper.MD"), "Upper");
    assert_eq!(note_title("welcome.md"), "welcome");
}

#[test]
fn version_changes_with_content() {
    assert_ne!(render_note("a", "one").version, render_note("a", "two").version);
    assert_ne!(render_note("a", "one").version, render_note("b", "one").version);
}

#[test]
fn version_is_stable() {
    let version = render_note("a", "one").version;
    assert_eq!(version, render_note("a", "one").version);
    assert_eq!(version.len(), 16);
    assert!(version.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
}

#[test]
fn page_carries_version_and_live_script() {
    let page = render_note("a", "one");
    let meta = format!("<meta name=\"tachylite-version\" content=\"{}\">", page.version);
    assert!(page.html.contains(&meta), "{}", page.html);
    assert!(page.html.contains("<script src=\"/.tachylite/live.js\" defer></script>"));
}
