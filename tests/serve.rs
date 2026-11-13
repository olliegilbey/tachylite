#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;

use common::{Dirs, Server, get, version_of};

#[test]
fn serves_note_at_its_vault_path() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let r = server.get("/people/Jane%20Doe");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
    assert!(r.text().contains("<title>Jane Doe</title>"));
    assert_eq!(r.header("cache-control"), Some("no-cache"));
}

#[test]
fn serves_asset_and_html_page_with_types() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let png = server.get("/img/dot.png");
    assert_eq!(png.status, 200);
    assert_eq!(png.header("content-type"), Some("image/png"));
    assert_eq!(png.body, fs::read(d.vault().join("img/dot.png")).unwrap());
    let page = server.get("/maps/route.html");
    assert_eq!(page.status, 200);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        page.body,
        fs::read(d.vault().join("maps/route.html")).unwrap()
    );
    let pdf = server.get("/docs/guide.pdf");
    assert_eq!(pdf.header("content-type"), Some("application/pdf"));
}

#[test]
fn reserved_characters_in_names() {
    let d = Dirs::empty();
    for name in ["C# notes", "50% off", "café", "a+b"] {
        fs::write(d.vault().join(format!("{name}.md")), format!("# {name}")).unwrap();
    }
    let server = Server::start(&d.vault(), &d.out());
    for (path, title) in [
        ("/C%23%20notes", "C# notes"),
        ("/50%25%20off", "50% off"),
        ("/caf%C3%A9", "café"),
        ("/a+b", "a+b"),
    ] {
        let r = server.get(path);
        assert_eq!(r.status, 200, "{path}");
        assert!(r.text().contains(&format!("\">{title}</h1>")), "{path}");
    }
}

#[test]
fn rejects_bad_paths() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    for path in [
        "/../x",
        "/%2e%2e/welcome",
        "/a//b",
        "/people/./Jane%20Doe",
        "/people/",
        "/.obsidian/app.json",
        "/.tachylite-output",
        "/welcome.html",
        "/%ff",
        "/a%2",
        "/x%00",
    ] {
        assert_eq!(server.get(path).status, 404, "{path}");
    }
}

#[test]
fn etag_gives_304() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let first = server.get("/welcome");
    let etag = first.header("etag").unwrap().to_string();
    assert!(etag.starts_with("W/\""), "{etag}");
    let again = get(server.addr, "/welcome", &[("If-None-Match", &etag)]);
    assert_eq!(again.status, 304);
    assert!(again.body.is_empty());
    let other = get(server.addr, "/welcome", &[("If-None-Match", "W/\"0-0\"")]);
    assert_eq!(other.status, 200);
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .unwrap();
    out
}

#[test]
fn a_page_is_gzipped_when_the_client_accepts_it() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let plain = server.get("/welcome");
    assert_eq!(plain.header("content-encoding"), None);
    assert_eq!(plain.header("vary"), Some("accept-encoding"));
    let zipped = get(
        server.addr,
        "/welcome",
        &[("Accept-Encoding", "br, gzip, deflate")],
    );
    assert_eq!(zipped.status, 200);
    assert_eq!(zipped.header("content-encoding"), Some("gzip"));
    assert_eq!(zipped.header("vary"), Some("accept-encoding"));
    assert_eq!(zipped.header("content-type"), plain.header("content-type"));
    assert!(zipped.body.len() < plain.body.len());
    assert_eq!(gunzip(&zipped.body), plain.body);
}

#[test]
fn gzip_refused_by_the_client_is_not_sent() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    for accept in ["identity", "br", "gzip;q=0"] {
        let r = get(server.addr, "/welcome", &[("Accept-Encoding", accept)]);
        assert_eq!(r.header("content-encoding"), None, "{accept}");
    }
}

#[test]
fn an_image_is_never_gzipped() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let png = get(server.addr, "/img/dot.png", &[("Accept-Encoding", "gzip")]);
    assert_eq!(png.header("content-encoding"), None);
    assert_eq!(png.body, fs::read(d.vault().join("img/dot.png")).unwrap());
}

#[test]
fn the_etag_holds_across_encodings() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let plain = server.get("/welcome");
    let zipped = get(server.addr, "/welcome", &[("Accept-Encoding", "gzip")]);
    let etag = plain.header("etag").unwrap();
    assert_eq!(zipped.header("etag"), Some(etag));
    let again = get(
        server.addr,
        "/welcome",
        &[("Accept-Encoding", "gzip"), ("If-None-Match", etag)],
    );
    assert_eq!(again.status, 304);
    assert!(again.body.is_empty());
}

#[test]
fn same_length_edit_in_one_mtime_tick_changes_the_etag() {
    let d = Dirs::empty();
    fs::write(d.vault().join("n.md"), "# cat").unwrap();
    let server = Server::start(&d.vault(), &d.out());
    let page = d.out().join("pages/n.html");
    let mtime = fs::metadata(&page).unwrap().modified().unwrap();
    let before = server.get("/n");
    let etag = before.header("etag").unwrap().to_string();
    let mut events = server.subscribe("/n", &version_of(&before.text()), None);
    fs::write(d.vault().join("n.md"), "# dog").unwrap();
    events.expect("page");
    // Same length, and the same mtime: a coarse clock tick that held both saves.
    let file = fs::File::options().write(true).open(&page).unwrap();
    file.set_modified(mtime).unwrap();
    let after = get(server.addr, "/n", &[("If-None-Match", &etag)]);
    assert_eq!(after.status, 200, "a stale page was revalidated");
    assert!(after.text().contains("\">dog</h1>"));
}

#[test]
fn serves_live_script() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let r = server.get("/.tachylite/live.js");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/javascript"));
}

#[test]
fn mermaid_js_is_gzipped() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let r = get(
        server.addr,
        "/.tachylite/mermaid.js",
        &[("Accept-Encoding", "gzip")],
    );
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/javascript"));
    assert_eq!(r.header("content-encoding"), Some("gzip"));
    assert_eq!(r.header("vary"), Some("accept-encoding"));
    assert_eq!(r.header("cache-control"), Some("no-cache"));
    assert!(r.header("etag").is_some());
    let vendored = concat!(env!("CARGO_MANIFEST_DIR"), "/src/vendor/mermaid.tiny.js.gz");
    assert_eq!(r.body, fs::read(vendored).unwrap());
    let plain = server.get("/.tachylite/mermaid.js");
    assert_eq!(plain.header("content-encoding"), None);
    assert_eq!(plain.body, gunzip(&r.body));
}

#[test]
fn serves_the_home_page_and_index_pages() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    for (path, title) in [
        ("/", "<title>Tachylite</title>"),
        ("/.tachylite/recent", "<title>Recent</title>"),
        ("/.tachylite/folders/people", "<title>people</title>"),
    ] {
        let r = server.get(path);
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
        assert!(r.text().contains(title), "{path}: {}", r.text());
    }
    let home = server.get("/").text();
    assert!(
        home.contains("href=\"/people/Jane%20Doe\"")
            || home.contains("href=\"/.tachylite/folders/people\""),
        "{home}"
    );
    let notes = server.get("/.tachylite/notes.json");
    assert_eq!(notes.header("content-type"), Some("application/json"));
    assert!(
        notes
            .text()
            .contains("[\"Jane Doe\",\"/people/Jane%20Doe\""),
        "{}",
        notes.text()
    );
}

#[test]
fn serves_the_stylesheet() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let r = get(
        server.addr,
        "/.tachylite/style.css",
        &[("Accept-Encoding", "gzip")],
    );
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/css"));
    assert_eq!(r.header("content-encoding"), Some("gzip"));
    assert_eq!(r.header("cache-control"), Some("no-cache"));
    let page = server.get("/welcome").text();
    assert!(page.contains("<link rel=\"stylesheet\" href=\"/.tachylite/style.css\">"));
}

#[test]
fn binds_loopback() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    assert_eq!(server.addr.ip().to_string(), "127.0.0.1");
}

#[test]
fn refuses_out_inside_vault() {
    let d = Dirs::fixture();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .arg("serve")
        .arg(d.vault())
        .arg(d.vault().join("site"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("overlap"));
}

#[test]
fn every_vault_response_carries_a_csp() {
    let d = Dirs::fixture();
    fs::write(
        d.vault().join("img/v.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    let server = Server::start(&d.vault(), &d.out());
    let main = "default-src 'self'; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; \
                object-src 'none'; base-uri 'none'; form-action 'none'";
    // A vault file opened as a document runs in an opaque origin; a pdf viewer needs the page's.
    for (path, csp) in [
        ("/people/Jane%20Doe", main),
        ("/docs/guide.pdf", main),
        ("/nope", main),
        ("/maps/route.html", "sandbox allow-scripts"),
        ("/img/dot.png", "sandbox allow-scripts"),
        ("/img/v.svg", "sandbox allow-scripts"),
    ] {
        let r = server.get(path);
        assert_eq!(r.header("content-security-policy"), Some(csp), "{path}");
        assert_eq!(
            r.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
    }
}

#[test]
fn serves_the_manifest_as_json() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let r = server.get("/.tachylite/manifest.json");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("application/json"));
    assert!(r.text().starts_with("{\"pages\":{"), "{}", r.text());
    assert!(r.text().contains("\"/welcome\":\""), "{}", r.text());
}

#[test]
fn serves_the_service_worker_for_the_whole_site() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let r = server.get("/.tachylite/sw.js");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/javascript"));
    assert_eq!(r.header("service-worker-allowed"), Some("/"));
    assert_eq!(r.header("cache-control"), Some("no-cache"));
}

#[test]
fn serves_the_app_manifest_and_icon() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let m = server.get("/.tachylite/app.webmanifest");
    assert_eq!(m.status, 200);
    assert_eq!(m.header("content-type"), Some("application/manifest+json"));
    assert!(m.text().contains("\"start_url\": \"/\""), "{}", m.text());
    // Android draws an icon without a maskable entry on a white tile.
    assert!(
        m.text().contains("\"purpose\": \"maskable\""),
        "{}",
        m.text()
    );
    for path in ["/.tachylite/icon.svg", "/.tachylite/icon-maskable.svg"] {
        let icon = server.get(path);
        assert_eq!(icon.status, 200, "{path}");
        assert_eq!(icon.header("content-type"), Some("image/svg+xml"), "{path}");
    }
}

#[test]
fn every_page_links_the_app_manifest() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let page = server.get("/welcome").text();
    assert!(
        page.contains("<link rel=\"manifest\" href=\"/.tachylite/app.webmanifest\">"),
        "{page}"
    );
}

/// Sends one request with the given method and `Host`, and returns the status line and
/// headers. The body is not read, so it works for the event stream too.
fn head(addr: std::net::SocketAddr, method: &str, path: &str, host: &str) -> common::Response {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let split = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "closed before the headers");
        raw.extend_from_slice(&buf[..n]);
    };
    let text = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    common::Response {
        status,
        headers: lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect(),
        body: Vec::new(),
    }
}

#[test]
fn every_response_carries_the_documented_headers() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let welcome = server.get("/welcome");
    let etag = welcome.header("etag").unwrap().to_string();
    let mut cases: Vec<(&str, &str, common::Response)> = Vec::new();
    for path in [
        "/",
        "/welcome",
        "/nope",
        "/img/dot.png",
        "/docs/guide.pdf",
        "/.tachylite/manifest.json",
        "/.tachylite/live.js",
        "/.tachylite/sw.js",
        "/.tachylite/app.webmanifest",
        "/.tachylite/icon.svg",
        "/.tachylite/icon-maskable.svg",
        "/.tachylite/style.css",
        "/.tachylite/mermaid.js",
        "/.tachylite/events?page=%2Fwelcome",
        "/.tachylite/events",
    ] {
        cases.push(("GET", path, head(server.addr, "GET", path, "test")));
        cases.push(("POST", path, head(server.addr, "POST", path, "test")));
    }
    let not_modified = get(server.addr, "/welcome", &[("If-None-Match", &etag)]);
    assert_eq!(not_modified.status, 304);
    assert_eq!(
        not_modified.header("content-type"),
        None,
        "a 304 has no body"
    );
    cases.push(("GET", "/welcome 304", not_modified));
    for (method, path, r) in &cases {
        let what = format!("{method} {path} {}", r.status);
        assert!(r.header("content-security-policy").is_some(), "{what}");
        assert_eq!(
            r.header("x-content-type-options"),
            Some("nosniff"),
            "{what}"
        );
        assert_eq!(r.header("referrer-policy"), Some("no-referrer"), "{what}");
    }
    let statuses: Vec<u16> = cases.iter().map(|(_, _, r)| r.status).collect();
    for code in [200, 304, 404, 405] {
        assert!(statuses.contains(&code), "no {code} was checked");
    }
}

#[test]
fn the_home_page_has_one_url() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    assert_eq!(server.get("/").status, 200);
    assert_eq!(server.get("/.tachylite/home").status, 404);
}

#[test]
fn an_asset_revalidates_without_a_content_hash_and_changes_tag_on_a_save() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let first = server.get("/img/dot.png");
    let etag = first.header("etag").unwrap().to_string();
    let again = get(server.addr, "/img/dot.png", &[("If-None-Match", &etag)]);
    assert_eq!(again.status, 304);
    assert!(again.body.is_empty());
    assert_eq!(again.header("content-type"), None);
    // A same-size save: the copy is a new file, so the tag changes.
    let dot = d.vault().join("img/dot.png");
    let bytes = fs::read(&dot).unwrap();
    let version = version_of(&server.get("/welcome").text());
    let mut events = server.subscribe("/welcome", &version, None);
    fs::write(&dot, &bytes).unwrap();
    fs::write(d.vault().join("welcome.md"), "# Welcome back").unwrap();
    events.expect("page");
    let after = get(server.addr, "/img/dot.png", &[("If-None-Match", &etag)]);
    assert_eq!(after.status, 200, "a re-saved asset kept its tag");
    assert_eq!(after.body, bytes);
}

#[test]
fn a_large_asset_streams_whole_with_its_length() {
    let d = Dirs::fixture();
    // Not a multiple of the chunk size, so the last chunk is short.
    let big: Vec<u8> = (0..3_000_017u32).map(|i| (i % 251) as u8).collect();
    fs::write(d.vault().join("docs/big.pdf"), &big).unwrap();
    let server = Server::start(&d.vault(), &d.out());
    let r = get(server.addr, "/docs/big.pdf", &[("Accept-Encoding", "gzip")]);
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-length"), Some("3000017"));
    assert_eq!(r.header("content-encoding"), None);
    assert!(r.body == big, "the streamed body differs from the file");
}

#[test]
fn an_html_page_is_still_gzipped() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let plain = server.get("/maps/route.html");
    let zipped = get(
        server.addr,
        "/maps/route.html",
        &[("Accept-Encoding", "gzip")],
    );
    assert_eq!(zipped.header("content-encoding"), Some("gzip"));
    assert_eq!(zipped.header("vary"), Some("accept-encoding"));
    assert_eq!(gunzip(&zipped.body), plain.body);
}

#[test]
fn without_hosts_any_host_is_answered() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    assert_eq!(
        head(server.addr, "GET", "/welcome", "rebound.example").status,
        200
    );
}

#[test]
fn with_hosts_an_unknown_host_is_refused() {
    let d = Dirs::fixture();
    fs::write(
        d.vault().join("tachylite.toml"),
        "hosts = [\"box.example.ts.net\"]\n",
    )
    .unwrap();
    let server = Server::start(&d.vault(), &d.out());
    for host in [
        "box.example.ts.net",
        "BOX.example.ts.net:8443",
        "127.0.0.1",
        "127.0.0.1:7420",
        "localhost:7420",
    ] {
        assert_eq!(
            head(server.addr, "GET", "/welcome", host).status,
            200,
            "{host}"
        );
    }
    for host in [
        "rebound.example",
        "rebound.example:7420",
        "box.example.ts.net.evil",
    ] {
        let r = head(server.addr, "GET", "/welcome", host);
        assert_eq!(r.status, 421, "{host}");
        assert_eq!(
            r.header("x-content-type-options"),
            Some("nosniff"),
            "{host}"
        );
    }
    let stream = head(
        server.addr,
        "GET",
        "/.tachylite/events?page=%2Fwelcome",
        "rebound.example",
    );
    assert_eq!(stream.status, 421);
}

fn serve_once(vault: &std::path::Path, out: &std::path::Path, port: u16) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_tachylite"))
        .arg("serve")
        .arg(vault)
        .arg(out)
        .args(["--port", &port.to_string()])
        .output()
        .unwrap()
}

type Snapshot = Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)>;

/// Every file under `dir`, with its bytes and modification time.
fn snapshot(dir: &std::path::Path) -> Snapshot {
    let mut all = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for item in fs::read_dir(d).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                all.push((path.clone(), fs::read(&path).unwrap(), modified));
            }
        }
    }
    all.sort();
    all
}

#[test]
fn a_start_on_a_busy_port_leaves_the_output_untouched() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let before = snapshot(&d.out());
    let run = serve_once(&d.vault(), &d.out(), server.addr.port());
    assert_eq!(run.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("cannot serve on 127.0.0.1"), "{stderr}");
    assert_eq!(
        snapshot(&d.out()),
        before,
        "a failed start changed the output"
    );
}

#[test]
fn a_bad_config_leaves_the_output_untouched() {
    let d = Dirs::fixture();
    drop(Server::start(&d.vault(), &d.out()));
    let before = snapshot(&d.out());
    fs::write(
        d.vault().join("tachylite.toml"),
        "excludes = [\"private\"]\n",
    )
    .unwrap();
    let run = serve_once(&d.vault(), &d.out(), 0);
    assert_eq!(run.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(
        stderr,
        "error: tachylite.toml: unknown key \"excludes\"; the keys are exclude, daily, pins, title and hosts\n"
    );
    assert_eq!(snapshot(&d.out()), before);
}

#[test]
fn two_servers_never_share_an_output() {
    let d = Dirs::fixture();
    let server = Server::start(&d.vault(), &d.out());
    let before = snapshot(&d.out());
    let run = serve_once(&d.vault(), &d.out(), 0);
    assert_eq!(run.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("is in use by another tachylite"),
        "{stderr}"
    );
    assert_eq!(snapshot(&d.out()), before);
    drop(server);
}

#[test]
fn a_deleted_output_stops_the_server_with_a_clear_error() {
    let d = Dirs::fixture();
    let mut server = Server::start(&d.vault(), &d.out());
    fs::remove_dir_all(d.out()).unwrap();
    fs::write(d.vault().join("welcome.md"), "# Welcome again").unwrap();
    let line = server.wait_line(
        |l| l.starts_with("error: "),
        std::time::Duration::from_secs(10),
    );
    let line = line.expect("the server kept running without its output");
    assert!(line.contains("lost its .tachylite-output marker"), "{line}");
    assert_eq!(server.child.wait().unwrap().code(), Some(1));
    assert!(
        !d.out().join("pages").exists(),
        "the server recreated a bare output"
    );
}
