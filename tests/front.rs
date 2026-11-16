use tachylite::front::{Meta, Value, meta, parse, split};

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn list(items: &[&str]) -> Value {
    Value::List(items.iter().map(|s| s.to_string()).collect())
}

fn pairs(yaml: &str) -> Vec<(String, Value)> {
    parse(yaml)
}

fn kv(k: &str, v: Value) -> (String, Value) {
    (k.to_string(), v)
}

#[test]
fn split_takes_the_block_between_fences() {
    assert_eq!(
        split("---\ntitle: A\n---\n# Body\n"),
        (Some("title: A\n"), "# Body\n")
    );
    assert_eq!(split("# No front\n"), (None, "# No front\n"));
    assert_eq!(split("---\n---\nbody"), (Some(""), "body"));
}

#[test]
fn unclosed_fence_is_no_frontmatter() {
    assert_eq!(
        split("---\ntitle: A\nbody\n"),
        (None, "---\ntitle: A\nbody\n")
    );
    assert_eq!(split(" ---\na: b\n---\n"), (None, " ---\na: b\n---\n"));
}

#[test]
fn crlf() {
    let (yaml, body) = split("---\r\ntitle: A\r\ntags:\r\n  - x\r\n---\r\nbody\r\n");
    assert_eq!(body, "body\r\n");
    assert_eq!(
        pairs(yaml.unwrap()),
        vec![kv("title", text("A")), kv("tags", list(&["x"]))]
    );
}

#[test]
fn scalars_plain_and_quoted() {
    assert_eq!(
        pairs("a: plain words\nb: \"double \\\"q\\\" \\\\ x\"\nc: 'it''s'\nd: 42\ne:\n"),
        vec![
            kv("a", text("plain words")),
            kv("b", text("double \"q\" \\ x")),
            kv("c", text("it's")),
            kv("d", text("42")),
            kv("e", text("")),
        ]
    );
}

#[test]
fn flow_and_block_lists() {
    assert_eq!(
        pairs(
            "tags: [a, \"b, c\", 'd']\nempty: []\naliases:\n  - JD\n  - \"J. Doe\"\nflush:\n- a\n-\nnext: x\n"
        ),
        vec![
            kv("tags", list(&["a", "b, c", "d"])),
            kv("empty", list(&[])),
            kv("aliases", list(&["JD", "J. Doe"])),
            kv("flush", list(&["a", ""])),
            kv("next", text("x")),
        ]
    );
}

#[test]
fn unquoted_wikilink_is_text() {
    assert_eq!(
        pairs("up: [[Home]]\nboth: [\"[[a]]\", b]\n"),
        vec![
            kv("up", text("[[Home]]")),
            kv("both", list(&["[[a]]", "b"]))
        ]
    );
}

#[test]
fn block_scalars() {
    assert_eq!(
        pairs("lit: |\n  one\n  two\nfold: >\n  one\n  two\nafter: x\n"),
        vec![
            kv("lit", text("one\ntwo")),
            kv("fold", text("one two")),
            kv("after", text("x")),
        ]
    );
}

#[test]
fn comments_and_colons_in_quotes() {
    assert_eq!(
        pairs("# a comment\nurl: https://example.com/a#b\nq: \"x: y # z\"\nc: value # note\n"),
        vec![
            kv("url", text("https://example.com/a#b")),
            kv("q", text("x: y # z")),
            kv("c", text("value")),
        ]
    );
}

#[test]
fn nested_map_is_raw_text() {
    assert_eq!(
        pairs("place:\n  city: Lisbon\n  zip: 1000\nafter: x\n"),
        vec![
            kv("place", text("city: Lisbon\nzip: 1000")),
            kv("after", text("x")),
        ]
    );
}

#[test]
fn junk_lines_are_skipped() {
    assert_eq!(
        pairs("no colon here\n: no key\nok: yes\n"),
        vec![kv("ok", text("yes"))]
    );
}

#[test]
fn meta_reads_title_and_aliases() {
    assert_eq!(
        meta("---\ntitle: The Title\naliases: [JD, Jane]\n---\nbody"),
        Meta {
            title: Some("The Title".to_string()),
            aliases: vec!["JD".to_string(), "Jane".to_string()],
        }
    );
    assert_eq!(
        meta("---\naliases: JD\ntitle: \"\"\n---\n"),
        Meta {
            title: None,
            aliases: vec!["JD".to_string()],
        }
    );
    assert_eq!(meta("no front"), Meta::default());
}

#[test]
fn a_byte_order_mark_before_the_fence_keeps_the_frontmatter() {
    assert_eq!(
        split("\u{FEFF}---\ntitle: A\n---\nbody"),
        (Some("title: A\n"), "body")
    );
    assert_eq!(
        split("\u{FEFF}# No front\n"),
        (None, "\u{FEFF}# No front\n")
    );
    assert_eq!(
        meta("\u{FEFF}---\ntitle: A\n---\n").title.as_deref(),
        Some("A")
    );
}

#[test]
fn a_flow_list_may_end_in_a_comment() {
    assert_eq!(
        pairs("tags: [a, b] # c\nx: [a]   #c\nodd: [a]b]\nq: [\"x] #y\", z] # c\n"),
        vec![
            kv("tags", list(&["a", "b"])),
            kv("x", list(&["a"])),
            kv("odd", list(&["a]b"])),
            kv("q", list(&["x] #y", "z"])),
        ]
    );
}

#[test]
fn comment_lines_in_a_nested_block_are_dropped() {
    assert_eq!(
        pairs("place:\n  # where\n  city: Lisbon\n  zip: 1000\nl:\n  - a\n  # c\n  - b\n"),
        vec![
            kv("place", text("city: Lisbon\nzip: 1000")),
            kv("l", list(&["a", "b"])),
        ]
    );
    assert_eq!(
        pairs("place:\n  note: |\n    # heading\n    body\n"),
        vec![kv("place", text("note: |\n  # heading\n  body"))]
    );
}

#[test]
fn a_quoted_key_keeps_its_colons() {
    assert_eq!(
        pairs("\"a: b\": c\n'it''s: x': y\n\"q\\\": r\": s\n"),
        vec![
            kv("a: b", text("c")),
            kv("it's: x", text("y")),
            kv("q\": r", text("s")),
        ]
    );
}
