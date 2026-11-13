use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use comrak::adapters::CodefenceRendererAdapter;
use comrak::html::dangerous_url;
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
    /// The notes it would have transcluded, had `Sources` held them.
    pub missing: BTreeSet<Arc<str>>,
    /// The keys of the tags it links, from its own text and frontmatter, not its embeds.
    pub tags: BTreeSet<String>,
}

/// The notes an embed may read, by path.
pub type Sources = HashMap<String, Vec<u8>>;

/// How many levels of notes one page embeds.
pub const EMBED_DEPTH: usize = 3;

/// Percent-encodes all but unreserved characters and `/`.
pub fn encode(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(path.len() + 8);
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
    out
}

/// The URL path of a published file: a note drops `.md`, any other file keeps its name.
pub fn url(path: &str) -> String {
    let n = path.len().saturating_sub(".md".len());
    let stem = if path.as_bytes()[n..].eq_ignore_ascii_case(b".md") {
        &path[..n]
    } else {
        path
    };
    let mut out = String::with_capacity(path.len() + 9);
    out.push('/');
    out.push_str(&encode(stem));
    out
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

/// `![[raw]]` from `from`, at `depth` levels of embedding, with `label` as its text.
/// `seen` holds the notes already on the page; each is transcluded once.
fn embed(
    body: &mut Body,
    (from, index, sources): (&str, &Index, &Sources),
    (depth, seen): (usize, &mut HashSet<String>),
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
        Some(Kind::Note) if depth < EMBED_DEPTH && !seen.contains(&*target) => {
            let Some(bytes) = sources.get(&*target) else {
                body.missing.insert(target);
                return link(body);
            };
            let markdown = String::from_utf8_lossy(bytes);
            seen.insert(target.to_string());
            // `seen` grows by one per embed, so its size numbers the embeds on the page.
            let prefix = format!("e{}.", seen.len() - 1);
            let inner = render(&target, &markdown, index, sources, (depth + 1, seen));
            body.missing.extend(inner.missing);
            let title = front::meta(&markdown)
                .title
                .unwrap_or_else(|| note_title(&target).to_string());
            body.embeds.insert(Arc::clone(&target));
            body.embeds.extend(inner.embeds);
            Embed::Block(format!(
                "<div class=\"embed\"><a class=\"embed-link\" href=\"{src}\">{}</a>\n{}</div>\n",
                escape(&title),
                scope_ids(&inner.html, &prefix)
            ))
        }
        _ => return link(body),
    };
    body.targets.insert(target);
    html
}

/// An embedded body with `prefix` on every id and on every link within the page, so its
/// heading and footnote ids cannot collide with the host's. Heading slugs never hold a `.`.
/// Text and code escape their quotes, so only a tag holds `id="` or `href="#`.
fn scope_ids(html: &str, prefix: &str) -> String {
    html.replace(" id=\"", &format!(" id=\"{prefix}"))
        .replace(" href=\"#", &format!(" href=\"#{prefix}"))
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

/// The longest file name the output filesystem takes, in bytes.
pub const NAME_MAX: usize = 255;

/// A tag's page key: the tag lowercased, as Obsidian matches tags in any case. A tag holds
/// letters, digits, `_`, `-` and `/`, with at least one non-digit and no empty level. A tag
/// longer than a page name may be is no tag, so no link points at a page that cannot be
/// written, and no nesting runs a page's path past the filesystem's limit.
pub fn tag_key(tag: &str) -> Option<String> {
    let chars = tag
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'));
    if !chars || tag.chars().all(|c| c.is_ascii_digit() || c == '/') {
        return None;
    }
    let key = tag.to_lowercase();
    let fits = key.len() + ".html".len() <= NAME_MAX;
    (fits && key.split('/').all(|level| !level.is_empty())).then_some(key)
}

/// A tag link to its tag page, or the text alone for no tag.
fn tag_link(tags: &mut BTreeSet<String>, tag: &str) -> String {
    let Some(key) = tag_key(tag) else {
        return escape(tag);
    };
    let html = format!(
        "<a class=\"tag\" href=\"/.tachylite/tags/{}\">#{}</a>",
        encode(&key),
        escape(tag)
    );
    tags.insert(key);
    html
}

/// One property value: tags, a lone `[[link]]`, or text. `tags: a, b` holds two tags.
fn property(body: &mut Body, from: &str, index: &Index, key: &str, value: &str) -> String {
    if key == "tags" {
        let tags: Vec<String> = value
            .split(',')
            .map(|t| t.trim().trim_start_matches('#'))
            .filter(|t| !t.is_empty())
            .map(|t| tag_link(&mut body.tags, t))
            .collect();
        if !tags.is_empty() {
            return tags.join(", ");
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
                    .filter(|item| !item.trim().is_empty())
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

/// Renders a note's body and resolves its links against the published paths. `sources`
/// holds the notes its embeds may read; an embed of a note missing from it renders as a
/// link.
pub fn render_body(from: &str, markdown: &str, index: &Index, sources: &Sources) -> Body {
    let mut seen = HashSet::from([from.to_string()]);
    render(from, markdown, index, sources, (0, &mut seen))
}

fn render(
    from: &str,
    markdown: &str,
    index: &Index,
    sources: &Sources,
    (depth, seen): (usize, &mut HashSet<String>),
) -> Body {
    let (yaml, raw) = front::split(markdown);
    let Stripped {
        text: markdown,
        unclosed,
        openers,
    } = strip_comments(raw);
    let mut body = Body::default();
    // An embedded note shows its body only.
    let props = match yaml {
        Some(yaml) if depth == 0 => properties(&mut body, from, yaml, index),
        _ => String::new(),
    };
    let arena = Arena::new();
    let options = options();
    // Before the main parse, so the two trees are never in memory at once.
    let shifted = opened_in_code(raw, &openers);
    let root = parse_document(&arena, &markdown, &options);
    // The stripper and the parser disagree on what is code. Withhold the text rather than
    // risk a comment.
    if shifted || (markdown.contains("%%") && unstripped(root)) {
        body.html = "<p class=\"comment-error\">This note has a comment Tachylite could not \
                     place, so its text is withheld.</p>\n"
            .to_string();
        body.targets.clear();
        body.broken.clear();
        body.tags.clear();
        return body;
    }
    let mut rewrites = Vec::new();
    let mut texts = Vec::new();
    let mut anchorizer = Anchorizer::new();
    let mut headings = Vec::new();
    let mut embeds = Vec::new();
    let mut images = Vec::new();
    let mut scripted = Vec::new();
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
            // Raw HTML on also turns off comrak's own check of link URLs.
            NodeValue::Link(link) if dangerous_url(&link.url) => {
                rewrites.push((node, "<a>".to_string()));
                continue;
            }
            NodeValue::Image(link) if dangerous_url(&link.url) => {
                scripted.push(node);
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
            NodeValue::Link(link) if link.url.starts_with('#') && link.url.len() > 1 => {
                decode(&link.url[1..]).map(|heading| (String::new(), Some(heading)))
            }
            _ => None,
        };
        let Some((path, heading)) = link else {
            continue;
        };
        let tag = open(&mut body, from, index, &path, heading.as_deref());
        rewrites.push((node, tag));
    }
    for node in scripted {
        if let NodeValue::Image(link) = &mut node.data_mut().value {
            link.url.clear();
        }
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
        match embed(
            &mut body,
            (from, index, sources),
            (depth, &mut *seen),
            &raw,
            &label,
        ) {
            Embed::Inline(html) => replace(&arena, node, html),
            Embed::Block(html) => {
                match node
                    .parent()
                    .filter(|p| matches!(p.data().value, NodeValue::Paragraph))
                {
                    Some(paragraph) => split_paragraph(&arena, paragraph, node, html),
                    None => replace(&arena, node, html),
                }
            }
            Embed::Link(open) => wrap(&arena, node, open, "</a>".to_string()),
        }
    }
    // Before the heading wrap, which gives a heading's first text a sibling.
    for node in texts {
        link_tags(&arena, node, &mut body.tags);
    }
    for (node, level, id) in headings {
        wrap(
            &arena,
            node,
            format!("<h{level} id=\"{id}\">"),
            format!("</h{level}>\n"),
        );
    }
    let mut html = props;
    let mut plugins = Plugins::default();
    plugins
        .render
        .codefence_renderers
        .insert("mermaid".to_string(), &Mermaid);
    // Writing into a String cannot fail.
    let _ = format_html_with_plugins(root, &options, &mut html, &plugins);
    if unclosed {
        html.push_str(
            "<p class=\"comment-error\">A comment in this note is not closed, so the text \
             after it is hidden.</p>\n",
        );
    }
    body.html = html;
    body
}

/// The byte ranges of the `#tags` in `text`, `#` included. A tag starts the text or follows
/// whitespace, and `tag_key` takes it.
fn tags(text: &str, at_start: bool) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        let boundary = prev.map_or(at_start, char::is_whitespace);
        prev = Some(c);
        if c != '#' || !boundary {
            continue;
        }
        let rest = &text[i + 1..];
        let len = rest
            .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
            .unwrap_or(rest.len());
        let tag = rest[..len].trim_end_matches('/');
        if tag_key(tag).is_some() {
            found.push((i, i + 1 + tag.len()));
        }
    }
    found
}

/// Splits a text node around its tags, which become tag links.
fn link_tags<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>, tags_found: &mut BTreeSet<String>) {
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
        let link = tag_link(tags_found, &text[start + 1..end]);
        node.insert_before(alloc(NodeValue::Raw(link)));
        last = end;
    }
    if last < text.len() {
        node.insert_before(alloc(NodeValue::Text(Cow::Owned(text[last..].into()))));
    }
    node.detach();
}

/// True when any node outside code still holds `%%`: text, raw HTML, a link's URL or title,
/// a footnote name, a fence's info string or a callout's type.
fn unstripped<'a>(root: &'a AstNode<'a>) -> bool {
    let has = |s: &str| s.contains("%%");
    root.descendants().any(|n| match &n.data().value {
        NodeValue::Text(t) => has(t),
        // `\\%%` parses as an escaped `%` before a text that starts with `%`.
        NodeValue::Escaped => {
            n.collect_text() == "%"
                && n.next_sibling().is_some_and(
                    |s| matches!(&s.data().value, NodeValue::Text(t) if t.starts_with('%')),
                )
        }
        NodeValue::HtmlInline(s) | NodeValue::Raw(s) => has(s),
        NodeValue::HtmlBlock(b) => has(&b.literal),
        NodeValue::Link(l) | NodeValue::Image(l) => has(&l.url) || has(&l.title),
        NodeValue::WikiLink(l) => has(&l.url),
        NodeValue::FootnoteDefinition(f) => has(&f.name),
        NodeValue::FootnoteReference(f) => has(&f.name),
        NodeValue::CodeBlock(c) => has(&c.info),
        NodeValue::Callout(c) => has(&c.callout_type) || c.metadata.as_deref().is_some_and(has),
        _ => false,
    })
}

/// True when the parser reads any of `openers`, the byte offsets where the stripper opened
/// a comment, as code. The stripper then took code for text, and its `%%` pairs may have
/// shifted, which would publish a comment's text with no `%%` left to find.
fn opened_in_code(md: &str, openers: &[usize]) -> bool {
    !openers.is_empty() && !plain(md, openers) && parsed_in_code(md, openers)
}

/// True when the stripper reads code exactly as the parser does up to the last opener, so
/// no opener can be in code: the only code is plain fences, and no comment holds a fence
/// character, since a fence line in a comment is no fence to the stripper.
fn plain(md: &str, openers: &[usize]) -> bool {
    let Some(&last) = openers.last() else {
        return true;
    };
    let fence_in_comment = openers.iter().any(|&at| {
        let body = &md[at + 2..];
        body[..body.find("%%").unwrap_or(body.len())].contains(['`', '~'])
    });
    // The whole line, so a fence's info string is read in full.
    let end = md[last..].find('\n').map_or(md.len(), |at| last + at + 1);
    !fence_in_comment && plain_fences(&md[..end])
}

/// The parser's answer for `opened_in_code`. Each opener becomes a mark the parser treats
/// as plain text, as it does `%%`, so code ends where it did. comrak's inline source
/// positions drift after a link title that spans lines, so the mark, not a position, says
/// where each opener landed.
fn parsed_in_code(md: &str, openers: &[usize]) -> bool {
    const MARK: &str = "\u{E000}\u{E000}";
    let mut marked = String::with_capacity(md.len() + openers.len() * 4);
    let mut last = 0;
    for &at in openers {
        marked.push_str(&md[last..at]);
        marked.push_str(MARK);
        last = at + 2;
    }
    marked.push_str(&md[last..]);
    let arena = Arena::new();
    let root = parse_document(&arena, &marked, &options());
    root.descendants().any(|n| match &n.data().value {
        NodeValue::Code(code) => code.literal.contains(MARK),
        NodeValue::CodeBlock(block) => block.literal.contains(MARK) || block.info.contains(MARK),
        _ => false,
    })
}

/// True when the only code in `md` is fences at column 0, which the stripper reads exactly
/// as the parser does: no other backtick or tilde, no tab, no four-space run, no lone
/// carriage return, and no HTML block that could hold a fence line. An HTML block that opens with `<!`, `<?`, `<pre`,
/// `<script`, `<style` or `<textarea` may run past a blank line, so it rules the note out.
/// Any other ends at a blank line, so it holds no fence line when its lines hold no
/// backtick or tilde.
fn plain_fences(md: &str) -> bool {
    const RAW: [&str; 6] = ["<!", "<?", "<pre", "<script", "<style", "<textarea"];
    // A lone `\r` ends a line for the parser but not for this scan.
    if md
        .match_indices('\r')
        .any(|(at, _)| md.as_bytes().get(at + 1) != Some(&b'\n'))
    {
        return false;
    }
    let mut fence = None;
    let mut html = false;
    for line in md.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let run = |c: u8| bytes.iter().take_while(|&&b| b == c).count();
        if let Some((c, n)) = fence {
            let lead = run(b' ');
            let fence_run = bytes[lead..].iter().take_while(|&&b| b == c).count();
            let rest = &line[lead + fence_run..];
            // The stripper's test, and the parser's: only spaces and tabs may follow.
            let loose = fence_run >= n && rest.trim().is_empty();
            let strict = fence_run >= n && rest.bytes().all(|b| b" \t\r\n".contains(&b));
            if loose || strict {
                // The parser closes a fence indented up to 3 spaces; the stripper does not.
                if lead > 0 || loose != strict {
                    return false;
                }
                fence = None;
            }
            continue;
        }
        if let Some(&c @ (b'`' | b'~')) = bytes.first()
            && !html
            && run(c) >= 3
            && !(c == b'`' && line[run(c)..].contains('`'))
        {
            fence = Some((c, run(c)));
            continue;
        }
        if line.contains(['`', '~', '\t']) || line.contains("    ") {
            return false;
        }
        let rest = line.trim_start_matches(' ');
        if rest.starts_with('<') {
            let start = rest.get(..9).unwrap_or(rest).to_ascii_lowercase();
            if RAW.iter().any(|raw| start.starts_with(raw)) {
                return false;
            }
            html = true;
        }
        // Only spaces make a blank line, and a blank line ends the HTML block.
        if bytes.iter().all(|b| matches!(b, b' ' | b'\r' | b'\n')) {
            html = false;
        }
    }
    true
}

/// The column after `prefix`, with tab stops every 4 columns.
fn columns(prefix: &str) -> usize {
    prefix.bytes().fold(0, |col, b| match b {
        b'\t' => col + 4 - col % 4,
        _ => col + 1,
    })
}

/// A line's quote depth, the column its quote content starts at, and the byte offset and
/// column of its first character after the quote markers and indentation.
fn quotes(line: &str) -> (usize, usize, usize, usize) {
    let bytes = line.as_bytes();
    let (mut depth, mut base, mut i) = (0, 0, 0);
    loop {
        while matches!(bytes.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        // Four columns of indent make code, not a quote marker.
        if bytes.get(i) != Some(&b'>') || columns(&line[..i]) - base > 3 {
            return (depth, base, i, columns(&line[..i]));
        }
        depth += 1;
        i += 1;
        if bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        base = columns(&line[..i]);
    }
}

/// The column where a list item's content starts, when `line[at..]` opens one, and whether
/// it holds text. A list marker followed by 5 or more spaces holds indented code; its content
/// column is then one past the marker.
fn list_item(line: &str, at: usize) -> Option<(usize, bool, bool)> {
    let rest = &line[at..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let marker = if digits > 0 {
        (digits <= 9 && rest[digits..].starts_with(['.', ')'])).then_some(digits + 1)?
    } else {
        rest.starts_with(['-', '*', '+']).then_some(1)?
    };
    let after = &rest[marker..];
    if !(after.is_empty() || after.starts_with([' ', '\t', '\n', '\r'])) {
        return None;
    }
    let end = at + marker;
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    let text = !after.trim().is_empty();
    let pad = columns(&line[..end + spaces]) - columns(&line[..end]);
    let content = if (1..=4).contains(&pad) && text {
        columns(&line[..end + spaces])
    } else {
        columns(&line[..end]) + 1
    };
    let one = digits == 0 || rest[..digits].trim_start_matches('0') == "1";
    Some((content, text, one))
}

/// `---`, `***` or `___`, three or more, spaces allowed between.
fn thematic(rest: &str) -> bool {
    let rest = rest.trim();
    let Some(c) = rest.chars().next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    rest.chars().all(|x| x == c || x == ' ' || x == '\t') && rest.matches(c).count() >= 3
}

/// True when a line of output continues a paragraph, so that an indented line after it is
/// text, not code. It errs to false, which reads more as code.
fn paragraph_line(line: &str) -> bool {
    let rest = strip_containers(line).trim_end();
    !rest.is_empty()
        && !atx(rest)
        && !rest.starts_with('<')
        && !rest.contains('|')
        && !thematic(rest)
        && !rest
            .bytes()
            .all(|b| matches!(b, b'=' | b'-' | b' ' | b'\t'))
}

/// A fence: its character, its length and the column it opens at.
#[derive(Clone, Copy)]
struct Fence(u8, usize, usize);

/// The fence a line opens, after container markers.
fn fence_open(line: &str) -> Option<Fence> {
    let rest = strip_containers(line);
    let c = *rest.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = rest.bytes().take_while(|&b| b == c).count();
    let info = &rest[n..];
    let col = columns(&line[..line.len() - rest.len()]);
    (n >= 3 && !(c == b'`' && info.contains('`'))).then_some(Fence(c, n, col))
}

/// A closing fence is no more indented than its opening one. A deeper run of the fence
/// character is the fence's content, as CommonMark allows a closer at most 3 columns in.
fn fence_close(line: &str, Fence(c, n, col): Fence) -> bool {
    let rest = strip_containers(line);
    let run = rest.bytes().take_while(|&b| b == c).count();
    run >= n && rest[run..].trim().is_empty() && columns(&line[..line.len() - rest.len()]) <= col
}

/// A line without its leading whitespace, quote markers and list markers.
fn strip_containers(mut line: &str) -> &str {
    loop {
        let trimmed = line.trim_start_matches([' ', '\t']);
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

/// Where a run of `n` backticks from `from` closes a code span: the next run of exactly `n`
/// on this line, or on a later line of the same paragraph. A blank line, a fence or a heading
/// ends a paragraph, and a heading is one line.
fn span_close(md: &str, line_start: usize, from: usize, n: usize) -> Option<usize> {
    let bytes = md.as_bytes();
    let mut heading = atx(&md[line_start..]);
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'`' => {
                let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                if run == n {
                    return Some(i + n);
                }
                i += run;
            }
            b'\n' => {
                let next = &md[i + 1..];
                let line = &next[..next.find('\n').map_or(next.len(), |e| e + 1)];
                let (_, _, at, _) = quotes(line);
                if heading || line[at..].trim().is_empty() || fence_open(line).is_some() {
                    return None;
                }
                heading = atx(line);
                if heading {
                    return None;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// True when a line, after container markers, is an ATX heading.
fn atx(line: &str) -> bool {
    let rest = strip_containers(line);
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    (1..=6).contains(&hashes)
        && matches!(
            rest.as_bytes().get(hashes),
            None | Some(b' ' | b'\t' | b'\n' | b'\r')
        )
}

/// What the stripper knows about the block structure at a line start.
#[derive(Default)]
struct Blocks {
    fence: Option<Fence>,
    /// The line before continues a paragraph.
    paragraph: bool,
    /// Open list items: quote depth and content column.
    lists: Vec<(usize, usize)>,
}

impl Blocks {
    /// True when `line` is code: in a fence, opening one, or indented code.
    fn code(&mut self, line: &str) -> bool {
        if let Some(fence) = self.fence {
            if fence_close(line, fence) {
                self.fence = None;
            }
            return true;
        }
        let (depth, quote, at, col) = quotes(line);
        if line[at..].trim().is_empty() {
            return false;
        }
        // Dropping an item too soon only reads more lines as code.
        self.lists.retain(|&(d, c)| d <= depth && c <= col);
        let base = self.lists.last().map_or(quote, |&(_, c)| c.max(quote));
        if !self.paragraph && col >= base + 4 {
            return true;
        }
        if !thematic(&line[at..]) {
            let mut at = at;
            while let Some((content, text, one)) = list_item(line, at) {
                // Only a bullet or `1.` with text can interrupt a paragraph.
                if self.paragraph && !(text && one) {
                    break;
                }
                self.lists.push((depth, content));
                let token = line[at..]
                    .find(char::is_whitespace)
                    .map_or(line.len(), |e| at + e);
                at = line.len() - line[token..].trim_start_matches([' ', '\t']).len();
            }
        }
        if let Some(fence) = fence_open(line) {
            self.fence = Some(fence);
            return true;
        }
        false
    }
}

/// A note's Markdown without its comments.
pub struct Stripped<'a> {
    pub text: Cow<'a, str>,
    /// The last comment runs to the end of the note.
    pub unclosed: bool,
    /// The byte offset of each `%%` that opened a comment.
    pub openers: Vec<usize>,
}

/// Removes `%%comments%%` from a note's Markdown, in note order, before it is parsed. A
/// comment runs to the next `%%`, or to the end of the note. Fenced code, indented code and
/// code spans keep their `%%`, unless a comment holds them. Newlines in a comment stay, so
/// lines keep their places.
pub fn strip_comments(md: &str) -> Stripped<'_> {
    let mut stripped = Stripped {
        text: Cow::Borrowed(md),
        unclosed: false,
        openers: Vec::new(),
    };
    // No comment opens past the last `%%`, so the rest of the note is kept as it is.
    let Some(last) = md.rfind("%%") else {
        return stripped;
    };
    let bytes = md.as_bytes();
    let mut next = Next::new(md);
    let mut out = String::with_capacity(md.len());
    let mut blocks = Blocks::default();
    let (mut i, mut kept, mut start, mut line) = (0, 0, true, 0);
    while i <= last {
        if start {
            (start, line) = (false, i);
            let end = md[i..].find('\n').map_or(md.len(), |e| i + e + 1);
            if blocks.code(&md[i..end]) {
                blocks.paragraph = false;
                (i, start) = (end, true);
                continue;
            }
        }
        match bytes[i] {
            b'\n' => {
                out.push_str(&md[kept..=i]);
                (i, kept, start) = (i + 1, i + 1, true);
                let line = &out[out[..out.len() - 1].rfind('\n').map_or(0, |n| n + 1)..];
                blocks.paragraph = paragraph_line(line);
            }
            // An escape never hides a comment marker.
            b'\\' => {
                i += if matches!(bytes.get(i + 1), Some(b'\n' | b'%')) {
                    1
                } else {
                    2
                }
            }
            b'`' => {
                let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                let end = span_close(md, line, i + n, n).unwrap_or(i + n);
                if let Some(nl) = md[i..end].rfind('\n') {
                    line = i + nl + 1;
                }
                i = end;
            }
            b'%' if bytes.get(i + 1) == Some(&b'%') => {
                out.push_str(&md[kept..i]);
                stripped.openers.push(i);
                let body = i + 2;
                let close = md[body..].find("%%").map(|at| body + at);
                let inside = &md[body..close.unwrap_or(md.len())];
                out.extend(inside.bytes().filter(|&b| b == b'\n').map(char::from));
                match close {
                    Some(at) => (i, kept) = (at + 2, at + 2),
                    None => {
                        stripped.unclosed = true;
                        stripped.text = Cow::Owned(out);
                        return stripped;
                    }
                }
            }
            _ => i = next.at(md, i + 1),
        }
    }
    out.push_str(&md[kept.min(md.len())..]);
    stripped.text = Cow::Owned(out);
    stripped
}

/// The next byte the stripper acts on: a newline, `\\`, a backtick or `%`. Each is found by a
/// fast search, rather than a test of every byte in between.
struct Next([usize; 4]);

impl Next {
    const BYTES: [char; 4] = ['\n', '\\', '`', '%'];

    fn new(md: &str) -> Next {
        Next(Self::BYTES.map(|c| md.find(c).unwrap_or(md.len())))
    }

    /// The offset of the first such byte at or after `from`, or the end of `md`.
    fn at(&mut self, md: &str, from: usize) -> usize {
        // Each byte sought is ASCII, so none starts inside a character.
        let from = (from..md.len())
            .find(|&i| md.is_char_boundary(i))
            .unwrap_or(md.len());
        for (at, c) in self.0.iter_mut().zip(Self::BYTES) {
            if *at < from {
                *at = md[from..].find(c).map_or(md.len(), |i| from + i);
            }
        }
        self.0.iter().copied().min().unwrap_or(md.len())
    }
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

/// Puts a block embed's `html` in place of `node`, between the part of `paragraph` before it
/// and the part after, so no block sits inside a `<p>`. An empty part is dropped.
fn split_paragraph<'a>(
    arena: &'a Arena<'a>,
    paragraph: &'a AstNode<'a>,
    node: &'a AstNode<'a>,
    html: String,
) {
    let breaks =
        |n: &'a AstNode<'a>| matches!(n.data().value, NodeValue::SoftBreak | NodeValue::LineBreak);
    let after = arena.alloc(AstNode::from(NodeValue::Paragraph));
    while let Some(next) = node.next_sibling() {
        after.append(next);
    }
    let block = arena.alloc(AstNode::from(NodeValue::Raw(html)));
    node.detach();
    paragraph.insert_after(block);
    while let Some(last) = paragraph.last_child().filter(|n| breaks(n)) {
        last.detach();
    }
    while let Some(first) = after.first_child().filter(|n| breaks(n)) {
        first.detach();
    }
    if let Some(last) = paragraph.last_child()
        && let NodeValue::Text(text) = &mut last.data_mut().value
    {
        *text = Cow::Owned(text.trim_end().to_string());
    }
    if let Some(first) = after.first_child()
        && let NodeValue::Text(text) = &mut first.data_mut().value
    {
        *text = Cow::Owned(text.trim_start().to_string());
    }
    if after.first_child().is_some() {
        block.insert_after(after);
    }
    if paragraph.first_child().is_none() {
        paragraph.detach();
    }
}

fn replace<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>, html: String) {
    node.insert_before(arena.alloc(AstNode::from(NodeValue::Raw(html))));
    node.detach();
}

pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    // Writing into a String cannot fail.
    let _ = comrak::html::escape(&mut escaped, text);
    escaped
}

/// The URL of a folder's index page.
pub fn folder_url(folder: &str) -> String {
    format!("/.tachylite/folders/{}", encode(folder))
}

/// Links to each folder above a path, outermost first, or nothing at the root.
pub fn crumbs(folder: &str) -> String {
    if folder.is_empty() {
        return String::new();
    }
    let mut html = String::from("<nav class=\"crumbs\">");
    let mut end = 0;
    for (i, name) in folder.split('/').enumerate() {
        if i > 0 {
            html.push_str(" / ");
        }
        end += name.len() + usize::from(i > 0);
        let href = folder_url(&folder[..end]);
        html.push_str(&format!("<a href=\"{href}\">{}</a>", escape(name)));
    }
    html.push_str("</nav>\n");
    html
}

/// Wraps a note's body in the page shell, under links to its folders and above its
/// backlinks, given as `(title, url)` pairs.
pub fn render_page(path: &str, title: &str, body: &str, backlinks: &[(String, String)]) -> Page {
    let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
    let mut main = crumbs(folder);
    main.push_str(body);
    if !backlinks.is_empty() {
        main.push_str("<section class=\"backlinks\"><h2>Backlinks</h2><ul>");
        for (text, href) in backlinks {
            main.push_str(&format!("<li><a href=\"{href}\">{}</a></li>", escape(text)));
        }
        main.push_str("</ul></section>\n");
    }
    shell(title, &main)
}

/// Every page's template: the head, the header, and `main`. `title` is plain text.
pub fn shell(title: &str, main: &str) -> Page {
    let title = escape(title);
    let before = "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
                  <meta name=\"color-scheme\" content=\"dark light\">\n\
                  <link rel=\"manifest\" href=\"/.tachylite/app.webmanifest\">\n\
                  <link rel=\"stylesheet\" href=\"/.tachylite/style.css\">\n\
                  <meta name=\"tachylite-version\" content=\"";
    let after = format!(
        "\">\n<title>{title}</title>\n\
         <script src=\"/.tachylite/live.js\" defer></script>\n</head>\n<body>\n\
         <header><a class=\"site\" href=\"/\">Tachylite</a><a href=\"/.tachylite/recent\">Recent</a>\
         <button type=\"button\" class=\"find\" title=\"Find a note (Ctrl-K)\">Find</button>\
         </header>\n<main>\n{main}</main>\n</body>\n</html>\n"
    );
    // The version hashes the whole page but itself, so a new template re-syncs every client.
    let mut hasher = DefaultHasher::new();
    (before, &after).hash(&mut hasher);
    let version = format!("{:016x}", hasher.finish());
    let html = format!("{before}{version}{after}");
    Page { html, version }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines and marks that move where code starts and ends, to the stripper or the parser.
    const PARTS: &[&str] = &[
        "%%",
        "%%",
        "%%d",
        "\n",
        "\n",
        "\n\n",
        "\r",
        "\r\n",
        " ",
        "  ",
        "    ",
        "\t",
        "\u{3000}",
        "```",
        "```\n",
        "~~~",
        "~",
        "`",
        "  ```",
        "<div>",
        "</div>",
        "<details>",
        "<x-y>",
        "<pre>",
        "</pre>",
        "<PRE>",
        "<!--",
        "-->",
        "<span title=\"",
        "\">",
        "<",
        "- ",
        "> ",
        "1. ",
        "# ",
        "---",
        "|",
        "x",
        "word ",
    ];

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(20_000))]

        /// The fast path never clears a note the parser reads an opener of as code.
        #[test]
        fn plain_notes_open_no_comment_in_code(
            parts in proptest::collection::vec(proptest::sample::select(PARTS), 0..30)
        ) {
            let md = parts.concat();
            let openers = strip_comments(&md).openers;
            if !openers.is_empty() && plain(&md, &openers) {
                proptest::prop_assert!(!parsed_in_code(&md, &openers), "md={md:?}");
            }
        }
    }
}
