//! The raw HTML in a note, rebuilt from an allowlist. Only tags built here reach the page,
//! and every other `<` and `>` is escaped, so a tag this misreads is dropped, never run.

const TAGS: &[&str] = &[
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

/// Elements dropped with their content: scripts, styles, frames, and raw text.
const DROP: &[&str] = &[
    "script",
    "style",
    "iframe",
    "noscript",
    "noembed",
    "noframes",
    "textarea",
    "title",
    "xmp",
    "plaintext",
    "template",
    "svg",
    "math",
];

fn allowed(tag: &str, attr: &str) -> bool {
    matches!(attr, "align" | "class" | "dir" | "id" | "lang" | "title")
        || matches!(
            (tag, attr),
            ("a", "href")
                | ("img", "src" | "alt" | "width" | "height" | "loading")
                | ("td" | "th", "colspan" | "rowspan")
                | ("col" | "colgroup", "span")
                | ("ol", "start")
                | ("details", "open")
                | ("time", "datetime")
        )
}

/// A link a browser cannot run: a web or mail scheme, a path, or a relative URL with no
/// scheme. An `&` could hide a colon in a character reference, so a relative URL has none.
fn safe_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if ["http://", "https://", "mailto:", "/", "#"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return true;
    }
    let head = url.split(['/', '?', '#']).next().unwrap_or(url);
    !url.contains('&') && !head.contains(':')
}

enum Token {
    Start(String, Vec<(String, String)>),
    End(String),
    /// A comment, a declaration, or a tag a browser drops.
    Skip,
}

pub fn sanitize(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(['<', '>']) {
        out.push_str(&rest[..i]);
        if rest.as_bytes()[i] == b'>' {
            out.push_str("&gt;");
            rest = &rest[i + 1..];
            continue;
        }
        let tail = &rest[i..];
        let Some((token, len)) = token(tail) else {
            out.push_str("&lt;");
            rest = &tail[1..];
            continue;
        };
        rest = &tail[len..];
        match token {
            Token::Start(name, _) if DROP.contains(&name.as_str()) => rest = after_end(rest, &name),
            Token::Start(name, attrs) if TAGS.contains(&name.as_str()) => {
                out.push('<');
                out.push_str(&name);
                let mut seen = Vec::new();
                for (attr, value) in attrs {
                    let url = attr == "href" || attr == "src";
                    if !allowed(&name, &attr) || seen.contains(&attr) || (url && !safe_url(&value))
                    {
                        continue;
                    }
                    out.push_str(&format!(" {attr}=\"{}\"", escape(&value)));
                    seen.push(attr);
                }
                out.push('>');
            }
            Token::End(name) if TAGS.contains(&name.as_str()) => {
                out.push_str(&format!("</{name}>"));
            }
            _ => {}
        }
    }
    out.push_str(rest);
    out
}

fn escape(value: &str) -> String {
    value
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The text after the end tag `</name ...>`, or nothing if it never closes.
fn after_end<'a>(rest: &'a str, name: &str) -> &'a str {
    let mut from = 0;
    while let Some(i) = rest[from..].find("</") {
        let at = from + i + 2;
        let close = rest
            .get(at..at + name.len())
            .is_some_and(|n| n.eq_ignore_ascii_case(name));
        if close {
            return rest[at..].find('>').map_or("", |j| &rest[at + j + 1..]);
        }
        from = at;
    }
    ""
}

fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// Reads one token at `s`, which starts with `<`, as a browser's tokenizer would. `None`
/// means the `<` is text. A tag cut off by the end of the input is skipped, as a browser
/// drops it.
fn token(s: &str) -> Option<(Token, usize)> {
    let b = s.as_bytes();
    let to_gt = |from: usize| s[from..].find('>').map_or(s.len(), |j| from + j + 1);
    if s[1..].starts_with("!--") {
        let end = s[4..].find("-->").map_or(s.len(), |j| 4 + j + 3);
        return Some((Token::Skip, end));
    }
    match b.get(1)? {
        b'!' | b'?' => Some((Token::Skip, to_gt(1))),
        b'/' => match b.get(2)? {
            c if c.is_ascii_alphabetic() => {
                let name_end = (2..b.len())
                    .find(|&j| space(b[j]) || b[j] == b'/' || b[j] == b'>')
                    .unwrap_or(b.len());
                let name = s[2..name_end].to_ascii_lowercase();
                match s[name_end..].find('>') {
                    Some(j) => Some((Token::End(name), name_end + j + 1)),
                    None => Some((Token::Skip, s.len())),
                }
            }
            b'>' => Some((Token::Skip, 3)),
            _ => Some((Token::Skip, to_gt(2))),
        },
        c if c.is_ascii_alphabetic() => start(s),
        _ => None,
    }
}

fn start(s: &str) -> Option<(Token, usize)> {
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() && !space(b[i]) && b[i] != b'/' && b[i] != b'>' {
        i += 1;
    }
    let name = s[1..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let cut = Some((Token::Skip, s.len()));
    loop {
        while i < b.len() && (space(b[i]) || b[i] == b'/') {
            i += 1;
        }
        match b.get(i) {
            None => return cut,
            Some(b'>') => return Some((Token::Start(name, attrs), i + 1)),
            _ => {}
        }
        let name_start = i;
        i += 1;
        while i < b.len() && !space(b[i]) && !matches!(b[i], b'/' | b'>' | b'=') {
            i += 1;
        }
        let attr = s[name_start..i].to_ascii_lowercase();
        while i < b.len() && space(b[i]) {
            i += 1;
        }
        let mut value = "";
        if b.get(i) == Some(&b'=') {
            i += 1;
            while i < b.len() && space(b[i]) {
                i += 1;
            }
            match b.get(i) {
                None => return cut,
                Some(&q) if q == b'"' || q == b'\'' => {
                    let Some(j) = s[i + 1..].find(q as char) else {
                        return cut;
                    };
                    value = &s[i + 1..i + 1 + j];
                    i += j + 2;
                }
                _ => {
                    let from = i;
                    while i < b.len() && !space(b[i]) && b[i] != b'>' {
                        i += 1;
                    }
                    value = &s[from..i];
                }
            }
        }
        attrs.push((attr, value.to_string()));
    }
}
