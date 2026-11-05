//! The HTTP edge: serves the output directory on 127.0.0.1.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::Error;

pub const DEFAULT_PORT: u16 = 7420;
const LIVE_JS: &str = include_str!("live.js");

/// Percent-decodes a URL path into a vault-style path, or `None` for a path no page can have:
/// invalid escapes or UTF-8, NUL, empty, `.` or `..` segments, a trailing `/`, or a segment
/// starting with `.` other than a leading `.tachylite`. `/` decodes to `""`.
pub fn decode(path: &str) -> Option<String> {
    let rest = path.strip_prefix('/')?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut iter = rest.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hi = (iter.next()? as char).to_digit(16)?;
            let lo = (iter.next()? as char).to_digit(16)?;
            bytes.push(u8::try_from(hi * 16 + lo).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    if decoded.is_empty() {
        return Some(decoded);
    }
    for (i, segment) in decoded.split('/').enumerate() {
        let bad = segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\0')
            || (segment.starts_with('.') && !(i == 0 && segment == ".tachylite"));
        if bad {
            return None;
        }
    }
    Some(decoded)
}

/// The output file a decoded path serves, by the roadmap's routing rule.
fn locate(out: &Path, key: &str) -> Option<PathBuf> {
    if key.is_empty() {
        let home = out.join("pages/.tachylite/home.html");
        return home.is_file().then_some(home);
    }
    let page = out.join("pages").join(format!("{key}.html"));
    if page.is_file() {
        return Some(page);
    }
    let file = out.join("files").join(key);
    file.is_file().then_some(file)
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn status(code: StatusCode) -> Response {
    (code, code.canonical_reason().unwrap_or_default()).into_response()
}

fn etag(meta: &fs::Metadata) -> Option<String> {
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!("W/\"{:x}-{:x}\"", mtime.as_nanos(), meta.len()))
}

fn matches(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim() == tag || t.trim() == "*")
}

async fn file(
    State(out): State<Arc<PathBuf>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(path) = decode(uri.path()).and_then(|key| locate(&out, &key)) else {
        return status(StatusCode::NOT_FOUND);
    };
    // Small files from the page cache; slice 6 owns streaming, compression and caching.
    let (meta, bytes) = match fs::metadata(&path).and_then(|m| Ok((m, fs::read(&path)?))) {
        Ok(found) => found,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return status(StatusCode::NOT_FOUND),
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let tag = etag(&meta).unwrap_or_default();
    let mut response = if !tag.is_empty() && matches(&headers, &tag) {
        status(StatusCode::NOT_MODIFIED)
    } else {
        let mut r = Response::new(Body::from(bytes));
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(content_type(&path)),
        );
        r
    };
    if response.status() == StatusCode::NOT_MODIFIED {
        *response.body_mut() = Body::empty();
    }
    let h = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&tag) {
        h.insert(header::ETAG, value);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

async fn live_js() -> Response {
    let mut r = Response::new(Body::from(LIVE_JS));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

pub fn router(out: PathBuf) -> Router {
    Router::new()
        .route("/.tachylite/live.js", get(live_js))
        .fallback(file)
        .with_state(Arc::new(out))
}

/// Builds the output, then serves it on `127.0.0.1:port` until the process is killed.
pub fn serve(vault: &Path, out: &Path, port: u16) -> Result<(), Error> {
    let summary = crate::build::build(vault, out)?;
    crate::report::skipped(&summary.skipped);
    let (_, out) = crate::build::open(vault, out)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| Error::Io {
            path: PathBuf::from("tokio runtime"),
            source,
        })?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|source| Error::Bind { port, source })?;
        let addr = listener
            .local_addr()
            .map_err(|source| Error::Bind { port, source })?;
        eprintln!("serving http://{addr}");
        axum::serve(listener, router(out))
            .await
            .map_err(|source| Error::Bind { port, source })
    })
}
