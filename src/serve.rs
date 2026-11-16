//! The HTTP edge: serves the output directory on 127.0.0.1.

use std::collections::HashMap;
use std::convert::Infallible;
use std::fs;
use std::future::IntoFuture;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::future::{self, Either};
use futures_util::stream;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::oneshot;

use crate::site::Push;
use crate::watch::{Live, Watcher};
use crate::{Error, output, report};

pub const DEFAULT_PORT: u16 = 7420;
const LIVE_JS: &str = include_str!("live.js");
const SW_JS: &str = include_str!("sw.js");
const APP_MANIFEST: &str = include_str!("app.webmanifest");
const ICON_SVG: &str = include_str!("icon.svg");
const ICON_MASKABLE_SVG: &str = include_str!("icon-maskable.svg");
const STYLE_CSS: &str = include_str!("style.css");
/// `@mermaid-js/tiny`, gzipped by `make mermaid`. It is served as stored, and inflated only
/// for a client that refuses gzip.
const MERMAID_JS_GZ: &[u8] = include_bytes!("vendor/mermaid.tiny.js.gz");

/// Percent-decodes a URL path into a vault-style path, or `None` for a path no page can have:
/// invalid escapes or UTF-8, NUL, empty, `.` or `..` segments, a trailing `/`, or a segment
/// starting with `.` other than a leading `.tachylite`. `/` decodes to `""`. The home page's
/// own file has no URL but `/`, so a page has one URL and one stream.
pub fn decode(path: &str) -> Option<String> {
    let decoded = percent_decode(path.strip_prefix('/')?)?;
    if decoded.is_empty() {
        return Some(decoded);
    }
    if decoded == ".tachylite/home" {
        return None;
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
    if key.starts_with(".tachylite/folders/") {
        let index = out.join("pages").join(key).join(".index.html");
        return index.is_file().then_some(index);
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
        // Generated only: the vault publishes no json.
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
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

/// Text that gzip shrinks. Images and pdfs are compressed already.
fn compressible(kind: &str) -> bool {
    kind.starts_with("text/") || kind.ends_with("json") || kind == "image/svg+xml"
}

/// Whether `Accept-Encoding` lists gzip with a non-zero weight.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|item| {
            let mut parts = item.split(';').map(str::trim);
            let coding = parts.next().unwrap_or_default();
            let refused = parts.any(|p| {
                p.strip_prefix("q=")
                    .and_then(|q| q.parse::<f32>().ok())
                    .is_some_and(|q| q <= 0.0)
            });
            (coding.eq_ignore_ascii_case("gzip") || coding == "*") && !refused
        })
}

/// gzip at the default level: about 0.2 ms for a typical page.
fn gzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(
        Vec::with_capacity(bytes.len() / 2),
        flate2::Compression::default(),
    );
    encoder.write_all(bytes)?;
    encoder.finish()
}

fn status(code: StatusCode) -> Response {
    (code, code.canonical_reason().unwrap_or_default()).into_response()
}

/// A 304 carries no body and no content type.
fn not_modified() -> Response {
    StatusCode::NOT_MODIFIED.into_response()
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
    /// The names answered besides `127.0.0.1` and `localhost`. `None` answers any `Host`.
    pub hosts: Option<Vec<String>>,
}

/// The main origin's policy: no inline script, and no requests off the origin but images.
/// Mermaid injects `<style>`, so styles may be inline.
const MAIN_CSP: &str = "default-src 'self'; img-src 'self' data: https:; \
                        style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; \
                        form-action 'none'";

/// A vault file opened as a page runs in an opaque origin: an html page, and an svg, which
/// can hold script. A pdf keeps the main policy, since a browser's viewer needs the origin.
const SANDBOX_CSP: &str = "sandbox allow-scripts";

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
    if !path.starts_with(app.out.join("files")) {
        return match fs::read(&path) {
            Ok(bytes) => respond(bytes, content_type(&path), &headers),
            Err(e) => io_status(&e),
        };
    }
    let mut response = vault_file(&path, &headers).await;
    if content_type(&path) != "application/pdf" {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(SANDBOX_CSP),
        );
    }
    response
}

fn io_status(e: &io::Error) -> Response {
    match e.kind() {
        io::ErrorKind::NotFound => status(StatusCode::NOT_FOUND),
        _ => status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Text above this size streams as stored rather than being gzipped in memory.
const GZIP_MAX: u64 = 256 * 1024;
const CHUNK: usize = 128 * 1024;

/// An html page or an asset, copied from the vault. Its tag comes from the file's size and
/// mtime, which the copy takes from its source, so a 304 reads no bytes and the tag holds
/// across restarts. The body is read off the runtime thread.
async fn vault_file(path: &Path, headers: &HeaderMap) -> Response {
    let opened = fs::File::open(path).and_then(|f| f.metadata().map(|m| (f, m)));
    let (mut file, meta) = match opened {
        Ok(opened) => opened,
        Err(e) => return io_status(&e),
    };
    let tag = format!(
        "W/\"{:x}-{:x}.{:x}\"",
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec()
    );
    let kind = content_type(path);
    let text = compressible(kind);
    let response = if matches(headers, &tag) {
        not_modified()
    } else if text && meta.len() <= GZIP_MAX && accepts_gzip(headers) {
        let zipped = blocking(move || {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            gzip(&bytes)
        });
        match zipped.await {
            Ok(zipped) => gzipped(Body::from(zipped)),
            Err(e) => return io_status(&e),
        }
    } else {
        let mut r = Response::new(chunks(file));
        r.headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len()));
        r
    };
    tagged(response, kind, &tag)
}

/// Runs `work` on the blocking pool, so it never delays a push.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}

/// The file's bytes, read off the runtime thread a chunk at a time, so a large asset holds
/// no more than a chunk and never stalls a push.
fn chunks(file: fs::File) -> Body {
    Body::from_stream(stream::try_unfold(file, |mut file| async move {
        let (file, chunk) = blocking(move || {
            let mut chunk = vec![0; CHUNK];
            let n = loop {
                match file.read(&mut chunk) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    read => break read?,
                }
            };
            chunk.truncate(n);
            Ok((file, chunk))
        })
        .await?;
        Ok::<_, io::Error>((!chunk.is_empty()).then(|| (Bytes::from(chunk), file)))
    }))
}

fn gzipped(body: Body) -> Response {
    let mut r = Response::new(body);
    r.headers_mut()
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    r
}

/// The response for `bytes`: a 304 when the client holds them, gzipped when it takes gzip.
fn respond(bytes: Vec<u8>, kind: &'static str, headers: &HeaderMap) -> Response {
    let tag = etag(&bytes);
    let response = if matches(headers, &tag) {
        not_modified()
    } else {
        match (compressible(kind) && accepts_gzip(headers)).then(|| gzip(&bytes)) {
            Some(Ok(zipped)) => gzipped(Body::from(zipped)),
            _ => Response::new(Body::from(bytes)),
        }
    };
    tagged(response, kind, &tag)
}

/// Adds the validator and caching headers, and the content type unless it is a 304.
fn tagged(mut response: Response, kind: &'static str, tag: &str) -> Response {
    let ok = response.status() == StatusCode::OK;
    let h = response.headers_mut();
    if ok {
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    }
    if compressible(kind) {
        h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if let Ok(value) = HeaderValue::from_str(tag) {
        h.insert(header::ETAG, value);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// The router-wide layer: refuses an unknown `Host`, then adds the headers every response
/// carries, so no handler can skip them.
async fn edge(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let mut response = if known_host(app.hosts.as_deref(), request.headers()) {
        next.run(request).await
    } else {
        status(StatusCode::MISDIRECTED_REQUEST)
    };
    let h = response.headers_mut();
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(MAIN_CSP),
        );
    }
    // Without it, a vault file served as html or pdf could load as a same-origin script.
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // A remote image in a note would otherwise tell its host the site's address.
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// True when `hosts` is unset, or `Host` names the loopback or a listed name, with or
/// without a port, in any case. A rebound DNS name fails, so a hostile page cannot read
/// the site as same-origin.
fn known_host(hosts: Option<&[String]>, headers: &HeaderMap) -> bool {
    let Some(hosts) = hosts else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let name = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    ["127.0.0.1", "localhost"]
        .into_iter()
        .chain(hosts.iter().map(String::as_str))
        .any(|h| h.eq_ignore_ascii_case(name))
}

/// A client file built into the binary.
fn embedded(text: &'static str, kind: &'static str, headers: &HeaderMap) -> Response {
    respond(text.as_bytes().to_vec(), kind, headers)
}

async fn live_js(headers: HeaderMap) -> Response {
    embedded(LIVE_JS, "text/javascript", &headers)
}

/// The worker lives under `/.tachylite/`, so it may control the whole site only with this
/// header.
async fn sw_js(headers: HeaderMap) -> Response {
    let mut r = embedded(SW_JS, "text/javascript", &headers);
    r.headers_mut().insert(
        HeaderName::from_static("service-worker-allowed"),
        HeaderValue::from_static("/"),
    );
    r
}

async fn app_manifest(headers: HeaderMap) -> Response {
    embedded(APP_MANIFEST, "application/manifest+json", &headers)
}

async fn icon_svg(headers: HeaderMap) -> Response {
    embedded(ICON_SVG, "image/svg+xml", &headers)
}

async fn icon_maskable_svg(headers: HeaderMap) -> Response {
    embedded(ICON_MASKABLE_SVG, "image/svg+xml", &headers)
}

async fn style_css(headers: HeaderMap) -> Response {
    embedded(STYLE_CSS, "text/css", &headers)
}

async fn mermaid_js(headers: HeaderMap) -> Response {
    static TAG: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| etag(MERMAID_JS_GZ));
    let response = if matches(&headers, &TAG) {
        not_modified()
    } else if accepts_gzip(&headers) {
        gzipped(Body::from(MERMAID_JS_GZ))
    } else {
        // Every browser takes gzip; this serves a client that refuses it.
        let plain = blocking(|| {
            let mut plain = Vec::new();
            flate2::read::GzDecoder::new(MERMAID_JS_GZ).read_to_end(&mut plain)?;
            Ok(plain)
        });
        match plain.await {
            Ok(plain) => Response::new(Body::from(plain)),
            Err(e) => return io_status(&e),
        }
    };
    tagged(response, "text/javascript", &TAG)
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
    // An event with empty data is never dispatched, so `gone` carries a word. Its id is no
    // version, so a reconnect's Last-Event-ID gets the page if it is back unchanged.
    Event::default().event("gone").id("gone").data("gone")
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
                let html = fs::read_to_string(locate(&self.app.out, &self.key)?).ok()?;
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
                Err(RecvError::Lagged(_)) => {
                    // Skip the retained backlog: the check sends the current page, and any
                    // push still queued would be older than it.
                    self.rx = self.rx.resubscribe();
                    self.check = true;
                }
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
    let app = Arc::new(app);
    Router::new()
        .route("/.tachylite/live.js", get(live_js))
        .route("/.tachylite/sw.js", get(sw_js))
        .route("/.tachylite/app.webmanifest", get(app_manifest))
        .route("/.tachylite/icon.svg", get(icon_svg))
        .route("/.tachylite/icon-maskable.svg", get(icon_maskable_svg))
        .route("/.tachylite/style.css", get(style_css))
        .route("/.tachylite/mermaid.js", get(mermaid_js))
        .route("/.tachylite/events", get(events))
        .fallback(file)
        .layer(middleware::from_fn_with_state(app.clone(), edge))
        .with_state(app)
}

/// Builds the output, watches the vault, and serves on `127.0.0.1:port` until killed or
/// until the watcher fails. The config, the port and the output lock are all taken before
/// the output is touched, so a start that fails leaves it as it was.
pub fn serve(vault: &Path, out: &Path, port: u16) -> Result<(), Error> {
    let (vault, out) = crate::build::open(vault, out)?;
    let config = crate::config::load(&vault)?;
    let listener = std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .and_then(|l| l.set_nonblocking(true).map(|()| l))
        .map_err(|source| Error::Bind { port, source })?;
    let _lock = crate::build::lock(&out)?;
    output::prepare(&out)?;
    let (pushes, _) = broadcast::channel(256);
    let live = Live {
        pushes,
        versions: Arc::new(Mutex::new(HashMap::new())),
    };
    let exclude = config.exclude.clone();
    let hosts = config.hosts.clone();
    let watcher = Watcher::start(vault, out.clone(), live.clone(), config)?;
    report::unmatched(&exclude.unmatched());
    let (failed_tx, failed_rx) = oneshot::channel();
    std::thread::spawn(move || {
        // The receiver is gone only if the server already stopped.
        let _ = failed_tx.send(watcher.run());
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)
            .map_err(|source| Error::Bind { port, source })?;
        let addr = listener
            .local_addr()
            .map_err(|source| Error::Bind { port, source })?;
        eprintln!("serving http://{addr}");
        let app = App { out, live, hosts };
        let server = axum::serve(listener, router(app)).into_future();
        match future::select(pin!(server), failed_rx).await {
            Either::Left((served, _)) => served.map_err(Error::Serve),
            Either::Right((Ok(error), _)) => Err(error),
            // Unreachable in release: a panic aborts the process (`panic = "abort"`).
            Either::Right((Err(_), _)) => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use futures_util::FutureExt as _;

    use super::*;

    fn page(version: &str) -> Arc<Push> {
        Arc::new(Push::Page {
            key: "n".into(),
            version: version.into(),
            html: format!("<p>{version}</p>").into(),
        })
    }

    #[test]
    fn a_lagged_subscriber_gets_the_current_page_and_no_stale_push() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("pages")).unwrap();
        fs::write(dir.path().join("pages/n.html"), "<p>v9</p>").unwrap();
        let (pushes, _) = broadcast::channel(4);
        let live = Live {
            pushes,
            versions: Arc::new(Mutex::new(HashMap::from([("n".into(), "v9".into())]))),
        };
        let rx = live.pushes.subscribe();
        // Ten pushes into four slots: the stream lags, and v6 to v9 stay queued.
        for i in 0..10 {
            live.pushes.send(page(&format!("v{i}"))).unwrap();
        }
        let app = Arc::new(App {
            out: dir.path().to_path_buf(),
            live: live.clone(),
            hosts: None,
        });
        let stream = Stream {
            app,
            key: "n".into(),
            rx,
            client: Some("v0".into()),
            check: false,
        };
        let (event, stream) = stream.next().now_or_never().unwrap().unwrap();
        let text = format!("{:?}", event.unwrap());
        assert!(text.contains("v9"), "{text}");
        assert!(
            stream.next().now_or_never().is_none(),
            "a stale push followed the current page"
        );
    }
}
