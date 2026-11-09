use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use comrak::adapters::CodefenceRendererAdapter;
use comrak::nodes::Sourcepos;
use comrak::nodes::{AstNode, NodeValue};
use comrak::options::Plugins;
use comrak::{Anchorizer, Arena, Options, format_html_with_plugins, parse_document};

use crate::config::Exclude;
use crate::front::{self, Value};
use crate::links::{Index, Resolved, resolve, split};
use crate::sanitize::sanitize;
use crate::vault::{Kind, classify, is_image};

/// The file stem of the last path segment: `people/Jane Doe.md` gives `Jane Doe`.
pub fn note_title(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

fn options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.extension.ofm_callouts = true;
    options.parse.escaped_char_spans = true;
    options.extension.highlight = true;
    // Raw HTML reaches the page only through `sanitize`, node by node.
    options.render.r#unsafe = true;
    options
}

/// A rendered note page and its version, a hash clients compare to know they are current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub html: String,
    pub version: String,
}

/// A link that resolves to no published file. `matches` is 0 for a missing target, and the
/// number of tied files for an ambiguous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub from: String,
    pub target: String,
    pub matches: usize,
}

/// A note's rendered body and what it links to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body {
    pub html: String,
    /// The published paths its links resolve to.
    pub targets: BTreeSet<Arc<str>>,
    pub broken: Vec<Broken>,
    /// Every note its embeds read, at any depth.
    pub embeds: BTreeSet<Arc<str>>,
}

/// The notes an embed may read, by path.
pub type Sources = HashMap<String, Vec<u8>>;

/// How many levels of notes one page embeds.
pub const EMBED_DEPTH: usize = 3;

/// Percent-encodes all but unreserved characters and `/`.
pub fn encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The URL path of a published file: a note drops `.md`, any other file keeps its name.
pub fn url(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let stem = if lower.ends_with(".md") {
        &path[..path.len() - ".md".len()]
    } else {
        path
    };
    format!("/{}", encode(stem))
}

fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A Markdown link into the vault: no scheme, and not from the site root or within the page.
fn local(url: &str) -> bool {
    let scheme = url.find(':').is_some_and(|i| {
        url[..i]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
    });
    !url.is_empty() && !scheme && !url.starts_with('/') && !url.starts_with('#')
}

fn anchor(heading: Option<&str>) -> String {
    match heading.map(str::trim) {
        Some(h) if !h.is_empty() => format!("#{}", encode(&Anchorizer::new().anchorize(h))),
        _ => String::new(),
    }
}

/// The opening `<a>` of a link to `path#heading` from `from`. Records its target, or logs it
/// as broken.
fn open(body: &mut Body, from: &str, index: &Index, path: &str, heading: Option<&str>) -> String {
    let fragment = anchor(heading);
    if path.is_empty() {
        return format!("<a href=\"{fragment}\">");
    }
    match resolve(index, from, path) {
        Resolved::Found(target) => {
            let tag = format!("<a href=\"{}{fragment}\">", url(&target));
            body.targets.insert(target);
            tag
        }
        unresolved => {
            body.broken.push(broken(from, path, &unresolved));
            "<a class=\"broken-link\">".to_string()
        }
    }
}

fn broken(from: &str, target: &str, resolved: &Resolved) -> Broken {
    Broken {
        from: from.to_string(),
        target: target.to_string(),
        matches: match resolved {
            Resolved::Ambiguous(n) => *n,
            _ => 0,
        },
    }
}

enum Embed {
    /// Stays in its paragraph.
    Inline(String),
    /// Takes its paragraph's place when it is the paragraph's only content.
    Block(String),
    /// A plain link: this opening tag around the label.
    Link(String),
}

/// `![[raw]]` from `from`, with `label` as its text. `chain` holds the notes being
/// embedded, outermost first.
fn embed(
    body: &mut Body,
    (from, index, sources): (&str, &Index, &Sources),
    chain: &mut Vec<String>,
    raw: &str,
    label: &str,
) -> Embed {
    let (path, heading) = split(raw);
    let path = path.trim();
    let link = |body: &mut Body| Embed::Link(open(body, from, index, path, heading));
    let Resolved::Found(target) = resolve(index, from, path) else {
        return link(body);
    };
    if heading.is_some() {
        return link(body);
    }
    let src = url(&target);
    let kind = classify(&target, &Exclude::default());
    let html = match kind {
        Some(Kind::Asset) if is_image(&target) => {
            let sized = label
                .split('x')
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            let alt = if sized {
                path.rsplit('/').next().unwrap_or(path)
            } else {
                label
            };
            Embed::Inline(format!(
                "<img src=\"{src}\" alt=\"{}\" loading=\"lazy\">",
                escape(alt)
            ))
        }
        Some(Kind::Html) => Embed::Block(format!(
            "<iframe class=\"embed-html\" sandbox=\"allow-scripts\" src=\"{src}\" \
             loading=\"lazy\"></iframe>\n"
        )),
        Some(Kind::Note) if chain.len() <= EMBED_DEPTH && !chain.iter().any(|c| **c == *target) => {
            let Some(bytes) = sources.get(&*target) else {
                return link(body);
            };
            let markdown = String::from_utf8_lossy(bytes);
            chain.push(target.to_string());
            let inner = render(&target, &markdown, index, sources, chain);
            chain.pop();
            let title = front::meta(&markdown)
                .title
                .unwrap_or_else(|| note_title(&target).to_string());
            body.embeds.insert(Arc::clone(&target));
            body.embeds.extend(inner.embeds);
            Embed::Block(format!(
                "<div class=\"embed\"><a class=\"embed-link\" href=\"{src}\">{}</a>\n{}</div>\n",
                escape(&title),
                inner.html
            ))
        }
        _ => return link(body),
    };
    body.targets.insert(target);
    html
}

/// The notes `markdown`'s embeds may read, without parsing it. It over-matches, in code
/// and comments, so it loads a superset of what the render reads.
pub fn embed_targets(from: &str, markdown: &str, index: &Index) -> Vec<Arc<str>> {
    let mut targets: Vec<Arc<str>> = Vec::new();
    let mut rest = markdown;
    while let Some(i) = rest.find("![[") {
        rest = &rest[i + 3..];
        let Some(end) = rest.find("]]") else {
            break;
        };
        let inner = &rest[..end];
        rest = &rest[end + 2..];
        let path = inner.split('|').next().unwrap_or(inner);
        if path.contains('#') {
            continue;
        }
        if let Resolved::Found(target) = resolve(index, from, path.trim())
            && classify(&target, &Exclude::default()) == Some(Kind::Note)
            && !targets.contains(&target)
        {
            targets.push(target);
        }
    }
    targets
}

/// A tag link. Tag pages are keyed by the lowercased tag.
fn tag_link(tag: &str) -> String {
    format!(
        "<a class=\"tag\" href=\"/.tachylite/tags/{}\">#{}</a>",
        encode(&tag.to_lowercase()),
        escape(tag)
    )
}

/// One property value: a tag, a lone `[[link]]`, or text.
fn property(body: &mut Body, from: &str, index: &Index, key: &str, value: &str) -> String {
    if key == "tags" {
        let tag = value.trim().trim_start_matches('#');
        if !tag.is_empty() {
            return tag_link(tag);
        }
    }
    if front::one_link(value) {
        let inner = &value[2..value.len() - 2];
        let (target, label) = inner.split_once('|').unwrap_or((inner, inner));
        let (path, heading) = split(target);
        let open = open(body, from, index, path.trim(), heading);
        return format!("{open}{}</a>", escape(label));
    }
    escape(value)
}

/// The frontmatter as a `<dl>`, every key in file order.
fn properties(body: &mut Body, from: &str, yaml: &str, index: &Index) -> String {
    let pairs = front::parse(yaml);
    if pairs.is_empty() {
        return String::new();
    }
    let mut html = String::from("<dl class=\"properties\">");
    for (key, value) in pairs {
        html.push_str(&format!("<dt>{}</dt><dd>", escape(&key)));
        match value {
            Value::Text(text) => html.push_str(&property(body, from, index, &key, &text)),
            Value::List(items) => {
                let items: Vec<String> = items
                    .iter()
                    .map(|item| property(body, from, index, &key, item))
                    .collect();
                html.push_str(&items.join(", "));
            }
        }
        html.push_str("</dd>");
    }
    html.push_str("</dl>\n");
    html
}

/// Renders a note's Markdown and resolves its links against the published paths.
/// Renders a note's body. `sources` holds the notes its embeds may read; an embed of a note
/// missing from it renders as a link.
pub fn render_body(from: &str, markdown: &str, index: &Index, sources: &Sources) -> Body {
    render(from, markdown, index, sources, &mut vec![from.to_string()])
}

fn render(
    from: &str,
    markdown: &str,
    index: &Index,
    sources: &Sources,
    chain: &mut Vec<String>,
) -> Body {
    let (yaml, markdown) = front::split(markdown);
    let markdown = strip_comments(markdown);
    let mut body = Body::default();
    // An embedded note shows its body only.
    let props = match yaml {
        Some(yaml) if chain.len() == 1 => properties(&mut body, from, yaml, index),
        _ => String::new(),
    };
    let arena = Arena::new();
    let options = options();
    let root = parse_document(&arena, &markdown, &options);
    // The stripper and the parser disagree on what is code. Withhold the text rather than
    // risk a comment.
    if markdown.contains("%%") && unstripped(root) {
        body.html = "<p class=\"comment-error\">This note has a comment Tachylite could not \
                     place, so its text is withheld.</p>\n"
            .to_string();
        body.targets.clear();
        body.broken.clear();
        return body;
    }
    let mut rewrites = Vec::new();
    let mut texts = Vec::new();
    let mut anchorizer = Anchorizer::new();
    let mut headings = Vec::new();
    let mut embeds = Vec::new();
    let mut images = Vec::new();
    let mut raw = Vec::new();
    for node in root.descendants() {
        match &node.data().value {
            NodeValue::Heading(heading) => {
                let id = escape(&anchorizer.anchorize(&node.collect_text()));
                headings.push((node, heading.level, id));
                continue;
            }
            NodeValue::HtmlInline(_) | NodeValue::HtmlBlock(_) => {
                raw.push(node);
                continue;
            }
            NodeValue::WikiLink(link) if link.embed => {
                embeds.push((node, link.url.clone()));
                continue;
            }
            NodeValue::Image(link) if local(&link.url) => {
                images.push((node, link.url.clone()));
                continue;
            }
            _ => {}
        }
        if let NodeValue::Text(text) = &node.data().value {
            if !text.contains('#') {
                continue;
            }
            let linked = node.ancestors().any(|a| {
                matches!(
                    a.data().value,
                    NodeValue::Link(_)
                        | NodeValue::WikiLink(_)
                        | NodeValue::Image(_)
                        | NodeValue::Escaped
                )
            });
            if !linked {
                texts.push(node);
            }
            continue;
        }
        let link = match &node.data().value {
            NodeValue::WikiLink(link) => {
                let (path, heading) = split(&link.url);
                Some((path.trim().to_string(), heading.map(str::to_string)))
            }
            NodeValue::Link(link) if local(&link.url) => {
                let (path, heading) = split(&link.url);
                decode(path).map(|path| (path, heading.and_then(decode)))
            }
            _ => None,
        };
        let Some((path, heading)) = link else {
            continue;
        };
        let tag = open(&mut body, from, index, &path, heading.as_deref());
        rewrites.push((node, tag));
    }
    for node in raw {
        match &mut node.data_mut().value {
            NodeValue::HtmlInline(html) => *html = sanitize(html),
            NodeValue::HtmlBlock(block) => block.literal = sanitize(&block.literal),
            _ => {}
        }
    }
    for (node, open) in rewrites {
        wrap(&arena, node, open, "</a>".to_string());
    }
    for (node, src) in images {
        let Some(path) = decode(split(&src).0) else {
            continue;
        };
        match resolve(index, from, &path) {
            Resolved::Found(target) => {
                if let NodeValue::Image(link) = &mut node.data_mut().value {
                    link.url = url(&target);
                }
                body.targets.insert(target);
            }
            unresolved => body.broken.push(broken(from, &path, &unresolved)),
        }
    }
    for (node, raw) in embeds {
        let label = node.collect_text();
        match embed(&mut body, (from, index, sources), chain, &raw, &label) {
            Embed::Inline(html) => replace(&arena, node, html),
            Embed::Block(html) => {
                let parent = node.parent().filter(|p| {
                    matches!(p.data().value, NodeValue::Paragraph)
                        && p.first_child().is_some_and(|c| c.same_node(node))
                        && p.last_child().is_some_and(|c| c.same_node(node))
                });
                replace(&arena, parent.unwrap_or(node), html);
            }
            Embed::Link(open) => wrap(&arena, node, open, "</a>".to_string()),
        }
    }
    for (node, level, id) in headings {
        wrap(
            &arena,
            node,
            format!("<h{level} id=\"{id}\">"),
            format!("</h{level}>\n"),
        );
    }
    for node in texts {
        link_tags(&arena, node);
    }
    let mut html = props;
    let mut plugins = Plugins::default();
    plugins
        .render
        .codefence_renderers
        .insert("mermaid".to_string(), &Mermaid);
    // Writing into a String cannot fail.
    let _ = format_html_with_plugins(root, &options, &mut html, &plugins);
    body.html = html;
    body
}

/// The byte ranges of the `#tags` in `text`, `#` included. A tag starts the text or follows
/// whitespace, and holds letters, digits, `_`, `-` and `/`, with at least one non-digit.
fn tags(text: &str, at_start: bool) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut prev: Option<char> = None;
    let mut skip_to = 0;
    for (i, c) in text.char_indices() {
        let boundary = prev.map_or(at_start, char::is_whitespace);
        prev = Some(c);
        if c != '#' || i < skip_to || !boundary {
            continue;
        }
        let rest = &text[i + 1..];
        let len = rest
            .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
            .unwrap_or(rest.len());
        let tag = rest[..len].trim_end_matches('/');
        if tag.chars().any(|c| !c.is_ascii_digit()) {
            found.push((i, i + 1 + tag.len()));
            skip_to = i + 1 + tag.len();
        }
    }
    found
}

/// Splits a text node around its tags, which become tag links.
fn link_tags<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>) {
    let at_start = node
        .previous_sibling()
        .is_none_or(|p| matches!(p.data().value, NodeValue::SoftBreak | NodeValue::LineBreak));
    let text = match &node.data().value {
        NodeValue::Text(text) => text.to_string(),
        _ => return,
    };
    let found = tags(&text, at_start);
    if found.is_empty() {
        return;
    }
    let alloc = |value: NodeValue| arena.alloc(AstNode::from(value));
    let mut last = 0;
    for (start, end) in found {
        if start > last {
            node.insert_before(alloc(NodeValue::Text(Cow::Owned(text[last..start].into()))));
        }
        node.insert_before(alloc(NodeValue::Raw(tag_link(&text[start + 1..end]))));
        last = end;
    }
    if last < text.len() {
        node.insert_before(alloc(NodeValue::Text(Cow::Owned(text[last..].into()))));
    }
    node.detach();
}

/// True when parsed text outside code still holds `%%`.
fn unstripped<'a>(root: &'a AstNode<'a>) -> bool {
    root.descendants()
        .any(|n| matches!(&n.data().value, NodeValue::Text(t) if t.contains("%%")))
}

/// The fence a line opens, after container markers: its character and length.
fn fence_open(line: &str) -> Option<(u8, usize)> {
    let rest = strip_containers(line);
    let c = *rest.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = rest.bytes().take_while(|&b| b == c).count();
    let info = &rest[n..];
    (n >= 3 && !(c == b'`' && info.contains('`'))).then_some((c, n))
}

fn fence_close(line: &str, (c, n): (u8, usize)) -> bool {
    let rest = strip_containers(line);
    let run = rest.bytes().take_while(|&b| b == c).count();
    run >= n && rest[run..].trim().is_empty()
}

/// A line without its leading whitespace, quote markers and list markers.
fn strip_containers(mut line: &str) -> &str {
    loop {
        let trimmed = line.trim_start();
        let marker = trimmed.strip_prefix('>').or_else(|| {
            let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
            let rest = &trimmed[digits..];
            let bullet = if digits > 0 {
                rest.strip_prefix(['.', ')'])
            } else {
                rest.strip_prefix(['-', '*', '+'])
            };
            bullet.filter(|r| r.starts_with([' ', '\t']))
        });
        match marker {
            Some(rest) => line = rest,
            None => return trimmed,
        }
    }
}

/// Removes `%%comments%%` from a note's Markdown, in note order, before it is parsed. A
/// comment runs to the next `%%`, or to the end of the note. Fenced code and code spans keep
/// their `%%`, unless a comment holds them. Newlines in a comment stay, so lines keep their
/// places.
pub fn strip_comments(md: &str) -> Cow<'_, str> {
    if !md.contains("%%") {
        return Cow::Borrowed(md);
    }
    let mut out = String::with_capacity(md.len());
    let mut comment = false;
    let mut fence = None;
    for line in md.split_inclusive('\n') {
        if !comment && let Some(open) = fence {
            if fence_close(line, open) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if !comment && let Some(open) = fence_open(line) {
            fence = Some(open);
            out.push_str(line);
            continue;
        }
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut kept = 0;
        while i < bytes.len() {
            if comment {
                match line[i..].find("%%") {
                    Some(at) => {
                        comment = false;
                        i += at + 2;
                        kept = i;
                    }
                    None => {
                        i = bytes.len();
                        kept = i;
                        if line.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                }
                continue;
            }
            match bytes[i] {
                b'\\' => i += 2,
                b'`' => {
                    let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                    let close = "`".repeat(n);
                    let after = i + n;
                    // A span closes at the next run of exactly `n` backticks.
                    let end = line[after..].match_indices(&close).find(|(at, _)| {
                        let at = after + at;
                        bytes.get(at + n) != Some(&b'`') && bytes[at - 1] != b'`'
                    });
                    i = end.map_or(after, |(at, _)| after + at + n);
                }
                b'%' if bytes.get(i + 1) == Some(&b'%') => {
                    out.push_str(&line[kept..i]);
                    comment = true;
                    i += 2;
                    kept = i;
                }
                _ => i += 1,
            }
        }
        if !comment {
            out.push_str(&line[kept.min(line.len())..]);
        }
    }
    Cow::Owned(out)
}

/// A Mermaid fence becomes `<pre class="mermaid">`, which the client draws.
struct Mermaid;

impl CodefenceRendererAdapter for Mermaid {
    fn write(
        &self,
        output: &mut dyn std::fmt::Write,
        _lang: &str,
        _meta: &str,
        code: &str,
        _sourcepos: Option<Sourcepos>,
    ) -> std::fmt::Result {
        writeln!(output, "<pre class=\"mermaid\">{}</pre>", escape(code))
    }
}

/// Replaces a node with raw tags around its children. comrak's own link markup would
/// leave `#` and `?` in a file name unescaped, and its heading ids add an anchor link.
fn wrap<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>, open: String, close: String) {
    let raw = |html: String| arena.alloc(AstNode::from(NodeValue::Raw(html)));
    node.insert_before(raw(open));
    while let Some(child) = node.first_child() {
        node.insert_before(child);
    }
    node.insert_before(raw(close));
    node.detach();
}

fn replace<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>, html: String) {
    node.insert_before(arena.alloc(AstNode::from(NodeValue::Raw(html))));
    node.detach();
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    // Writing into a String cannot fail.
    let _ = comrak::html::escape(&mut escaped, text);
    escaped
}

/// Wraps a body in the page shell, with its backlinks as `(title, url)` pairs.
pub fn render_page(title: &str, body: &str, backlinks: &[(String, String)]) -> Page {
    let title = escape(title);
    let mut main = body.to_string();
    if !backlinks.is_empty() {
        main.push_str("<section class=\"backlinks\"><h2>Backlinks</h2><ul>");
        for (text, href) in backlinks {
            main.push_str(&format!("<li><a href=\"{href}\">{}</a></li>", escape(text)));
        }
        main.push_str("</ul></section>\n");
    }
    let mut hasher = DefaultHasher::new();
    (&title, &main).hash(&mut hasher);
    let version = format!("{:016x}", hasher.finish());
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"tachylite-version\" content=\"{version}\">\n\
         <title>{title}</title>\n\
         <script src=\"/.tachylite/live.js\" defer></script>\n</head>\n\
         <body>\n<main>\n{main}</main>\n</body>\n</html>\n"
    );
    Page { html, version }
}
