//! Index pages: pure builders from lists the site gathers.

use std::cmp::Reverse;

use crate::render::{Page, crumbs, escape, folder_url, shell};

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

pub const HOME_RECENT: usize = 10;
pub const RECENT: usize = 50;

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
            html.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>",
                item.url,
                escape(item.title)
            ));
        }
        html.push_str("</ul>\n");
    }
    html
}

fn recent_list(items: &[&Item]) -> String {
    let mut html = String::from("<ul class=\"recent\">");
    for item in items {
        html.push_str(&format!(
            "<li><a href=\"{}\">{}</a> <span class=\"meta\">",
            item.url,
            escape(item.title)
        ));
        if !item.folder.is_empty() {
            html.push_str(&escape(item.folder));
            html.push_str(" · ");
        }
        html.push_str(&date(item.modified));
        html.push_str("</span></li>");
    }
    html.push_str("</ul>\n");
    html
}

/// `/`: the newest notes, then the root folder. The header names the site, so the
/// page's heading is for screen readers only.
pub fn home(site: &str, recent: &[&Item], root: &Folder) -> Page {
    let mut main = format!("<h1 class=\"sr\">{}</h1>\n", escape(site));
    if !recent.is_empty() {
        main.push_str("<h2>Recent</h2>\n");
        main.push_str(&recent_list(&recent[..recent.len().min(HOME_RECENT)]));
        main.push_str("<p><a href=\"/.tachylite/recent\">All recent notes</a></p>\n");
    }
    main.push_str(&listing(root));
    shell(site, &main)
}

/// `/.tachylite/folders/<folder>`.
pub fn folder(folder: &Folder) -> Page {
    let (parent, name) = folder.path.rsplit_once('/').unwrap_or(("", folder.path));
    let mut main = crumbs(parent);
    main.push_str(&format!("<h1>{}</h1>\n", escape(name)));
    main.push_str(&listing(folder));
    shell(name, &main)
}

/// `/.tachylite/recent`.
pub fn recent(recent: &[&Item]) -> Page {
    let mut main = String::from("<h1>Recent</h1>\n");
    main.push_str(&recent_list(&recent[..recent.len().min(RECENT)]));
    shell("Recent", &main)
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
