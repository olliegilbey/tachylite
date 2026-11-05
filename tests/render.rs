#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tachylite::build::build;
use tachylite::links::Index;
use tachylite::render::{Broken, Page, note_title, render_body, render_page};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vault")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// A page with no links to resolve and no backlinks.
fn render_note(title: &str, markdown: &str) -> Page {
    render_page(
        title,
        &render_body("x.md", markdown, &Index::default()).html,
        &[],
    )
}

fn welcome() -> String {
    render_note("welcome", &fixture("welcome.md")).html
}

/// Builds the fixture vault and returns the page for `key`.
fn built(key: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault");
    build(&vault, &tmp.path().join("out")).unwrap();
    std::fs::read_to_string(tmp.path().join(format!("out/pages/{key}.html"))).unwrap()
}

#[test]
fn links_snapshot() {
    insta::assert_snapshot!(built("notes/links"));
}

#[test]
fn jane_snapshot() {
    insta::assert_snapshot!(built("people/Jane Doe"));
}

#[test]
fn links_resolve_to_percent_encoded_urls() {
    let index = Index::new(["people/Jane Doe.md", "C# notes.md", "maps/route.html"]);
    let body = render_body(
        "a.md",
        "[[jane doe|Jane]] [y](C%23%20notes.md#Part%20One) [[route.html]] [x](people/Jane%20Doe.md#Top)",
        &index,
    );
    for href in [
        "<a href=\"/people/Jane%20Doe\">Jane</a>",
        "<a href=\"/C%23%20notes#part-one\">",
        "<a href=\"/maps/route.html\">",
        "<a href=\"/people/Jane%20Doe#top\">x</a>",
    ] {
        assert!(body.html.contains(href), "missing {href} in {}", body.html);
    }
    let targets: Vec<&str> = body.targets.iter().map(|t| &**t).collect();
    assert_eq!(
        targets,
        ["C# notes.md", "maps/route.html", "people/Jane Doe.md"]
    );
    assert!(body.broken.is_empty());
}

#[test]
fn broken_and_ambiguous_links_render_without_href() {
    let index = Index::new(["a/r.md", "b/r.md"]);
    let body = render_body("n.md", "[[nobody|Who]] and [[r]] and [x](gone.md)", &index);
    assert!(
        body.html.contains("<a class=\"broken-link\">Who</a>"),
        "{}",
        body.html
    );
    assert_eq!(body.html.matches("broken-link").count(), 3, "{}", body.html);
    assert!(!body.html.contains("href"), "{}", body.html);
    let broken = |target: &str, matches| Broken {
        from: "n.md".into(),
        target: target.into(),
        matches,
    };
    assert_eq!(
        body.broken,
        [broken("nobody", 0), broken("r", 2), broken("gone.md", 0)]
    );
}

#[test]
fn links_with_a_scheme_or_a_slash_stay_as_written() {
    let body = render_body(
        "n.md",
        "[a](https://example.com/x.md) [b](/abs) [c](#here) [d](mailto:x@example.com)",
        &Index::default(),
    );
    for href in [
        "https://example.com/x.md",
        "/abs",
        "#here",
        "mailto:x@example.com",
    ] {
        assert!(
            body.html.contains(&format!("href=\"{href}\"")),
            "{}",
            body.html
        );
    }
    assert!(body.targets.is_empty());
    assert!(body.broken.is_empty());
}

#[test]
fn every_heading_gets_its_own_id() {
    let body = render_body(
        "n.md",
        "# A\n\n---\n\n## A\n\n| x |\n|---|\n| y |\n\n### B",
        &Index::default(),
    );
    for tag in [
        "<h1 id=\"a\">A</h1>",
        "<h2 id=\"a-1\">A</h2>",
        "<h3 id=\"b\">B</h3>",
        "<hr />",
        "<th>x</th>",
    ] {
        assert!(body.html.contains(tag), "missing {tag} in {}", body.html);
    }
}

#[test]
fn heading_links_use_heading_ids() {
    let body = render_body(
        "n.md",
        "# Some Heading\n\n[[#Some Heading]]",
        &Index::default(),
    );
    assert!(body.html.contains("id=\"some-heading\""), "{}", body.html);
    assert!(
        body.html.contains("<a href=\"#some-heading\">"),
        "{}",
        body.html
    );
    assert!(body.targets.is_empty());
}

#[test]
fn backlinks_list_at_the_foot_of_main() {
    let page = render_page(
        "b",
        "<p>body</p>\n",
        &[("a<1>".to_string(), "/a%3C1%3E".to_string())],
    );
    assert!(
        page.html.contains(
            "<p>body</p>\n<section class=\"backlinks\"><h2>Backlinks</h2><ul>\
             <li><a href=\"/a%3C1%3E\">a&lt;1&gt;</a></li></ul></section>\n</main>"
        ),
        "{}",
        page.html
    );
    assert!(
        !render_page("b", "<p>body</p>\n", &[])
            .html
            .contains("backlinks")
    );
}

#[test]
fn version_covers_backlinks() {
    let link = [("a".to_string(), "/a".to_string())];
    assert_ne!(
        render_page("b", "x", &[]).version,
        render_page("b", "x", &link).version
    );
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
    assert_ne!(
        render_note("a", "one").version,
        render_note("a", "two").version
    );
    assert_ne!(
        render_note("a", "one").version,
        render_note("b", "one").version
    );
}

#[test]
fn version_is_stable() {
    let version = render_note("a", "one").version;
    assert_eq!(version, render_note("a", "one").version);
    assert_eq!(version.len(), 16);
    assert!(
        version
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
}

#[test]
fn page_carries_version_and_live_script() {
    let page = render_note("a", "one");
    let meta = format!(
        "<meta name=\"tachylite-version\" content=\"{}\">",
        page.version
    );
    assert!(page.html.contains(&meta), "{}", page.html);
    assert!(
        page.html
            .contains("<script src=\"/.tachylite/live.js\" defer></script>")
    );
}
