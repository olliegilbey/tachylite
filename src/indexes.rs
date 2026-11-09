//! Index pages: pure builders from lists the site gathers.

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
    /// The subfolders' paths, sorted.
    pub folders: Vec<&'a str>,
    /// The notes and html pages, sorted by title.
    pub items: Vec<Item<'a>>,
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

/// `/`: the newest notes, then the root folder.
pub fn home(site: &str, recent: &[&Item], root: &Folder) -> Page {
    let mut main = format!("<h1>{}</h1>\n", escape(site));
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
pub fn notes_json<'a>(
    notes: impl Iterator<Item = (&'a str, String, &'a str, &'a [String])>,
) -> String {
    let mut json = String::from("[");
    for (i, (title, url, path, aliases)) in notes.enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push('[');
        json_string(&mut json, title);
        json.push(',');
        json_string(&mut json, &url);
        json.push(',');
        json_string(&mut json, path);
        for alias in aliases {
            json.push(',');
            json_string(&mut json, alias);
        }
        json.push(']');
    }
    json.push(']');
    json
}

fn json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
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
