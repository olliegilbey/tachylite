#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use tachylite::sanitize::sanitize;

#[test]
fn keeps_formatting_tags_and_their_safe_attributes() {
    assert_eq!(
        sanitize("<span class=\"x\" style=\"color:red\">b</span>"),
        "<span class=\"x\">b</span>"
    );
    assert_eq!(
        sanitize("<details open><summary>S</summary>"),
        "<details open=\"\"><summary>S</summary>"
    );
    assert_eq!(
        sanitize("<img src=x.png alt='A \"q\"' width=10 onerror=alert(1)>"),
        "<img src=\"x.png\" alt=\"A &quot;q&quot;\" width=\"10\">"
    );
    assert_eq!(
        sanitize("<a href=\"https://example.com/?a=1&amp;b=2\" target=_blank>x</a>"),
        "<a href=\"https://example.com/?a=1&amp;b=2\">x</a>"
    );
    assert_eq!(sanitize("<BR/>a<Hr />"), "<br>a<hr>");
}

#[test]
fn drops_scripts_and_their_content() {
    for (input, want) in [
        ("<script>alert(1)</script>after", "after"),
        ("<SCRIPT src=x></SCRIPT >after", "after"),
        ("<style>p{}</style>a", "a"),
        ("<svg onload=alert(1)><circle/></svg>a", "a"),
        ("<math><mtext><style><img src=x onerror=alert(1)>", ""),
        (
            "<iframe srcdoc=\"<script>alert(1)</script>\"></iframe>a",
            "a",
        ),
        ("<script>never closed", ""),
        ("<object data=x>fallback</object>", "fallback"),
        (
            "<embed src=x><base href=//e.x><meta http-equiv=refresh><link rel=x>a",
            "a",
        ),
        (
            "<form><button formaction=javascript:alert(1)>b</button></form>",
            "b",
        ),
        ("<!-- c --><!--><img src=x onerror=alert(1)-->a", "a"),
        ("<!doctype html><?xml x?>a", "a"),
    ] {
        assert_eq!(sanitize(input), want, "{input}");
    }
}

#[test]
fn drops_script_urls() {
    for bad in [
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        " javascript:alert(1)",
        "java\tscript:alert(1)",
        "jav&#x61;script:alert(1)",
        "javascript&#58;alert(1)",
        "javascript&colon;alert(1)",
        "&#106;avascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "vbscript:x",
    ] {
        let out = sanitize(&format!("<a href=\"{bad}\">x</a>"));
        assert_eq!(out, "<a>x</a>", "{bad}");
    }
    for good in [
        "https://e.x/a",
        "http://e.x",
        "mailto:a@e.x",
        "/abs",
        "#frag",
        "rel/a.png",
        "a.html?x=1",
        "//e.x/i.png",
    ] {
        let out = sanitize(&format!("<img src=\"{good}\">"));
        assert_eq!(out, format!("<img src=\"{good}\">"), "{good}");
    }
}

#[test]
fn every_other_angle_bracket_is_text() {
    assert_eq!(sanitize("a < b > c"), "a &lt; b &gt; c");
    assert_eq!(sanitize("<<script>script>alert(1)</script>"), "&lt;");
    // A browser reads the tag name as `scr<script`, too.
    assert_eq!(sanitize("<scr<script>ipt>x</script>"), "ipt&gt;x");
    assert_eq!(sanitize("<img src=x onerror=alert(1)//"), "");
    assert_eq!(sanitize("</>a</ b>c</"), "ac&lt;/");
    assert_eq!(
        sanitize("<div/onmouseover=alert(1)>x</div>"),
        "<div>x</div>"
    );
    assert_eq!(sanitize("<a/href=\"javascript:alert(1)\">x"), "<a>x");
}

#[test]
fn keeps_text_and_entities() {
    assert_eq!(
        sanitize("caf\u{e9} &amp; &lt;b&gt;"),
        "caf\u{e9} &amp; &lt;b&gt;"
    );
}

const ALLOWED: &[&str] = &[
    "a",
    "abbr",
    "b",
    "bdi",
    "bdo",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "mark",
    "ol",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "small",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
];

/// Checks the output's shape: each `<` opens an allowed tag whose attributes are all
/// `name="value"`, with no handler, no style and no script URL.
fn well_formed(out: &str) -> Result<(), String> {
    let mut rest = out;
    while let Some(i) = rest.find('<') {
        let tag = &rest[i + 1..];
        let end = tag.find('>').ok_or("unclosed tag")?;
        let inner = &tag[..end];
        if inner.contains('<') {
            return Err(format!("< inside a tag: {inner}"));
        }
        let inner = inner.strip_prefix('/').unwrap_or(inner);
        let (name, mut attrs) = inner.split_once(' ').unwrap_or((inner, ""));
        if !ALLOWED.contains(&name) {
            return Err(format!("tag {name}"));
        }
        while !attrs.is_empty() {
            let (attr, value) = attrs
                .split_once("=\"")
                .ok_or(format!("attribute in {inner}"))?;
            let close = value.find('"').ok_or("unquoted value")?;
            let value_text = &value[..close];
            if attr.starts_with("on") || attr == "style" {
                return Err(format!("attribute {attr}"));
            }
            let lower = value_text.to_ascii_lowercase();
            let prefixed = ["http://", "https://", "mailto:", "/", "#"]
                .iter()
                .any(|p| lower.starts_with(p));
            let head = lower.split(['/', '?', '#']).next().unwrap_or("");
            if (attr == "href" || attr == "src")
                && !prefixed
                && (lower.contains('&') || head.contains(':'))
            {
                return Err(format!("url {value_text}"));
            }
            attrs = value[close + 1..].trim_start();
        }
        rest = &tag[end + 1..];
    }
    if rest.contains('>') {
        return Err("stray >".to_string());
    }
    Ok(())
}

const PIECES: &[&str] = &[
    "<",
    ">",
    "/",
    "=",
    "\"",
    "'",
    " ",
    "\t",
    "\n",
    "a",
    "img",
    "script",
    "SCRIPT",
    "style",
    "svg",
    "div",
    "br",
    "href",
    "src",
    "onerror",
    "class",
    "javascript:",
    "jav&#x61;script:",
    "&#58;",
    "&colon;",
    "alert(1)",
    "x.png",
    "https://e.x",
    "<!--",
    "-->",
    "<!",
    "?",
    "</",
    "iframe",
    "math",
    "\u{e9}",
    "&",
];

proptest! {
    #[test]
    fn output_holds_only_allowed_markup(parts in prop::collection::vec(prop::sample::select(PIECES), 0..48)) {
        let input = parts.concat();
        let out = sanitize(&input);
        prop_assert!(well_formed(&out).is_ok(), "{:?} gave {:?}: {:?}", input, out, well_formed(&out));
    }
}
