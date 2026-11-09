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
    let r = server.get("/.tachylite/mermaid.js");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("text/javascript"));
    assert_eq!(r.header("content-encoding"), Some("gzip"));
    assert_eq!(r.header("cache-control"), Some("no-cache"));
    assert!(r.header("etag").is_some());
    let vendored = concat!(env!("CARGO_MANIFEST_DIR"), "/src/vendor/mermaid.tiny.js.gz");
    assert_eq!(r.body, fs::read(vendored).unwrap());
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
    let icon = server.get("/.tachylite/icon.svg");
    assert_eq!(icon.status, 200);
    assert_eq!(icon.header("content-type"), Some("image/svg+xml"));
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
