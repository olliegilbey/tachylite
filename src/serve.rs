//! The HTTP edge: serves the output directory on 127.0.0.1.

use std::collections::HashMap;
use std::convert::Infallible;
use std::fs;
use std::future::IntoFuture;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::future::{self, Either};
use futures_util::stream;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::oneshot;

use crate::site::Push;
use crate::watch::{Live, Watcher};
use crate::{Error, output};

pub const DEFAULT_PORT: u16 = 7420;
const LIVE_JS: &str = include_str!("live.js");

/// Percent-decodes a URL path into a vault-style path, or `None` for a path no page can have:
/// invalid escapes or UTF-8, NUL, empty, `.` or `..` segments, a trailing `/`, or a segment
/// starting with `.` other than a leading `.tachylite`. `/` decodes to `""`.
pub fn decode(path: &str) -> Option<String> {
    let decoded = percent_decode(path.strip_prefix('/')?)?;
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

/// Percent-decodes to UTF-8, or `None` for a bad escape or invalid UTF-8. `+` stays `+`.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut iter = text.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hi = (iter.next()? as char).to_digit(16)?;
            let lo = (iter.next()? as char).to_digit(16)?;
            bytes.push(u8::try_from(hi * 16 + lo).ok()?);
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).ok()
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

/// A hash of the bytes, so two saves within one mtime tick still differ.
fn etag(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("W/\"{:x}-{:016x}\"", bytes.len(), hasher.finish())
}

fn matches(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim() == tag || t.trim() == "*")
}

/// What every handler shares.
pub struct App {
    pub out: PathBuf,
    pub live: Live,
}

async fn file(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(path) = decode(uri.path()).and_then(|key| locate(&app.out, &key)) else {
        return status(StatusCode::NOT_FOUND);
    };
    // Small files from the page cache; slice 6 owns streaming, compression and caching.
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return status(StatusCode::NOT_FOUND),
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let tag = etag(&bytes);
    let mut response = if matches(&headers, &tag) {
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

/// The query parameter `name`, percent-decoded.
fn query(uri: &Uri, name: &str) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == name)
        .and_then(|(_, v)| percent_decode(v))
}

struct Stream {
    app: Arc<App>,
    key: String,
    rx: broadcast::Receiver<Arc<Push>>,
    /// The version the client shows; `None` when it shows nothing.
    client: Option<String>,
    check: bool,
}

fn page_event(version: &str, html: &str) -> Event {
    Event::default()
        .event("page")
        .id(version)
        .data(html.replace("\r\n", "\n"))
}

fn gone_event() -> Event {
    // An event with empty data is never dispatched, so `gone` carries a word.
    Event::default().event("gone").data("gone")
}

impl Stream {
    /// The event that brings the client to the current page, if it is behind.
    fn catch_up(&mut self) -> Option<Event> {
        let current = self
            .app
            .live
            .versions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&self.key)
            .cloned();
        match current {
            None if self.client.is_some() => {
                self.client = None;
                Some(gone_event())
            }
            None => None,
            Some(version) if self.client.as_deref() == Some(version.as_str()) => None,
            Some(version) => {
                let path = self
                    .app
                    .out
                    .join("pages")
                    .join(format!("{}.html", self.key));
                let html = fs::read_to_string(path).ok()?;
                self.client = Some(version.clone());
                Some(page_event(&version, &html))
            }
        }
    }

    async fn next(mut self) -> Option<(Result<Event, Infallible>, Stream)> {
        loop {
            if self.check {
                self.check = false;
                if let Some(event) = self.catch_up() {
                    return Some((Ok(event), self));
                }
            }
            match self.rx.recv().await {
                Ok(push) => match &*push {
                    Push::Page { key, version, html } if *key == self.key => {
                        self.client = Some(version.clone());
                        return Some((Ok(page_event(version, html)), self));
                    }
                    Push::Gone { key } if *key == self.key => {
                        self.client = None;
                        return Some((Ok(gone_event()), self));
                    }
                    _ => {}
                },
                Err(RecvError::Lagged(_)) => self.check = true,
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

async fn events(State(app): State<Arc<App>>, uri: Uri, headers: HeaderMap) -> Response {
    let Some(key) = query(&uri, "page").and_then(|page| decode(&page)) else {
        return status(StatusCode::NOT_FOUND);
    };
    let client = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| query(&uri, "v"))
        .filter(|v| !v.is_empty());
    // Subscribe before the first check, so no push falls between them.
    let rx = app.live.pushes.subscribe();
    let state = Stream {
        app,
        key,
        rx,
        client,
        check: true,
    };
    Sse::new(stream::unfold(state, Stream::next))
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/.tachylite/live.js", get(live_js))
        .route("/.tachylite/events", get(events))
        .fallback(file)
        .with_state(Arc::new(app))
}

/// Builds the output, watches the vault, and serves on `127.0.0.1:port` until killed or
/// until the watcher fails.
pub fn serve(vault: &Path, out: &Path, port: u16) -> Result<(), Error> {
    let (vault, out) = crate::build::open(vault, out)?;
    output::prepare(&out)?;
    let (pushes, _) = broadcast::channel(256);
    let live = Live {
        pushes,
        versions: Arc::new(Mutex::new(HashMap::new())),
    };
    let config = crate::config::load(&vault)?;
    let watcher = Watcher::start(vault, out.clone(), live.clone(), config)?;
    let (failed_tx, failed_rx) = oneshot::channel();
    std::thread::spawn(move || {
        // The receiver is gone only if the server already stopped.
        let _ = failed_tx.send(watcher.run());
    });
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
        let server = axum::serve(listener, router(App { out, live })).into_future();
        match future::select(pin!(server), failed_rx).await {
            Either::Left((served, _)) => served.map_err(|source| Error::Bind { port, source }),
            Either::Right((Ok(error), _)) => Err(error),
            // Unreachable in release: a panic aborts the process (`panic = "abort"`).
            Either::Right((Err(_), _)) => Ok(()),
        }
    })
}
