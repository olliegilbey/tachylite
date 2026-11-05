use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Anchorizer, Arena, Options, format_html, parse_document};

use crate::front::{self, Value};
use crate::links::{Index, Resolved, resolve, split};

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
}

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
            let matches = match unresolved {
                Resolved::Ambiguous(n) => n,
                _ => 0,
            };
            body.broken.push(Broken {
                from: from.to_string(),
                target: path.to_string(),
                matches,
            });
            "<a class=\"broken-link\">".to_string()
        }
    }
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
pub fn render_body(from: &str, markdown: &str, index: &Index) -> Body {
    let (yaml, markdown) = front::split(markdown);
    let mut body = Body::default();
    let props = yaml.map_or_else(String::new, |y| properties(&mut body, from, y, index));
    let arena = Arena::new();
    let options = options();
    let root = parse_document(&arena, markdown, &options);
    let mut rewrites = Vec::new();
    let mut anchorizer = Anchorizer::new();
    let mut ids = Vec::new();
    for node in root.descendants() {
        if let NodeValue::Heading(_) = node.data().value {
            ids.push(anchorizer.anchorize(&node.collect_text()));
            continue;
        }
        let raw = match &node.data().value {
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
        let Some((path, heading)) = raw else {
            continue;
        };
        let tag = open(&mut body, from, index, &path, heading.as_deref());
        rewrites.push((node, tag));
    }
    for (node, open) in rewrites {
        wrap(&arena, node, open);
    }
    let mut html = props;
    // Writing into a String cannot fail.
    let _ = format_html(root, &options, &mut html);
    body.html = add_ids(&html, &ids);
    body
}

/// Replaces a link node with raw tags around its children. comrak's own link markup would
/// leave `#` and `?` in a file name unescaped.
fn wrap<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>, open: String) {
    let raw = |html: String| arena.alloc(AstNode::from(NodeValue::Raw(html)));
    node.insert_before(raw(open));
    while let Some(child) = node.first_child() {
        node.insert_before(child);
    }
    node.insert_before(raw("</a>".to_string()));
    node.detach();
}

/// Gives each `<hN>` tag the next id, in document order. With raw HTML off, only a heading
/// writes `<hN>`. comrak's own heading ids would add an anchor link to every heading.
fn add_ids(html: &str, ids: &[String]) -> String {
    let mut out =
        String::with_capacity(html.len() + ids.iter().map(|id| id.len() + 6).sum::<usize>());
    let mut ids = ids.iter();
    let mut rest = html;
    while let Some(i) = rest.find("<h") {
        let heading = matches!(rest.as_bytes().get(i + 2..i + 4), Some([b'1'..=b'6', b'>']));
        match if heading { ids.next() } else { None } {
            Some(id) => {
                out.push_str(&rest[..i + 3]);
                out.push_str(&format!(" id=\"{}\">", escape(id)));
                rest = &rest[i + 4..];
            }
            None => {
                out.push_str(&rest[..i + 2]);
                rest = &rest[i + 2..];
            }
        }
    }
    out.push_str(rest);
    out
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
