//! Frontmatter: the YAML subset Obsidian's Properties support. Scalars, flow and block lists,
//! and `|` or `>` block scalars. Any other value, such as a nested map, is kept as raw text.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    List(Vec<String>),
}

/// What the site needs from a note's frontmatter before any body renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub title: Option<String>,
    pub aliases: Vec<String>,
}

/// The YAML between a first line of `---` and the next line of `---`, and the body after it.
/// Without a closing line there is no frontmatter. A byte-order mark may come before it.
pub fn split(md: &str) -> (Option<&str>, &str) {
    let unmarked = md.strip_prefix('\u{FEFF}').unwrap_or(md);
    let Some(rest) = unmarked
        .strip_prefix("---\n")
        .or_else(|| unmarked.strip_prefix("---\r\n"))
    else {
        return (None, md);
    };
    let mut start = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return (Some(&rest[..start]), &rest[start + line.len()..]);
        }
        start += line.len();
    }
    (None, md)
}

pub fn meta(md: &str) -> Meta {
    let Some(yaml) = split(md).0 else {
        return Meta::default();
    };
    let mut meta = Meta::default();
    for (key, value) in parse(yaml) {
        match (key.as_str(), value) {
            ("title", Value::Text(t)) if meta.title.is_none() && !t.trim().is_empty() => {
                meta.title = Some(t.trim().to_string());
            }
            ("aliases", Value::Text(t)) if !t.trim().is_empty() => {
                meta.aliases = vec![t.trim().to_string()];
            }
            ("aliases", Value::List(items)) => {
                meta.aliases = items
                    .into_iter()
                    .map(|a| a.trim().to_string())
                    .filter(|a| !a.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    meta
}

pub fn parse(yaml: &str) -> Vec<(String, Value)> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.trim().is_empty()
            || line.trim_start().starts_with('#')
            || line.starts_with([' ', '\t', '-'])
        {
            continue;
        }
        let Some((key, rest)) = key_value(line) else {
            continue;
        };
        // The lines that belong to this key: indented ones, and `-` items at column 0.
        let start = i;
        while i < lines.len()
            && (lines[i].trim().is_empty() || lines[i].starts_with([' ', '\t', '-']))
        {
            i += 1;
        }
        let block = &lines[start..i];
        let value = match rest {
            "" => nested(block),
            "|" | "|-" | "|+" => Value::Text(dedent(block).join("\n").trim_end().to_string()),
            ">" | ">-" | ">+" => Value::Text(fold(&dedent(block))),
            // YAML reads an unquoted `[[link]]` as a nested list; the author meant a link.
            link if one_link(link) => Value::Text(link.to_string()),
            other => match flow(other) {
                Some(inner) => Value::List(items(inner)),
                None => Value::Text(scalar(other)),
            },
        };
        out.push((key, value));
    }
    out
}

/// True for exactly one `[[link]]`.
pub fn one_link(s: &str) -> bool {
    s.strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .is_some_and(|inner| !inner.contains("]]") && !inner.contains("[["))
}

/// The inside of a flow list, `[a, b]`, which a ` # comment` may follow.
fn flow(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('[')?;
    if let Some(inner) = inner.strip_suffix(']') {
        return Some(inner);
    }
    let mut quote = None;
    for (at, c) in inner.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, ']') => {
                let after = &inner[at + 1..];
                if after.starts_with([' ', '\t']) && after.trim_start().starts_with('#') {
                    return Some(&inner[..at]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `key: rest`, split at the first colon followed by a space, a tab or the line end, and
/// after a quoted key's closing quote.
fn key_value(line: &str) -> Option<(String, &str)> {
    let bytes = line.as_bytes();
    let from = match bytes.first() {
        Some(&q @ (b'"' | b'\'')) => closing_quote(line, q).map_or(0, |at| at + 1),
        _ => 0,
    };
    let colon = (from..bytes.len()).find(|&j| {
        bytes[j] == b':' && bytes.get(j + 1).is_none_or(|b| matches!(b, b' ' | b'\t'))
    })?;
    let key = scalar(line[..colon].trim());
    (!key.is_empty()).then(|| (key, line[colon + 1..].trim()))
}

/// The offset of the quote that closes the one `line` opens with: `\"` escapes a double
/// quote, and `''` a single one.
fn closing_quote(line: &str, q: u8) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut j = 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if q == b'"' => j += 1,
            b if b == q && q == b'\'' && bytes.get(j + 1) == Some(&q) => j += 1,
            b if b == q => return Some(j),
            _ => {}
        }
        j += 1;
    }
    None
}

/// A block under an empty value: a list if every line is an item, else raw text. A comment
/// line is neither, unless it is a block scalar's text.
fn nested(block: &[&str]) -> Value {
    let indent = |l: &str| l.len() - l.trim_start().len();
    // The indent of the line that opened a block scalar, while its text runs.
    let mut opener = None;
    let mut lines = Vec::new();
    for &line in block {
        if line.trim().is_empty() {
            continue;
        }
        if opener.is_some_and(|at| indent(line) > at) {
            lines.push(line);
            continue;
        }
        opener = None;
        if line.trim_start().starts_with('#') {
            continue;
        }
        let end = line.trim_end();
        let opens = ["|", "|-", "|+", ">", ">-", ">+"].iter().any(|ind| {
            end.strip_suffix(ind)
                .is_some_and(|before| before.ends_with([' ', '\t']) || before.trim().is_empty())
        });
        if opens {
            opener = Some(indent(line));
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return Value::Text(String::new());
    }
    let mut list = Vec::new();
    for line in &lines {
        match line.trim() {
            "-" => list.push(String::new()),
            item => match item.strip_prefix("- ") {
                Some(item) => list.push(scalar(item)),
                None => return Value::Text(dedent(&lines).join("\n").trim_end().to_string()),
            },
        }
    }
    Value::List(list)
}

/// The lines with their common leading whitespace removed.
fn dedent<'a>(block: &[&'a str]) -> Vec<&'a str> {
    let indent = block
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    block
        .iter()
        .map(|l| l.get(indent..).unwrap_or("").trim_end())
        .collect()
}

/// A `>` block: lines join with spaces, and a blank line is a line break.
fn fold(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        if line.is_empty() {
            out.push('\n');
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(line);
        }
    }
    out.trim_end().to_string()
}

/// The items of a flow list's inside, split at commas outside quotes.
fn items(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut quote = None;
    let mut start = 0;
    for (j, c) in inner.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, ',') => {
                out.push(scalar(&inner[start..j]));
                start = j + 1;
            }
            _ => {}
        }
    }
    let last = scalar(&inner[start..]);
    if !last.is_empty() || !out.is_empty() {
        out.push(last);
    }
    out
}

/// A plain, single-quoted or double-quoted scalar.
fn scalar(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => {}
                },
                c => out.push(c),
            }
        }
        out
    } else if let Some(rest) = s.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    break;
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        let end = s.find(" #").unwrap_or(s.len());
        s[..end].trim_end().to_string()
    }
}
