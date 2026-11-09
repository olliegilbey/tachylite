#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tachylite::build::build;
use tachylite::links::Index;
use tachylite::render::{
    Body, Broken, Page, Sources, embed_targets, note_title, render_body, render_page,
};

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
        html.contains(
            "<div class=\"callout\" data-callout=\"tip\" data-callout-fold=\"collapsed\">"
        )
    );
    assert!(html.contains("<mark>mark</mark>"));
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
    // comrak's code span crosses the line break; the stripper reads spans within a line.
    let index = Index::new(["b.md"]);
    let body = render_body(
        "a.md",
        "`a\nb` %% zebra [[b]] `d`\n",
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
fn author_iframe_is_sandboxed() {
    let html = plain(
        "<iframe src=\"maps/route.html\" sandbox=\"allow-scripts allow-same-origin\" onload=\"x()\"></iframe>\n",
    );
    assert!(html.contains("sandbox=\"allow-scripts\""), "{html}");
    assert!(!html.contains("allow-same-origin"), "{html}");
    assert!(!html.contains("onload"), "{html}");
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
            "<div class=\"embed\"><a class=\"embed-link\" href=\"/b\">Bee</a>\n<h1 id=\"b\">B</h1>"
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
