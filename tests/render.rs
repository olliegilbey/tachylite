#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use tachylite::build::build;
use tachylite::links::Index;
use tachylite::render::{
    Body, Broken, Page, Sources, embed_targets, note_title, render_body, render_page,
};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(common::fixture().join(name)).unwrap()
}

/// A page with no links to resolve and no backlinks.
fn render_note(title: &str, markdown: &str) -> Page {
    render_page(
        "x.md",
        title,
        &render_body("x.md", markdown, &Index::default(), &Sources::new()).html,
        &[],
    )
}

fn welcome() -> String {
    render_note("welcome", &fixture("welcome.md")).html
}

/// Builds the fixture vault and returns the page for `key`.
fn built(key: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    build(&common::fixture(), &tmp.path().join("out")).unwrap();
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
        &Sources::new(),
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
    let body = render_body(
        "n.md",
        "[[nobody|Who]] and [[r]] and [x](gone.md)",
        &index,
        &Sources::new(),
    );
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
        &Sources::new(),
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
        &Sources::new(),
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
        &Sources::new(),
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
fn markdown_heading_links_use_heading_ids() {
    let html = plain("# Some Heading\n\n[x](#Some%20Heading) [y](#some-heading)");
    assert_eq!(
        html.matches("<a href=\"#some-heading\">").count(),
        2,
        "{html}"
    );
    assert!(plain("[top](#)").contains("<a href=\"#\">top</a>"));
}

#[test]
fn a_tag_can_start_a_heading() {
    let html = plain("# #start of it\n\n## #only");
    for want in [
        "<h1 id=\"start-of-it\"><a class=\"tag\" href=\"/.tachylite/tags/start\">#start</a> of it</h1>",
        "<h2 id=\"only\"><a class=\"tag\" href=\"/.tachylite/tags/only\">#only</a></h2>",
    ] {
        assert!(html.contains(want), "{want} not in {html}");
    }
}

#[test]
fn frontmatter_tabs_comma_tags_and_empty_items() {
    let html = plain("---\ntitle:\tTabbed\ntags: a, b\nlist:\n  - x\n  -\n  - y\n---\n");
    for want in [
        "<dt>title</dt><dd>Tabbed</dd>",
        "<dd><a class=\"tag\" href=\"/.tachylite/tags/a\">#a</a>, \
         <a class=\"tag\" href=\"/.tachylite/tags/b\">#b</a></dd>",
        "<dt>list</dt><dd>x, y</dd>",
    ] {
        assert!(html.contains(want), "{want} not in {html}");
    }
}

#[test]
fn backlinks_list_at_the_foot_of_main() {
    let page = render_page(
        "b.md",
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
        !render_page("b.md", "b", "<p>body</p>\n", &[])
            .html
            .contains("backlinks")
    );
}

#[test]
fn version_covers_backlinks() {
    let link = [("a".to_string(), "/a".to_string())];
    assert_ne!(
        render_page("b.md", "b", "x", &[]).version,
        render_page("b.md", "b", "x", &link).version
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

#[test]
fn properties_block_lists_every_key() {
    let body = render_body(
        "welcome.md",
        &fixture("welcome.md"),
        &Index::default(),
        &Sources::new(),
    );
    assert!(body.html.starts_with(
        "<dl class=\"properties\"><dt>title</dt><dd>Welcome</dd><dt>tags</dt><dd>\
         <a class=\"tag\" href=\"/.tachylite/tags/guide\">#guide</a>, \
         <a class=\"tag\" href=\"/.tachylite/tags/sample\">#sample</a></dd></dl>\n"
    ));
    assert!(!body.html.contains("title: Welcome"));
}

#[test]
fn property_values_are_escaped() {
    let body = render_body(
        "a.md",
        "---\n\"<k>\": \"a <b> & c\"\nlist: [x, \"<y>\"]\n---\n",
        &Index::default(),
        &Sources::new(),
    );
    assert!(body.html.starts_with(
        "<dl class=\"properties\"><dt>&lt;k&gt;</dt><dd>a &lt;b&gt; &amp; c</dd>\
         <dt>list</dt><dd>x, &lt;y&gt;</dd></dl>\n"
    ));
}

#[test]
fn property_wikilink_resolves_and_counts_as_target() {
    let index = Index::new(["people/Jane Doe.md"]);
    let body = render_body(
        "a.md",
        "---\nauthor: \"[[jane doe|Jane]]\"\nup: [[nobody]]\nnot: \"see [[jane doe]]\"\n---\n",
        &index,
        &Sources::new(),
    );
    assert!(
        body.html
            .contains("<dt>author</dt><dd><a href=\"/people/Jane%20Doe\">Jane</a></dd>")
    );
    assert!(
        body.html
            .contains("<dt>up</dt><dd><a class=\"broken-link\">nobody</a></dd>")
    );
    assert!(body.html.contains("<dt>not</dt><dd>see [[jane doe]]</dd>"));
    assert_eq!(
        body.targets.iter().map(|t| &**t).collect::<Vec<_>>(),
        ["people/Jane Doe.md"]
    );
    assert_eq!(
        body.broken,
        [Broken {
            from: "a.md".into(),
            target: "nobody".into(),
            matches: 0
        }]
    );
}

#[test]
fn syntax_snapshot() {
    insta::assert_snapshot!(built("notes/syntax"));
}

fn plain(markdown: &str) -> String {
    render_body("a.md", markdown, &Index::default(), &Sources::new()).html
}

#[test]
fn tags_render_as_links() {
    let html = plain("x #a/b-c, and #Über #t/ ok\n\n# Head #h");
    for want in [
        "x <a class=\"tag\" href=\"/.tachylite/tags/a/b-c\">#a/b-c</a>, and ",
        "<a class=\"tag\" href=\"/.tachylite/tags/%C3%BCber\">#Über</a> ",
        "<a class=\"tag\" href=\"/.tachylite/tags/t\">#t</a>/ ok",
        "<h1 id=\"head-h\">Head <a class=\"tag\" href=\"/.tachylite/tags/h\">#h</a></h1>",
    ] {
        assert!(html.contains(want), "{want} not in {html}");
    }
}

#[test]
fn not_tags() {
    let html = plain("C#x #123 `#c` [#l](u) https://e.com/#a (#p) #\n\n```\n#f\n```");
    assert!(!html.contains("class=\"tag\""), "{html}");
}

#[test]
fn callouts_and_highlight() {
    let html = plain("> [!tip]- Folded\n> body\n\n==mark==");
    assert!(
        html.contains("<details class=\"callout\" data-callout=\"tip\">"),
        "{html}"
    );
    assert!(html.contains("<mark>mark</mark>"));
}

#[test]
fn foldable_callouts_render_as_details() {
    let html =
        plain("> [!tip]- Folded *tip*\n> body\n\n> [!faq]+\n> shown\n\n> [!note]\n> fixed\n");
    assert_eq!(
        html,
        "<details class=\"callout\" data-callout=\"tip\">\n\
         <summary class=\"callout-title\">Folded <em>tip</em></summary>\n\
         <div class=\"callout-content\">\n<p>body</p>\n</div>\n</details>\n\
         <details class=\"callout\" data-callout=\"faq\" open>\n\
         <summary class=\"callout-title\">Faq</summary>\n\
         <div class=\"callout-content\">\n<p>shown</p>\n</div>\n</details>\n\
         <div class=\"callout\" data-callout=\"note\" data-callout-fold=\"none\">\n\
         <div class=\"callout-title\">Note</div>\n\
         <div class=\"callout-content\">\n<p>fixed</p>\n</div>\n</div>\n"
    );
}

#[test]
fn stray_comment_hides_to_its_pair() {
    let html = plain("one %%zebra\nyak%% two\n\nthree %%lynx\n\n- moth\n\nnewt%% four\n\nfive");
    for gone in ["zebra", "yak", "lynx", "moth", "newt"] {
        assert!(!html.contains(gone), "{gone} in {html}");
    }
    for kept in ["one", "two", "three", "four", "five"] {
        assert!(html.contains(kept), "{kept} not in {html}");
    }
}

#[test]
fn unclosed_stray_comment_hides_to_end() {
    let index = Index::new(["b.md"]);
    let body = render_body(
        "a.md",
        "one %%zebra [[b]]\n\n# yak\n\n- moth",
        &index,
        &Sources::new(),
    );
    for gone in ["zebra", "yak", "moth", "<h1"] {
        assert!(!body.html.contains(gone), "{gone} in {}", body.html);
    }
    assert!(body.html.contains("one"));
    assert!(body.targets.is_empty());
}

#[test]
fn comments_in_code_stay() {
    let html = plain("`%%` and\n\n```\n%%\n```\n\nafter");
    assert_eq!(html.matches("%%").count(), 2, "{html}");
    assert!(html.contains("after"));
}

#[test]
fn mermaid_fence_renders_pre() {
    assert_eq!(
        plain("```mermaid\nA-->B & <C>\n```\n\n```rust\nx\n```"),
        "<pre class=\"mermaid\">A--&gt;B &amp; &lt;C&gt;\n</pre>\n\
         <pre><code class=\"language-rust\">x\n</code></pre>\n"
    );
}

#[test]
fn escaped_hash_is_not_a_tag() {
    let html = plain("\\#notatag and \\\\ #real");
    assert!(!html.contains("tags/notatag"), "{html}");
    assert!(html.contains("#notatag"), "{html}");
    assert!(html.contains("tags/real"), "{html}");
}

#[test]
fn comment_wins_over_code_it_holds() {
    let html = plain("%%\n```\nzebra\n```\n%% after");
    assert!(!html.contains("zebra"), "{html}");
    assert!(html.contains("after"), "{html}");
}

#[test]
fn comments_in_code_spans_and_fences_in_containers_stay() {
    let html =
        plain("a `x %% y` b %%zebra%% c\n\n> ```\n> %% kept\n> ```\n\n- ```\n  %% kept too\n  ```");
    assert!(!html.contains("zebra"), "{html}");
    assert!(html.contains("x %% y"), "{html}");
    assert!(html.contains("%% kept\n"), "{html}");
    assert!(html.contains("%% kept too"), "{html}");
}

#[test]
fn disagreement_on_code_withholds_the_text() {
    // The stripper reads a fence inside the HTML block; the parser reads HTML, then text.
    let index = Index::new(["b.md"]);
    let body = render_body(
        "a.md",
        "<div>\n```\n</div>\n\n%% zebra [[b]] %%\n```\n",
        &index,
        &Sources::new(),
    );
    assert!(
        body.html.contains("class=\"comment-error\""),
        "{}",
        body.html
    );
    assert!(!body.html.contains("zebra"));
    assert!(body.targets.is_empty());
}

#[test]
fn code_spans_and_indented_code_keep_their_percent_signs() {
    for (md, kept) in [
        (
            "Intro.\n\n    printf(\"%%d\\n\", n);\n\nAfter %%zebra%% this.",
            "this",
        ),
        (
            "Use `printf(\"%%d\",\nn)` to print. %%zebra%% Rest.",
            "Rest",
        ),
        // A heading is one line, so a code span cannot run out of it.
        ("# Title `x\nmore `%%d` %%zebra%% `y`", "more"),
        // Indented four columns, list content is text, not code.
        (
            "- a\n\n    more %%zebra%% text\n\n\t- b %%zebra%% c `%%d`",
            "c",
        ),
    ] {
        let html = plain(md);
        assert!(html.contains("%%d"), "{html}");
        assert!(html.contains(kept), "{kept} not in {html}");
        assert!(!html.contains("zebra"), "{html}");
        assert!(!html.contains("comment-error"), "{html}");
    }
}

#[test]
fn an_escape_never_hides_a_comment() {
    let html = plain("a \\%%zebra%% b");
    assert!(html.contains(" b") && !html.contains("zebra"), "{html}");
    assert!(!html.contains("comment-error"), "{html}");
    // In code to the stripper, but text to the parser: `\%%` parses as two nodes.
    let html = plain("<div>\n```\n</div>\n\n\\%%zebra\n```\n");
    assert!(html.contains("comment-error"), "{html}");
    assert!(!html.contains("zebra"), "{html}");
}

#[test]
fn only_spaces_and_tabs_indent() {
    // U+3000 is text, so the next line opens a fence, as the parser reads it.
    let html = plain("\u{3000}```\n```\nx %%a\n%%b%%\n```\n");
    assert!(
        html.contains("<pre><code>x %%a\n%%b%%\n</code></pre>"),
        "{html}"
    );
}

#[test]
fn an_unclosed_comment_shows_a_notice() {
    let html = plain("one %%zebra\n\nyak");
    assert!(html.contains("one"), "{html}");
    assert!(!html.contains("zebra") && !html.contains("yak"), "{html}");
    assert!(html.contains("class=\"comment-error\""), "{html}");
    assert!(!plain("one %%zebra%% two").contains("comment-error"));
}

#[test]
fn inline_html_keeps_formatting() {
    let html = plain(
        "a <span class=\"x\">b</span> <kbd>K</kbd>\n\n<details><summary>S</summary>\n\nbody\n\n</details>\n",
    );
    assert!(html.contains("<span class=\"x\">b</span>"), "{html}");
    assert!(html.contains("<kbd>K</kbd>"), "{html}");
    assert!(html.contains("<details><summary>S</summary>"), "{html}");
}

#[test]
fn inline_html_drops_scripts() {
    let html = plain(
        "<script>alert(1)</script>\n\n<img src=\"x.png\" onerror=\"alert(2)\">\n\n\
         <a href=\"javascript:alert(3)\">j</a> <object data=\"x\"></object><embed src=\"y\">\n\n\
         <p style=\"color:red\">s</p>\n",
    );
    for bad in [
        "<script",
        "alert",
        "onerror",
        "javascript:",
        "<object",
        "<embed",
        "style=",
    ] {
        assert!(!html.contains(bad), "{bad} in {html}");
    }
    assert!(html.contains("<img src=\"x.png\">"), "{html}");
}

#[test]
fn author_iframe_is_dropped() {
    let html = plain(
        "<iframe src=\"maps/route.html\" sandbox=\"allow-scripts allow-same-origin\"></iframe>\n\nafter\n",
    );
    assert!(!html.contains("<iframe"), "{html}");
    assert!(html.contains("after"), "{html}");
}

#[test]
fn raw_heading_takes_no_heading_id() {
    let html = plain("<h2>raw</h2>\n\n## Real\n");
    assert!(html.contains("<h2>raw</h2>"), "{html}");
    assert!(html.contains("<h2 id=\"real\">Real</h2>"), "{html}");
}

#[test]
fn sanitizer_keeps_tachylite_markup() {
    let md = fixture("notes/syntax.md") + "\n- [x] done\n\n[[welcome]] [[nobody]]\n\n<b>raw</b>\n";
    let index = Index::new(["welcome.md", "notes/syntax.md"]);
    let html = render_body("notes/syntax.md", &md, &index, &Sources::new()).html;
    for want in [
        "<dl class=\"properties\">",
        "<a class=\"tag\" href=\"/.tachylite/tags/garden\">",
        "<h1 id=\"syntax-tour-heading\">",
        "<div class=\"callout\" data-callout=\"warning\"",
        "<mark>highlighted</mark>",
        "<sup class=\"footnote-ref\"><a href=\"#fn-1\" id=\"fnref-1\" data-footnote-ref",
        "aria-label=\"Back to reference 1\"",
        "<section class=\"footnotes\" data-footnotes",
        "<input type=\"checkbox\" checked=\"\" disabled=\"\"",
        "<pre class=\"mermaid\">",
        "<a href=\"/welcome\">welcome</a>",
        "<a class=\"broken-link\">nobody</a>",
        "<b>raw</b>",
    ] {
        assert!(html.contains(want), "{want} missing from {html}");
    }
}

fn sources(notes: &[(&str, &str)]) -> Sources {
    notes
        .iter()
        .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
        .collect()
}

/// Renders `md` as `from`, beside `notes` (loaded as embed sources) and `files`.
fn embed(from: &str, md: &str, notes: &[(&str, &str)], files: &[&str]) -> Body {
    let paths = notes.iter().map(|(p, _)| *p).chain(files.iter().copied());
    let index = Index::new(paths.chain([from]));
    render_body(from, md, &index, &sources(notes))
}

fn paths(set: &std::collections::BTreeSet<std::sync::Arc<str>>) -> Vec<&str> {
    set.iter().map(|p| &**p).collect()
}

#[test]
fn a_note_embed_splits_its_paragraph() {
    let body = embed(
        "a.md",
        "Before ![[b]] after.\n![[c]]\n![[d]]\n",
        &[("b.md", "Bee"), ("c.md", "Cee"), ("d.md", "Dee")],
        &[],
    );
    assert!(!body.html.contains("<p><div"), "{}", body.html);
    assert!(!body.html.contains("</div>\n</p>"), "{}", body.html);
    for want in [
        "<p>Before</p>\n<div class=\"embed\"><a class=\"embed-link\" href=\"/b\">b</a>\n<p>Bee</p>\n</div>\n<p>after.</p>\n<div class=\"embed\">",
        "<p>Dee</p>\n</div>\n",
    ] {
        assert!(body.html.contains(want), "{want} not in {}", body.html);
    }
}

#[test]
fn image_embed_renders_img() {
    let body = embed(
        "a.md",
        "![[dot.png]] ![[img/dot.png|A dot]] ![[dot.png|300]] ![[dot.png|300x200]]\n",
        &[],
        &["img/dot.png"],
    );
    assert!(
        body.html
            .contains("<img src=\"/img/dot.png\" alt=\"dot.png\" loading=\"lazy\">"),
        "{}",
        body.html
    );
    assert!(body.html.contains("alt=\"A dot\""), "{}", body.html);
    assert_eq!(
        body.html.matches("alt=\"dot.png\"").count(),
        3,
        "{}",
        body.html
    );
    assert_eq!(paths(&body.targets), ["img/dot.png"]);
}

#[test]
fn markdown_image_resolves_like_a_link() {
    let body = embed(
        "notes/a.md",
        "![A dot](dot.png) ![gone](nope.png) ![r](https://example.com/i.png)\n",
        &[],
        &["img/dot.png"],
    );
    assert!(
        body.html
            .contains("<img src=\"/img/dot.png\" alt=\"A dot\""),
        "{}",
        body.html
    );
    assert!(
        body.html.contains("<img src=\"nope.png\" alt=\"gone\""),
        "{}",
        body.html
    );
    assert!(
        body.html.contains("src=\"https://example.com/i.png\""),
        "{}",
        body.html
    );
    assert_eq!(
        body.broken,
        [Broken {
            from: "notes/a.md".to_string(),
            target: "nope.png".to_string(),
            matches: 0
        }]
    );
}

#[test]
fn html_embed_renders_a_sandboxed_iframe() {
    let body = embed("a.md", "![[route.html]]\n", &[], &["maps/route.html"]);
    assert!(
        body.html.contains(
            "<iframe class=\"embed-html\" sandbox=\"allow-scripts\" src=\"/maps/route.html\" \
             loading=\"lazy\"></iframe>"
        ),
        "{}",
        body.html
    );
    assert!(!body.html.contains("<p><iframe"), "{}", body.html);
}

#[test]
fn html_embed_snapshot() {
    let body = embed("a.md", "before ![[route.html]] after\n", &[], &["maps/route.html"]);
    insta::assert_snapshot!(body.html);
}

#[test]
fn note_embed_transcludes_the_body() {
    let body = embed(
        "a.md",
        "Before.\n\n![[b]]\n",
        &[
            (
                "b.md",
                "---\ntitle: Bee\nk: v\n---\n# B\n\nSee [[c]] and [[gone]].\n",
            ),
            ("c.md", "C"),
        ],
        &[],
    );
    assert!(
        body.html.contains(
            "<div class=\"embed\"><a class=\"embed-link\" href=\"/b\">Bee</a>\n<h1 id=\"e1.b\">B</h1>"
        ),
        "{}",
        body.html
    );
    assert!(body.html.contains("<a href=\"/c\">c</a>"), "{}", body.html);
    assert!(!body.html.contains("properties"), "{}", body.html);
    assert!(!body.html.contains("<p><div"), "{}", body.html);
    assert_eq!(
        paths(&body.targets),
        ["b.md"],
        "an embed's own links stay its own"
    );
    assert!(body.broken.is_empty(), "{:?}", body.broken);
    assert_eq!(paths(&body.embeds), ["b.md"]);
}

#[test]
fn embeds_nest_three_deep() {
    let body = embed(
        "a.md",
        "![[b]]",
        &[
            ("b.md", "bee ![[c]]"),
            ("c.md", "sea ![[d]]"),
            ("d.md", "dee ![[e]]"),
            ("e.md", "eee"),
        ],
        &[],
    );
    for want in ["bee", "sea", "dee", "<a href=\"/e\">e</a>"] {
        assert!(
            body.html.contains(want),
            "{want} missing from {}",
            body.html
        );
    }
    assert!(!body.html.contains("eee"), "{}", body.html);
    assert_eq!(paths(&body.embeds), ["b.md", "c.md", "d.md"]);
}

#[test]
fn embed_cycles_become_links() {
    let body = embed("a.md", "self ![[a]] ![[b]]", &[("b.md", "bee ![[a]]")], &[]);
    assert_eq!(body.html.matches("self").count(), 1, "{}", body.html);
    assert_eq!(
        body.html.matches("<a href=\"/a\">a</a>").count(),
        2,
        "{}",
        body.html
    );
    assert!(body.html.contains("bee"), "{}", body.html);
}

#[test]
fn other_embeds_are_links() {
    let body = embed(
        "a.md",
        "![[b#Part]] ![[guide.pdf]] ![[nope]] ![[c]]",
        &[("b.md", "# Part\n\nbee")],
        &["docs/guide.pdf", "c.md"],
    );
    assert!(
        body.html.contains("<a href=\"/b#part\">b#Part</a>"),
        "{}",
        body.html
    );
    assert!(
        body.html
            .contains("<a href=\"/docs/guide.pdf\">guide.pdf</a>"),
        "{}",
        body.html
    );
    assert!(
        body.html.contains("<a class=\"broken-link\">nope</a>"),
        "{}",
        body.html
    );
    assert!(
        body.html.contains("<a href=\"/c\">c</a>"),
        "unloaded source: {}",
        body.html
    );
    assert!(!body.html.contains("bee"), "{}", body.html);
    assert_eq!(body.broken.len(), 1);
    assert!(body.embeds.is_empty());
}

#[test]
fn embed_targets_lists_the_notes_to_load() {
    let index = Index::new(["a.md", "b.md", "c.md", "d.md", "x.png"]);
    let targets = embed_targets(
        "a.md",
        "![[b]] `![[c]]` ![[d#h]] ![[x.png]] ![[b|again]] ![[nope]]",
        &index,
    );
    assert_eq!(
        targets.iter().map(|p| &**p).collect::<Vec<_>>(),
        ["b.md", "c.md"]
    );
}

#[test]
fn script_urls_in_markdown_are_dropped() {
    let html = plain(
        "[x](javascript:alert(1)) <javascript:alert(2)> [y](&#106;avascript:alert(3)) \
         [z](vbscript:x) ![i](javascript:alert(4)) [ok](https://example.com) <https://example.com/a>\n",
    );
    for bad in ["=\"javascript", "=\"vbscript", "=\"&#106;"] {
        assert!(!html.contains(bad), "{bad} in {html}");
    }
    assert!(html.contains("<a>x</a>"), "{html}");
    assert!(
        html.contains("<a href=\"https://example.com\">ok</a>"),
        "{html}"
    );
    assert!(
        html.contains("<a href=\"https://example.com/a\">"),
        "{html}"
    );
}

#[test]
fn a_note_transcludes_once_per_page() {
    let body = embed(
        "a.md",
        "![[b]]\n\n![[b]]\n\n![[c]]",
        &[
            ("b.md", "bee ![[d]]"),
            ("c.md", "sea ![[d]]"),
            ("d.md", "dee"),
        ],
        &[],
    );
    assert_eq!(body.html.matches("bee").count(), 1, "{}", body.html);
    assert_eq!(body.html.matches("dee").count(), 1, "{}", body.html);
    assert!(body.html.contains("<a href=\"/b\">b</a>"), "{}", body.html);
    assert!(body.html.contains("<a href=\"/d\">d</a>"), "{}", body.html);
}

#[test]
fn render_reports_embeds_it_could_not_load() {
    let index = Index::new(["a.md", "n.md"]);
    let body = render_body(
        "n.md",
        "| t |\n|---|\n| ![[a\\|x]] |\n",
        &index,
        &Sources::new(),
    );
    assert_eq!(paths(&body.missing), ["a.md"]);
    assert!(body.embeds.is_empty());
}

/// The page with the contents of every `<code>` and `<pre>` element removed. Code keeps its
/// `%%` by design; everything else, attributes included, must not.
fn outside_code(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    let mut depth = 0usize;
    while let Some(i) = rest.find('<') {
        if depth == 0 {
            out.push_str(&rest[..i]);
        }
        let end = rest[i..].find('>').map_or(rest.len(), |j| i + j + 1);
        let tag = &rest[i..end];
        if tag.starts_with("<code") || tag.starts_with("<pre") {
            if depth == 0 {
                out.push_str(tag);
            }
            depth += 1;
        } else if tag.starts_with("</code") || tag.starts_with("</pre") {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            // A heading id slugs the heading's text, code spans included.
            let id = tag
                .find(" id=\"")
                .filter(|_| tag.starts_with("<h"))
                .and_then(|at| Some((at, at + 5 + tag[at + 5..].find('"')? + 1)));
            match id {
                Some((from, to)) => out.push_str(&format!("{}{}", &tag[..from], &tag[to..])),
                None => out.push_str(tag),
            }
        }
        rest = &rest[end..];
    }
    if depth == 0 {
        out.push_str(rest);
    }
    out
}

/// The parser's own reading of a note, comments and all, with Tachylite's extensions.
fn raw_html(md: &str) -> String {
    let mut options = comrak::Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.extension.ofm_callouts = true;
    options.extension.highlight = true;
    options.parse.escaped_char_spans = true;
    options.render.r#unsafe = true;
    comrak::markdown_to_html(md, &options)
}

/// Block and inline syntax that moves where code starts and ends.
const SYNTAX: &[&str] = &[
    "```",
    "~~~",
    "```md",
    "    ",
    "  ",
    "\t",
    "\n",
    "\n",
    "\n",
    "\n\n",
    "- ",
    "1. ",
    "2) ",
    "> ",
    "# ",
    "---",
    "<div>",
    "</div>",
    "<details>",
    "<span title=\"",
    "\">",
    "<p>",
    "[x](https://example.com/ \"",
    "\")",
    "[x](<",
    ">)",
    "`",
    "`",
    "``",
    "word",
    "*",
    "<!--",
    "-->",
    "|",
    "[[",
    "]]",
    "[^1]",
    "[^1]: ",
    "> [!note] ",
];

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(2000))]

    /// Whatever the note, no `%%` reaches the page outside code.
    #[test]
    fn no_comment_marker_outside_code(
        parts in proptest::collection::vec(
            proptest::sample::select(
                [SYNTAX, &["%%", "%%", "%", "\\"]].concat()
            ),
            0..60,
        )
    ) {
        // `%\%` escapes its second `%`, and a dropped HTML comment can join two `%`. Neither
        // is a marker, so a space keeps them apart. A blank first line rules out frontmatter,
        // which is YAML, where `%%` is a plain value.
        let md = format!("\n{}", parts.concat())
            .replace("%\\%", "% \\%")
            .replace("%<!--", "% <!--");
        let html = render_body("a.md", &md, &Index::new(["a.md"]), &Sources::new()).html;
        let text = outside_code(&html);
        proptest::prop_assert!(!text.contains("%%"), "md={md:?}\nhtml={html}");
    }

    /// In a note whose every `%%` outside code pairs into a comment, no comment text reaches
    /// the page outside code.
    #[test]
    fn no_comment_text_outside_code(
        parts in proptest::collection::vec(
            proptest::sample::select(
                [SYNTAX, &[
                    "%%zebra%%", "%%zebra%%", "\n\n%%\nzebra\n%%\n\n",
                    // `%%` in code: if the stripper reads it as text, the pairs shift.
                    "\n```\nx %%d\n```\n", "`%%d`", "\n\n    %%d\n\n",
                ]].concat()
            ),
            0..60,
        )
    ) {
        let md = format!("\n{}", parts.concat());
        // A `%%d` that its neighbours took out of code opens a comment, so its pairs differ.
        let raw = raw_html(&md);
        let in_code = raw.matches("%%d").count() - outside_code(&raw).matches("%%d").count();
        if in_code != md.matches("%%d").count() {
            return Ok(());
        }
        let html = render_body("a.md", &md, &Index::new(["a.md"]), &Sources::new()).html;
        let text = outside_code(&html);
        proptest::prop_assert!(!text.contains("zebra"), "md={md:?}\nhtml={html}");
    }
}

/// Every `attr="…"` value in `html`.
fn attrs<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let pat = format!(" {attr}=\"");
    html.match_indices(&pat)
        .map(|(at, _)| {
            let rest = &html[at + pat.len()..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect()
}

#[test]
fn embedded_ids_stay_unique_and_their_links_stay_inside() {
    let note = "# Top\n\nText[^1] and [[#Top]].\n\n[^1]: A note.\n";
    let body = embed(
        "a.md",
        &format!("{note}\n![[b]]\n"),
        &[("b.md", &format!("{note}\n![[c]]\n")), ("c.md", note)],
        &[],
    );
    let ids = attrs(&body.html, "id");
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
    assert_eq!(ids.len(), 9, "{ids:?}");
    for href in attrs(&body.html, "href") {
        if let Some(fragment) = href.strip_prefix('#') {
            assert!(
                ids.contains(&fragment),
                "{href} targets no id in {}",
                body.html
            );
        }
    }
    // The host's own ids keep their plain names.
    for id in ["top", "fn-1", "fnref-1"] {
        assert!(ids.contains(&id), "{id} not in {ids:?}");
    }
}
