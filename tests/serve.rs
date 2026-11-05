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
        "/",
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
