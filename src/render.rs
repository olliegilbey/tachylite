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

/// Renders one note to a complete HTML page.
pub fn render_note(title: &str, markdown: &str) -> String {
    let body = markdown_to_html(markdown, &options());
    let mut escaped = String::with_capacity(title.len());
    // Writing into a String cannot fail.
    let _ = comrak::html::escape(&mut escaped, title);
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{escaped}</title>\n</head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n"
    )
}
