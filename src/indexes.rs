//! Index pages: pure builders from lists the site gathers.

use std::cmp::Reverse;

use crate::render::{Page, crumbs, encode, escape, folder_url, shell};

/// A note or html page as an index lists it.
pub struct Item<'a> {
    pub title: &'a str,
    pub url: String,
    /// The folder it lives in; `""` at the root.
    pub folder: &'a str,
    /// Seconds since the epoch.
    pub modified: i64,
}

/// One folder's contents.
pub struct Folder<'a> {
    /// The folder's path; `""` for the root.
    pub path: &'a str,
    /// The subfolders' paths, in listing order.
    pub folders: Vec<&'a str>,
    /// The notes and html pages, in listing order.
    pub items: Vec<Item<'a>>,
}

/// How a folder listing orders a name: names that start with an ISO date
/// (`YYYY-MM-DD`) come first, newest first; the rest follow by name, ignoring case.
pub fn order(name: &str) -> (bool, Reverse<String>, String) {
    let lower = name.to_lowercase();
    if dated(name) {
        (false, Reverse(lower), String::new())
    } else {
        (true, Reverse(String::new()), lower)
    }
}

fn dated(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 10
        && b[..10].iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

pub const HOME_RECENT: usize = 5;
pub const RECENT: usize = 50;
/// The newest daily notes that open the Recent page, and that the home page leaves out.
pub const DAILY: usize = 3;
/// The daily notes the Today page lists. Enough to reach back two months, and small.
pub const TODAY: usize = 60;

/// The URL of a folder's recent page.
pub fn recent_url(folder: &str) -> String {
    format!("/.tachylite/recent/{}", encode(folder))
}

/// A pin as the header shows it.
pub struct Pin {
    pub label: String,
    pub url: String,
}

/// A tab: a top-level page the header links to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Today,
    Recent,
}

/// The header every page carries: the tab bar of Home, Today when there is a Today page,
/// and Recent; then Find, and then the pins. A tab page marks its own tab as `current`. It
/// comes from config alone, so a save never changes it.
pub fn header(today: bool, pins: &[Pin], current: Option<Tab>) -> String {
    let mut html = String::from("<header>");
    for (tab, url, name) in [
        (Tab::Home, "/", "Home"),
        (Tab::Today, "/.tachylite/today", "Today"),
        (Tab::Recent, "/.tachylite/recent", "Recent"),
    ] {
        if tab == Tab::Today && !today {
            continue;
        }
        // Written here, so the tab shows as current with no script.
        let mark = if current == Some(tab) {
            " aria-current=\"page\""
        } else {
            ""
        };
        html.push_str(&format!("<a href=\"{url}\"{mark}>{name}</a>"));
    }
    html.push_str(
        "<button type=\"button\" class=\"find\" title=\"Find a note (Ctrl-K)\">Find</button>",
    );
    if !pins.is_empty() {
        html.push_str("<nav class=\"pins\">");
        for pin in pins {
            html.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                pin.url,
                escape(&pin.label)
            ));
        }
        html.push_str("</nav>");
    }
    html.push_str("</header>\n");
    html
}

/// One piece of a daily-note format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Year,
    Month,
    Day,
    Literal(char),
}

/// Parses a Moment.js format of `YYYY`, `MM` and `DD` with literal separators, `/` for a
/// subfolder. Any other token is refused with why: a guess could list the wrong notes.
pub fn date_format(format: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut rest = format;
    while let Some(c) = rest.chars().next() {
        let (part, len) = if rest.starts_with("YYYY") {
            (Part::Year, 4)
        } else if rest.starts_with("MM") {
            (Part::Month, 2)
        } else if rest.starts_with("DD") {
            (Part::Day, 2)
        } else if c.is_alphabetic() || c == '[' {
            let token: String = rest.chars().take_while(|t| *t == c).collect();
            return Err(format!(
                "daily format {format:?}: {token:?} is not supported; use YYYY, MM and DD"
            ));
        } else {
            (Part::Literal(c), c.len_utf8())
        };
        parts.push(part);
        rest = &rest[len..];
    }
    for (part, name) in [(Part::Year, "YYYY"), (Part::Month, "MM"), (Part::Day, "DD")] {
        if !parts.contains(&part) {
            return Err(format!("daily format {format:?}: it has no {name}"));
        }
    }
    Ok(parts)
}

/// The ISO date `YYYY-MM-DD` a name in `format` carries, or `None` if it does not match.
/// A token used twice must read the same both times.
pub fn date_of(format: &[Part], name: &str) -> Option<String> {
    let mut rest = name.as_bytes();
    let mut ymd = [None::<u32>; 3];
    for part in format {
        let (slot, len) = match part {
            Part::Literal(c) => {
                let mut buf = [0; 4];
                rest = rest.strip_prefix(c.encode_utf8(&mut buf).as_bytes())?;
                continue;
            }
            Part::Year => (0, 4),
            Part::Month => (1, 2),
            Part::Day => (2, 2),
        };
        let digits = rest.get(..len)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let n = digits.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0'));
        if *ymd[slot].get_or_insert(n) != n {
            return None;
        }
        rest = &rest[len..];
    }
    let [Some(y), Some(m), Some(d)] = ymd else {
        return None;
    };
    (rest.is_empty() && (1..=12).contains(&m) && (1..=31).contains(&d))
        .then(|| format!("{y:04}-{m:02}-{d:02}"))
}

fn listing(folder: &Folder) -> String {
    let mut html = String::new();
    if !folder.folders.is_empty() {
        html.push_str("<ul class=\"folders\">");
        for path in &folder.folders {
            let name = path.rsplit('/').next().unwrap_or(path);
            html.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>",
                folder_url(path),
                escape(name)
            ));
        }
        html.push_str("</ul>\n");
    }
    if !folder.items.is_empty() {
        html.push_str("<ul class=\"notes\">");
        for item in &folder.items {
            html.push_str(&row(item.title, &item.url));
        }
        html.push_str("</ul>\n");
    }
    html
}

/// One page's line in a list of notes.
pub fn row(title: &str, url: &str) -> String {
    format!("<li><a href=\"{url}\">{}</a></li>", escape(title))
}

/// One note's line in a recent list: its title, then its folder and date.
pub fn recent_row(item: &Item) -> String {
    let mut html = format!(
        "<li><a href=\"{}\">{}</a> <span class=\"meta\">",
        item.url,
        escape(item.title)
    );
    if !item.folder.is_empty() {
        html.push_str(&escape(item.folder));
        html.push_str(" · ");
    }
    html.push_str(&date(item.modified));
    html.push_str("</span></li>");
    html
}

fn recent_list<'a>(rows: impl IntoIterator<Item = &'a str>) -> String {
    let mut html = String::from("<ul class=\"recent\">");
    html.extend(rows);
    html.push_str("</ul>\n");
    html
}

/// `/`: the newest of `recent`, then the root folder. The Home tab names the page, so its
/// heading is for screen readers only.
pub fn home(header: &str, site: &str, recent: &[&str], root: &Folder) -> Page {
    let mut main = format!("<h1 class=\"sr\">{}</h1>\n", escape(site));
    if !recent.is_empty() {
        main.push_str("<h2>Recent</h2>\n");
        main.push_str(&recent_list(recent.iter().take(HOME_RECENT).copied()));
    }
    main.push_str(&listing(root));
    shell(header, site, &main)
}

/// `/.tachylite/folders/<folder>`, with a link to its recent page when it has one.
pub fn folder(header: &str, folder: &Folder, recent: bool) -> Page {
    let (parent, name) = folder.path.rsplit_once('/').unwrap_or(("", folder.path));
    let mut main = crumbs(parent);
    main.push_str(&format!("<h1>{}</h1>\n", escape(name)));
    if recent {
        main.push_str(&format!(
            "<p><a href=\"{}\">Recent in {}</a></p>\n",
            recent_url(folder.path),
            escape(name)
        ));
    }
    main.push_str(&listing(folder));
    shell(header, name, &main)
}

/// `/.tachylite/tags/<tag>`: the notes with the tag or a tag nested under it, as `row`s in
/// listing order.
pub fn tag<'a>(header: &str, tag: &str, rows: impl IntoIterator<Item = &'a str>) -> Page {
    let mut main = format!("<h1>#{}</h1>\n<ul class=\"notes\">", escape(tag));
    main.extend(rows);
    main.push_str("</ul>\n");
    shell(header, &format!("#{tag}"), &main)
}

/// `/.tachylite/recent`: a Daily block of the newest daily notes when there are any, then
/// the other notes, newest first. Both are `recent_row`s. The Recent tab names the page,
/// as the Home tab names the home page.
pub fn recent(header: &str, daily: &[&str], rows: &[&str]) -> Page {
    let mut main = String::from("<h1 class=\"sr\">Recent notes</h1>\n");
    if !daily.is_empty() {
        main.push_str("<h2>Daily</h2>\n");
        main.push_str(&recent_list(daily.iter().copied()));
    }
    if !rows.is_empty() {
        main.push_str("<h2>Recent</h2>\n");
        main.push_str(&recent_list(rows.iter().take(RECENT).copied()));
    }
    shell(header, "Recent", &main)
}

/// `/.tachylite/recent/<folder>`: the newest notes under a folder, at any depth.
pub fn recent_in<'a>(header: &str, folder: &str, rows: impl IntoIterator<Item = &'a str>) -> Page {
    let name = folder.rsplit('/').next().unwrap_or(folder);
    let mut main = crumbs(folder);
    main.push_str(&format!("<h1>Recent in {}</h1>\n", escape(name)));
    main.push_str(&recent_list(rows));
    shell(header, &format!("Recent in {name}"), &main)
}

/// `/.tachylite/today`: the daily notes as `(date, title, url)`, newest first. Each link
/// carries its ISO date, so the client shows the newest dated on or before its own today in
/// place of the list. The list sits in `[data-pick]`, which the stylesheet hides while a
/// script can run, so the list never paints before the note. A note's raw HTML cannot carry
/// that attribute.
pub fn today(header: &str, days: &[(&str, &str, String)]) -> Page {
    let mut main = String::new();
    if days.is_empty() {
        main.push_str("<h1>Daily notes</h1>\n<p>No daily notes yet.</p>\n");
    } else {
        main.push_str("<div data-pick>\n<h1>Daily notes</h1>\n<ul class=\"notes\">");
        for (date, title, url) in days {
            main.push_str(&format!(
                "<li><a href=\"{url}\" data-date=\"{date}\">{}</a></li>",
                escape(title)
            ));
        }
        main.push_str("</ul>\n</div>\n");
    }
    shell(header, "Today", &main)
}

/// `/.tachylite/notes.json`: `[[title, url, path, alias…], …]`, for the switcher.
#[derive(Default)]
pub struct NotesJson(String);

impl NotesJson {
    pub fn push(&mut self, title: &str, url: &str, path: &str, aliases: &[String]) {
        self.0.push(if self.0.is_empty() { '[' } else { ',' });
        self.0.push('[');
        json_string(&mut self.0, title);
        self.0.push(',');
        json_string(&mut self.0, url);
        self.0.push(',');
        json_string(&mut self.0, path);
        for alias in aliases {
            self.0.push(',');
            json_string(&mut self.0, alias);
        }
        self.0.push(']');
    }

    pub fn finish(mut self) -> String {
        if self.0.is_empty() {
            self.0.push('[');
        }
        self.0.push(']');
        self.0
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => {
                out.push_str("\\u00");
                out.push(char::from(HEX[c as usize >> 4]));
                out.push(char::from(HEX[c as usize & 15]));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `YYYY-MM-DD` in UTC, from seconds since the epoch.
pub fn date(seconds: i64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// How many search shards there are. A fixed count, so a note that comes or goes moves no
/// other note to a new shard. `docs/plans/2026-11-19-search.md` says how it was picked.
pub const SHARDS: u32 = 512;

/// The search shard a note's text lives in: FNV-1a of its path.
pub fn shard(path: &str) -> u32 {
    let hash = path.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    hash % SHARDS
}

/// The URL key of a search shard.
pub fn shard_key(shard: u32) -> String {
    format!(".tachylite/search/{shard:03x}.txt")
}

/// Whether a tag sits inside a word or a sentence. One leaves no gap in the text, so
/// `<b>bold</b>ly` reads `boldly`. Any other tag leaves a space.
fn inline(name: &str) -> bool {
    let mut lower = [0; 6];
    let Some(lower) = lower.get_mut(..name.len()) else {
        return false;
    };
    lower.copy_from_slice(name.as_bytes());
    lower.make_ascii_lowercase();
    matches!(
        &*lower,
        b"a" | b"abbr"
            | b"b"
            | b"bdi"
            | b"bdo"
            | b"cite"
            | b"code"
            | b"del"
            | b"dfn"
            | b"em"
            | b"i"
            | b"ins"
            | b"kbd"
            | b"mark"
            | b"q"
            | b"s"
            | b"samp"
            | b"small"
            | b"span"
            | b"strike"
            | b"strong"
            | b"sub"
            | b"sup"
            | b"time"
            | b"tt"
            | b"u"
            | b"var"
            | b"wbr"
    )
}

/// Appends a note's search row: its URL, its modified time and the text of `body`, with a
/// tab between and a newline after. The text is the HTML with tags dropped, character
/// references decoded and each run of whitespace made one space, so it holds neither.
pub fn search_row(out: &mut String, url: &str, modified: i64, body: &str) {
    out.push_str(url);
    out.push('\t');
    out.push_str(&modified.to_string());
    out.push('\t');
    let start = out.len();
    let bytes = body.as_bytes();
    // A byte of text: not whitespace, a control, or the start of a tag or a reference.
    let text = |b: u8| b > b' ' && !matches!(b, b'<' | b'&' | 0x7f);
    let mut gap = false;
    let mut at = 0;
    while at < bytes.len() {
        // A run of words with one space between each is copied whole.
        let mut end = at;
        while end < bytes.len() {
            if text(bytes[end]) {
                end += 1;
            } else if end > at && bytes[end] == b' ' && bytes.get(end + 1).is_some_and(|b| text(*b))
            {
                end += 2;
            } else {
                break;
            }
        }
        if end > at {
            if gap && out.len() > start {
                out.push(' ');
            }
            gap = false;
            // A run ends at an ASCII byte, so it ends on a character boundary.
            out.push_str(&body[at..end]);
            at = end;
            continue;
        }
        let (decoded, len) = match bytes[at] {
            b'<' => {
                let (len, inline) = element(&body[at..]);
                (if inline { None } else { Some(' ') }, len)
            }
            b'&' => reference(&body[at..]),
            _ => (Some(' '), 1),
        };
        at += len;
        match decoded {
            Some(c) if c <= ' ' || c == '\u{7f}' => gap = true,
            Some(c) => {
                if gap && out.len() > start {
                    out.push(' ');
                }
                gap = false;
                out.push(c);
            }
            None => {}
        }
    }
    out.push('\n');
}

/// The length of the tag that starts `html`, and whether it is an inline one. A `>` inside
/// a quoted attribute does not end it. A tag with no end runs to the end.
fn element(html: &str) -> (usize, bool) {
    let name = html[1..].trim_start_matches('/');
    let name = &name[..name
        .bytes()
        .position(|b| !b.is_ascii_alphanumeric())
        .unwrap_or(name.len())];
    let inline = inline(name);
    let bytes = html.as_bytes();
    let mut at = 1;
    while let Some(byte) = bytes.get(at) {
        match byte {
            b'>' => return (at + 1, inline),
            // A quoted value runs to its closing quote, or to the end.
            b'"' | b'\'' => {
                let close = bytes[at + 1..].iter().position(|b| b == byte);
                at += close.map_or(bytes.len(), |close| close + 1);
            }
            _ => {}
        }
        at += 1;
    }
    (html.len(), inline)
}

/// The character that the reference at the start of `html` names, and the reference's
/// length. Anything else is a literal `&`.
fn reference(html: &str) -> (Option<char>, usize) {
    let end = html.bytes().take(12).position(|b| b == b';');
    let decoded = end.and_then(|end| {
        let c = match &html[1..end] {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            "nbsp" => ' ',
            name => {
                let number = name.strip_prefix('#')?;
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => number.parse().ok()?,
                };
                char::from_u32(code)?
            }
        };
        Some((Some(c), end + 1))
    });
    decoded.unwrap_or((Some('&'), 1))
}

#[cfg(test)]
mod tests {
    use super::search_row;

    fn text(body: &str) -> String {
        let mut row = String::new();
        search_row(&mut row, "/u", 1, body);
        let text = row
            .strip_prefix("/u\t1\t")
            .and_then(|r| r.strip_suffix('\n'));
        text.expect("a row is url, time, text and a newline")
            .to_string()
    }

    #[test]
    fn a_search_row_keeps_words_and_drops_markup() {
        for (body, want) in [
            ("  one  two \n three ", "one two three"),
            ("<p>one</p>\n<p>two</p>", "one two"),
            ("<B>bold</B>ly <Em>so</Em>", "boldly so"),
            ("one <b>two</b> three", "one two three"),
            ("one<br>two<span>three</span>", "one twothree"),
            ("<a title=\"a > b\" href='c>d'>link</a>", "link"),
            ("<div title=\"it's > x\">in</div> out", "in out"),
            ("cut <a title=\"no end>text", "cut"),
            (
                "a &amp; b&nbsp;c &#x41;&#66; &unknown; &",
                "a & b c AB &unknown; &",
            ),
            ("é one\u{7f}two", "é one two"),
            ("", ""),
        ] {
            assert_eq!(text(body), want, "{body:?}");
        }
    }
}
