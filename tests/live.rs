#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;
use std::time::Duration;

use common::{Dirs, Server, version_of};

fn with_notes(notes: &[(&str, &str)]) -> (Dirs, Server) {
    let d = Dirs::empty();
    for (path, body) in notes {
        let path = d.vault().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    let server = Server::start(&d.vault(), &d.out());
    (d, server)
}

fn version(server: &Server, url: &str) -> String {
    let r = server.get(url);
    assert_eq!(r.status, 200, "{url}");
    version_of(&r.text())
}

#[test]
fn edit_pushes_new_html() {
    let (d, server) = with_notes(&[("a.md", "# first")]);
    let mut events = server.subscribe("/a", &version(&server, "/a"), None);
    fs::write(d.vault().join("a.md"), "# second").unwrap();
    let event = events.expect("page");
    assert!(event.data.contains("<h1>second</h1>"), "{}", event.data);
    assert_eq!(event.id.as_deref(), Some(version_of(&event.data).as_str()));
    assert_eq!(server.get("/a").text(), event.data);
}

#[test]
fn atomic_rename_save_pushes_final_content() {
    let (d, server) = with_notes(&[("n.md", "# old")]);
    let mut events = server.subscribe("/n", &version(&server, "/n"), None);
    fs::write(d.vault().join(".n.md.tmp"), "# final").unwrap();
    fs::rename(d.vault().join(".n.md.tmp"), d.vault().join("n.md")).unwrap();
    let event = events.expect("page");
    assert!(event.data.contains("<h1>final</h1>"));
}

#[test]
fn create_pushes_to_a_waiting_subscriber() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/new", "", None);
    fs::write(d.vault().join("new.md"), "# fresh").unwrap();
    let event = events.expect("page");
    assert!(event.data.contains("<h1>fresh</h1>"));
}

#[test]
fn delete_pushes_gone_and_removes_output() {
    let (d, server) = with_notes(&[("a.md", "# a"), ("d/b.md", "# b")]);
    let mut events = server.subscribe("/d/b", &version(&server, "/d/b"), None);
    fs::remove_file(d.vault().join("d/b.md")).unwrap();
    events.expect("gone");
    assert_eq!(server.get("/d/b").status, 404);
    assert!(
        !d.out().join("pages/d").exists(),
        "empty folder left behind"
    );
}

#[test]
fn rename_note_moves_page() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut old = server.subscribe("/a", &version(&server, "/a"), None);
    let mut new = server.subscribe("/b", "", None);
    fs::rename(d.vault().join("a.md"), d.vault().join("b.md")).unwrap();
    old.expect("gone");
    new.expect("page");
    assert_eq!(server.get("/b").status, 200);
}

#[test]
fn new_folder_with_notes_is_published() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/d/e/n", "", None);
    fs::create_dir_all(d.vault().join("d/e")).unwrap();
    fs::write(d.vault().join("d/e/n.md"), "# deep").unwrap();
    let event = events.expect("page");
    assert!(event.data.contains("<h1>deep</h1>"));
    fs::write(d.vault().join("d/e/n.md"), "# deeper").unwrap();
    assert!(events.expect("page").data.contains("<h1>deeper</h1>"));
}

#[test]
fn folder_moved_in_from_outside_is_published() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let outside = d.tmp.path().join("outside");
    fs::create_dir_all(outside.join("sub")).unwrap();
    fs::write(outside.join("sub/m.md"), "# moved").unwrap();
    let mut events = server.subscribe("/m/sub/m", "", None);
    fs::rename(&outside, d.vault().join("m")).unwrap();
    assert!(events.expect("page").data.contains("<h1>moved</h1>"));
}

#[test]
fn rename_folder_moves_its_pages() {
    let (d, server) = with_notes(&[("d/n.md", "# n"), ("d/e/m.md", "# m")]);
    let mut old = server.subscribe("/d/e/m", &version(&server, "/d/e/m"), None);
    let mut new = server.subscribe("/f/e/m", "", None);
    fs::rename(d.vault().join("d"), d.vault().join("f")).unwrap();
    old.expect("gone");
    new.expect("page");
    assert_eq!(server.get("/f/n").status, 200);
    assert_eq!(server.get("/d/n").status, 404);
    // The moved folder is still watched under its new name.
    fs::write(d.vault().join("f/e/m.md"), "# after").unwrap();
    assert!(new.expect("page").data.contains("<h1>after</h1>"));
}

#[test]
fn unchanged_save_pushes_nothing() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/a", &version(&server, "/a"), None);
    fs::write(d.vault().join("a.md"), "# a").unwrap();
    fs::write(d.vault().join("a.md"), "# changed").unwrap();
    // Events on one stream arrive in order, so the first is the real change.
    assert!(events.expect("page").data.contains("<h1>changed</h1>"));
}

#[test]
fn excluded_edit_publishes_nothing() {
    let (d, server) = with_notes(&[("a.md", "# a"), (".obsidian/x.md", "# x")]);
    let mut barrier = server.subscribe("/a", &version(&server, "/a"), None);
    fs::write(d.vault().join(".obsidian/x.md"), "# edited").unwrap();
    fs::create_dir_all(d.vault().join("node_modules/p")).unwrap();
    fs::write(d.vault().join("node_modules/p/r.md"), "# r").unwrap();
    fs::write(d.vault().join("a.md"), "# barrier").unwrap();
    barrier.expect("page");
    assert!(!d.out().join("pages/.obsidian").exists());
    assert!(!d.out().join("pages/node_modules").exists());
}

#[test]
fn symlink_swap_is_unpublished() {
    let (d, server) = with_notes(&[("s.md", "# s")]);
    let outside = d.tmp.path().join("secret.md");
    fs::write(&outside, "# outside secret").unwrap();
    let mut events = server.subscribe("/s", &version(&server, "/s"), None);
    symlink(&outside, d.vault().join(".s.link")).unwrap();
    fs::rename(d.vault().join(".s.link"), d.vault().join("s.md")).unwrap();
    events.expect("gone");
    assert!(
        server
            .wait_line(|l| l == "skipped s.md: symlink", Duration::from_secs(10))
            .is_some()
    );
    assert_eq!(server.get("/s").status, 404);
}

#[test]
fn fifo_swap_does_not_hang() {
    let (d, server) = with_notes(&[("f.md", "# f"), ("g.md", "# g")]);
    let mut events = server.subscribe("/f", &version(&server, "/f"), None);
    let fifo = d.vault().join(".f.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    fs::rename(&fifo, d.vault().join("f.md")).unwrap();
    events.expect("gone");
    let mut g = server.subscribe("/g", &version(&server, "/g"), None);
    fs::write(d.vault().join("g.md"), "# still alive").unwrap();
    g.expect("page");
}

#[test]
fn stale_version_gets_current_page_on_connect() {
    let (_d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/a", "0000000000000000", None);
    let event = events.expect("page");
    assert_eq!(event.data, server.get("/a").text());
}

#[test]
fn deleted_page_gets_gone_on_connect() {
    let (_d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/missing", "0000000000000000", None);
    events.expect("gone");
}

#[test]
fn last_event_id_overrides_v() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let current = version(&server, "/a");
    let mut events = server.subscribe("/a", "0000000000000000", Some(&current));
    fs::write(d.vault().join("a.md"), "# next").unwrap();
    assert!(events.expect("page").data.contains("<h1>next</h1>"));
}

#[test]
fn reserved_characters_get_their_own_pushes() {
    let (d, server) = with_notes(&[("C# notes.md", "# c"), ("50% off.md", "# p")]);
    let mut c = server.subscribe("/C%23%20notes", &version(&server, "/C%23%20notes"), None);
    fs::write(d.vault().join("C# notes.md"), "# sharp").unwrap();
    assert!(c.expect("page").data.contains("<h1>sharp</h1>"));
}

#[test]
fn vault_root_removed_exits_with_error() {
    let (d, mut server) = with_notes(&[("a.md", "# a")]);
    fs::rename(d.vault(), d.tmp.path().join("moved")).unwrap();
    let line = server
        .wait_line(|l| l.starts_with("error:"), Duration::from_secs(10))
        .expect("no error line");
    assert!(line.contains("vault"), "{line}");
    let status = server.child.wait().unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn half_written_new_note_is_not_pushed() {
    use std::io::Write;
    let (d, server) = with_notes(&[("w.md", "# w"), ("bar.md", "# b0")]);
    let mut events = server.subscribe("/w", &version(&server, "/w"), None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    // Vim's default save: rename to a backup, then create, write and close.
    fs::rename(d.vault().join("w.md"), d.vault().join("w.md~")).unwrap();
    events.expect("gone");
    let mut file = fs::File::create(d.vault().join("w.md")).unwrap();
    file.write_all(b"# half").unwrap();
    // The barrier's push means the watcher has handled the CREATE before it.
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    barrier.expect("page");
    file.write_all(b"\n\nrest of the note").unwrap();
    drop(file);
    let event = events.expect("page");
    assert!(event.data.contains("rest of the note"), "{}", event.data);
}

#[test]
fn hard_link_is_published() {
    let (d, server) = with_notes(&[("a.md", "# linked")]);
    let mut events = server.subscribe("/b", "", None);
    fs::hard_link(d.vault().join("a.md"), d.vault().join("b.md")).unwrap();
    let event = events.expect("page");
    assert!(event.data.contains("<h1>linked</h1>"));
}

#[test]
fn delete_then_create_saves_never_push_half_a_note() {
    use std::io::Write;
    let (d, server) = with_notes(&[("w.md", "# w"), ("bar.md", "# b")]);
    let mut events = server.subscribe("/w", &version(&server, "/w"), None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    // Each round is one save in the style of vim (rename) or git (unlink). Without a pause,
    // the removal and the half-written file often share a watcher batch.
    for round in 0..20 {
        let note = d.vault().join("w.md");
        if round % 2 == 0 {
            fs::rename(&note, d.vault().join("w.md~")).unwrap();
        } else {
            fs::remove_file(&note).unwrap();
        }
        let mut file = fs::File::create(&note).unwrap();
        file.write_all(format!("# round {round}\n\nhalf").as_bytes())
            .unwrap();
        fs::write(d.vault().join("bar.md"), format!("# b{round}")).unwrap();
        barrier.expect("page");
        file.write_all(b" and the rest").unwrap();
        drop(file);
        loop {
            let event = events
                .next(Duration::from_secs(10))
                .unwrap_or_else(|| panic!("no page in round {round}"));
            if event.name == "gone" {
                continue;
            }
            assert!(
                event
                    .data
                    .contains(&format!("round {round}</h1>\n<p>half and the rest")),
                "round {round}: {}",
                event.data
            );
            break;
        }
    }
}
