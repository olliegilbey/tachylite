use std::hash::{DefaultHasher, Hash, Hasher};

use comrak::{Options, markdown_to_html};

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
    options.extension.front_matter_delimiter = Some("---".into());
    options
}

/// A rendered note page and its version, a hash clients compare to know they are current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub html: String,
    pub version: String,
}

/// Renders one note to a complete HTML page.
pub fn render_note(title: &str, markdown: &str) -> Page {
    let body = markdown_to_html(markdown, &options());
    let mut escaped = String::with_capacity(title.len());
    // Writing into a String cannot fail.
    let _ = comrak::html::escape(&mut escaped, title);
    let mut hasher = DefaultHasher::new();
    (&escaped, &body).hash(&mut hasher);
    let version = format!("{:016x}", hasher.finish());
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"tachylite-version\" content=\"{version}\">\n\
         <title>{escaped}</title>\n\
         <script src=\"/.tachylite/live.js\" defer></script>\n</head>\n\
         <body>\n<main>\n{body}</main>\n</body>\n</html>\n"
    );
    Page { html, version }
}
