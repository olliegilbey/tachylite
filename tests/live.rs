#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;
use std::time::Duration;

use common::{Dirs, Server, version_of};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process, waitid};

fn with_notes(notes: &[(&str, &str)]) -> (Dirs, Server) {
    with_notes_env(notes, &[])
}

fn with_notes_env(notes: &[(&str, &str)], envs: &[(&str, &str)]) -> (Dirs, Server) {
    let d = Dirs::empty();
    for (path, body) in notes {
        let path = d.vault().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    let server = Server::start_with(&d.vault(), &d.out(), envs);
    (d, server)
}

/// A hold no test outlasts, for tests that keep a note open across a barrier.
const LONG_HOLD: &[(&str, &str)] = &[("TACHYLITE_HOLD_MS", "600000")];

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
    assert!(event.data.contains("\">second</h1>"), "{}", event.data);
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
    assert!(event.data.contains("\">final</h1>"));
}

#[test]
fn create_pushes_to_a_waiting_subscriber() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/new", "", None);
    fs::write(d.vault().join("new.md"), "# fresh").unwrap();
    let event = events.expect("page");
    assert!(event.data.contains("\">fresh</h1>"));
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
    assert!(event.data.contains("\">deep</h1>"));
    fs::write(d.vault().join("d/e/n.md"), "# deeper").unwrap();
    assert!(events.expect("page").data.contains("\">deeper</h1>"));
}

#[test]
fn folder_moved_in_from_outside_is_published() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let outside = d.tmp.path().join("outside");
    fs::create_dir_all(outside.join("sub")).unwrap();
    fs::write(outside.join("sub/m.md"), "# moved").unwrap();
    let mut events = server.subscribe("/m/sub/m", "", None);
    fs::rename(&outside, d.vault().join("m")).unwrap();
    assert!(events.expect("page").data.contains("\">moved</h1>"));
}

#[test]
fn rename_folder_moves_its_pages() {
    let (d, server) = with_notes(&[("d/n.md", "# n"), ("d/e/m.md", "# m")]);
    let mut old = server.subscribe("/d/e/m", &version(&server, "/d/e/m"), None);
    let mut new = server.subscribe("/f/e/m", "", None);
    // Pages are written in path order, so `f/n` lands after `f/e/m`'s push: await its own.
    let mut moved = server.subscribe("/f/n", "", None);
    fs::rename(d.vault().join("d"), d.vault().join("f")).unwrap();
    old.expect("gone");
    new.expect("page");
    moved.expect("page");
    assert_eq!(server.get("/f/n").status, 200);
    assert_eq!(server.get("/d/n").status, 404);
    // The moved folder is still watched under its new name.
    fs::write(d.vault().join("f/e/m.md"), "# after").unwrap();
    assert!(new.expect("page").data.contains("\">after</h1>"));
}

#[test]
fn unchanged_save_pushes_nothing() {
    let (d, server) = with_notes(&[("a.md", "# a"), ("bar.md", "# b0")]);
    let mut events = server.subscribe("/a", &version(&server, "/a"), None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    fs::write(d.vault().join("a.md"), "# a").unwrap();
    // The barrier's push means the watcher has handled the unchanged save.
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    barrier.expect("page");
    fs::write(d.vault().join("a.md"), "# changed").unwrap();
    // Events on one stream arrive in order, so the first is the real change.
    assert!(events.expect("page").data.contains("\">changed</h1>"));
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
    assert!(events.expect("page").data.contains("\">next</h1>"));
}

#[test]
fn reserved_characters_get_their_own_pushes() {
    // Each name with the pathname a browser shows for it.
    let names = [
        ("C# notes", "/C%23%20notes"),
        ("50% off", "/50%25%20off"),
        ("a+b", "/a+b"),
        ("café", "/caf%C3%A9"),
    ];
    let notes: Vec<(String, String)> = names
        .iter()
        .map(|(name, _)| (format!("{name}.md"), "# before".to_string()))
        .collect();
    let notes: Vec<(&str, &str)> = notes
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_str()))
        .collect();
    let (d, server) = with_notes(&notes);
    for (i, (name, url)) in names.iter().enumerate() {
        let mut events = server.subscribe(url, &version(&server, url), None);
        fs::write(d.vault().join(format!("{name}.md")), format!("# edit {i}")).unwrap();
        let event = events.expect("page");
        assert!(event.data.contains(&format!("\">edit {i}</h1>")), "{name}");
    }
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
    let (d, server) = with_notes_env(&[("w.md", "# w"), ("bar.md", "# b0")], LONG_HOLD);
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
    assert!(event.data.contains("\">linked</h1>"));
}

#[test]
fn delete_then_create_saves_never_push_half_a_note() {
    use std::io::Write;
    let (d, server) = with_notes_env(&[("w.md", "# w"), ("bar.md", "# b")], LONG_HOLD);
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

/// The server's own child, by the PID captured at spawn.
fn pid(server: &Server) -> Pid {
    Pid::from_raw(i32::try_from(server.child.id()).unwrap()).unwrap()
}

/// Stops the server, so every event that follows lands in its next batch.
fn stop(server: &Server) {
    kill_process(pid(server), Signal::STOP).unwrap();
    // Returns once every thread has stopped, or if the child exits. NOWAIT reaps nothing.
    let status = waitid(
        WaitId::Pid(pid(server)),
        WaitIdOptions::STOPPED | WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
    )
    .unwrap()
    .unwrap();
    assert!(status.stopped(), "the server exited instead of stopping");
}

fn resume(server: &Server) {
    kill_process(pid(server), Signal::CONT).unwrap();
}

/// Stops the server and writes enough new notes to overflow its inotify queue. Returns the
/// queue limit.
fn overflow(d: &Dirs, server: &Server) -> usize {
    let limit: usize = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        limit <= 65_536,
        "max_queued_events is {limit}, too many files to write; lower it to run this test"
    );
    stop(server);
    // Each new note queues CREATE and CLOSE_WRITE, so this overflows the queue.
    for i in 0..limit / 2 + 2_000 {
        fs::write(d.vault().join(format!("n{i}.md")), "# n").unwrap();
    }
    limit
}

#[test]
fn queue_overflow_reloads_the_config() {
    let (d, server) = with_notes(&[("a.md", "# a"), ("p/x.md", "# x")]);
    let mut hidden = server.subscribe("/p/x", &version(&server, "/p/x"), None);
    overflow(&d, &server);
    // The kernel drops this save's events: the queue is already full.
    fs::write(d.vault().join("tachylite.toml"), "exclude = [\"p\"]\n").unwrap();
    resume(&server);
    hidden.expect("gone");
    assert_eq!(server.get("/p/x").status, 404);
}

#[test]
fn queue_overflow_resyncs_the_vault() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut gone = server.subscribe("/a", &version(&server, "/a"), None);
    let mut late = server.subscribe("/z", "", None);
    let limit = overflow(&d, &server);
    // These events come after the overflow, so the kernel drops them.
    fs::remove_file(d.vault().join("a.md")).unwrap();
    fs::write(d.vault().join("z.md"), "# late").unwrap();
    resume(&server);
    assert!(
        server
            .wait_line(|l| l.contains("overflowed"), Duration::from_secs(10))
            .is_some(),
        "the queue did not overflow"
    );
    gone.expect("gone");
    assert!(late.expect("page").data.contains("\">late</h1>"));
    assert_eq!(server.get(&format!("/n{}", limit / 2)).status, 200);
}

#[test]
fn excluding_a_folder_unpublishes_it() {
    let (d, server) = with_notes(&[("a.md", "# a"), ("d/n.md", "# n")]);
    let mut events = server.subscribe("/d/n", &version(&server, "/d/n"), None);
    fs::write(d.vault().join("tachylite.toml"), "exclude = [\"d\"]\n").unwrap();
    events.expect("gone");
    assert_eq!(server.get("/d/n").status, 404);
}

#[test]
fn unexcluding_republishes() {
    let (d, server) = with_notes(&[("tachylite.toml", "exclude = [\"d\"]\n"), ("d/n.md", "# n")]);
    assert_eq!(server.get("/d/n").status, 404);
    let mut events = server.subscribe("/d/n", "", None);
    fs::write(d.vault().join("tachylite.toml"), "exclude = []\n").unwrap();
    assert!(events.expect("page").data.contains("\">n</h1>"));
}

#[test]
fn bad_config_edit_keeps_serving() {
    let (d, server) = with_notes(&[("tachylite.toml", "exclude = [\"d\"]\n"), ("a.md", "# a")]);
    let mut events = server.subscribe("/a", &version(&server, "/a"), None);
    fs::write(d.vault().join("tachylite.toml"), "exclude = [\n").unwrap();
    assert!(
        server
            .wait_line(
                |l| l.contains("keeping the previous config"),
                Duration::from_secs(10)
            )
            .is_some()
    );
    fs::write(d.vault().join("a.md"), "# edited").unwrap();
    assert!(events.expect("page").data.contains("\">edited</h1>"));
}

#[test]
fn deleting_the_config_keeps_its_exclusions() {
    let (d, server) = with_notes(&[
        ("tachylite.toml", "exclude = [\"d\"]\n"),
        ("d/n.md", "# n"),
        ("bar.md", "# b0"),
    ]);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    fs::remove_file(d.vault().join("tachylite.toml")).unwrap();
    fs::write(d.vault().join("d/n.md"), "# still hidden").unwrap();
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    barrier.expect("page");
    assert_eq!(server.get("/d/n").status, 404);
}

#[test]
fn new_link_pushes_backlink_to_target() {
    let (d, server) = with_notes(&[("a.md", "# a"), ("b.md", "# b")]);
    let mut events = server.subscribe("/b", &version(&server, "/b"), None);
    fs::write(d.vault().join("a.md"), "see [[b]]").unwrap();
    let event = events.expect("page");
    assert!(
        event
            .data
            .contains("<h2>Backlinks</h2><ul><li><a href=\"/a\">a</a></li></ul>"),
        "{}",
        event.data
    );
}

#[test]
fn renamed_target_pushes_broken_link_to_linker() {
    let (d, server) = with_notes(&[("a.md", "see [[b]]"), ("b.md", "# b")]);
    let mut events = server.subscribe("/a", &version(&server, "/a"), None);
    fs::rename(d.vault().join("b.md"), d.vault().join("c.md")).unwrap();
    let event = events.expect("page");
    assert!(
        event.data.contains("<a class=\"broken-link\">b</a>"),
        "{}",
        event.data
    );
    assert!(
        server
            .wait_line(|l| l == "broken link in a.md: b", Duration::from_secs(10))
            .is_some()
    );
}

#[test]
fn a_stale_home_page_catches_up_on_connect() {
    let (_d, server) = with_notes(&[("a.md", "# first")]);
    let current = version(&server, "/");
    let mut events = server.subscribe("/", "0000000000000000", None);
    let event = events.expect("page");
    assert_eq!(event.id.as_deref(), Some(current.as_str()));
    assert!(
        event.data.contains("<title>Tachylite</title>"),
        "{}",
        event.data
    );
}

/// The next `page` event, past any `gone`.
fn next_page(events: &mut common::Events) -> String {
    loop {
        let event = events.next(Duration::from_secs(10)).expect("no page event");
        if event.name == "page" {
            return event.data;
        }
    }
}

/// Writes the start of a note and keeps it open, as a slow writer does.
fn open_half(path: &std::path::Path, text: &str) -> fs::File {
    use std::io::Write;
    let mut file = fs::File::create(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
    file
}

fn finish(mut file: fs::File, text: &str) {
    use std::io::Write;
    file.write_all(text.as_bytes()).unwrap();
}

#[test]
fn a_config_save_during_a_config_reload_is_kept() {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
    use std::mem::MaybeUninit;
    // Many folders make a config reload's rewatch long.
    let notes: Vec<(String, &str)> = (0..300).map(|i| (format!("f{i}/n.md"), "# f")).collect();
    let mut notes: Vec<(&str, &str)> = notes.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    notes.push(("tachylite.toml", "exclude = []\n"));
    notes.push(("d/n.md", "# n"));
    let (d, server) = with_notes(&notes);
    let toml = d.vault().join("tachylite.toml");
    let opens = inotify::init(CreateFlags::CLOEXEC).unwrap();
    // inotify merges an event into an equal one still queued. Each open is followed by
    // its close, so two opens never sit together and none is merged away.
    let mask = WatchFlags::OPEN | WatchFlags::CLOSE_WRITE | WatchFlags::CLOSE_NOWRITE;
    inotify::add_watch(&opens, &toml, mask).unwrap();
    let mut buf = vec![MaybeUninit::<u8>::uninit(); 4096];
    // Awaits one open of the config: ours, then the server's reload.
    // One read can return several opens, so count them all.
    let mut seen = 0;
    let mut await_open = || {
        while seen == 0 {
            let timeout = Timespec::try_from(Duration::from_secs(10)).unwrap();
            let mut fds = [PollFd::new(&opens, PollFlags::IN)];
            assert_eq!(poll(&mut fds, Some(&timeout)).unwrap(), 1, "no open");
            let mut reader = inotify::Reader::new(&opens, &mut buf);
            loop {
                if reader.next().unwrap().events().contains(ReadFlags::OPEN) {
                    seen += 1;
                }
                if reader.is_buffer_empty() {
                    break;
                }
            }
        }
        seen -= 1;
    };
    for round in 0..20 {
        let (first, last) = if round % 2 == 0 {
            ("[]", "[\"d\"]")
        } else {
            ("[\"d\"]", "[]")
        };
        let mut events = server.subscribe(&format!("/r{round}"), "", None);
        fs::write(&toml, format!("exclude = {first}\n")).unwrap();
        await_open();
        await_open();
        // The server has read the first save, so the second lands during its rewatch.
        fs::write(&toml, format!("exclude = {last}\n")).unwrap();
        await_open();
        // The server reloads the second save: its event was not lost.
        await_open();
        fs::write(d.vault().join(format!("r{round}.md")), "# r").unwrap();
        events.expect("page");
        let want = if round % 2 == 0 { 404 } else { 200 };
        assert_eq!(server.get("/d/n").status, want, "round {round}");
    }
}

#[test]
fn a_save_earlier_in_the_batch_does_not_publish_a_new_half_note() {
    let (d, server) = with_notes_env(&[("w.md", "# w"), ("bar.md", "# b0")], LONG_HOLD);
    let mut events = server.subscribe("/w", &version(&server, "/w"), None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    let note = d.vault().join("w.md");
    stop(&server);
    // One batch: the saved note, its removal, and a new file still being written.
    fs::write(&note, "# saved").unwrap();
    fs::remove_file(&note).unwrap();
    let file = open_half(&note, "# new\n\nhalf");
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    resume(&server);
    barrier.expect("page");
    finish(file, " and the rest");
    let page = next_page(&mut events);
    assert!(page.contains("half and the rest"), "{page}");
}

#[test]
fn a_dependent_rerender_waits_for_an_open_note() {
    let (d, server) = with_notes_env(
        &[("a.md", "# a"), ("b.md", "# b"), ("bar.md", "# b0")],
        LONG_HOLD,
    );
    let mut events = server.subscribe("/b", &version(&server, "/b"), None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    let mut a = server.subscribe("/a", &version(&server, "/a"), None);
    let note = d.vault().join("b.md");
    stop(&server);
    // One batch removes and recreates `b.md`, so it keeps its page while held.
    fs::remove_file(&note).unwrap();
    let file = open_half(&note, "# b\n\nhalf");
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    resume(&server);
    barrier.expect("page");
    // A new link to `b` re-renders its page in a later batch, while `b.md` is still open.
    fs::write(d.vault().join("a.md"), "see [[b]]").unwrap();
    a.expect("page");
    finish(file, " and the rest");
    let page = next_page(&mut events);
    assert!(page.contains("half and the rest"), "{page}");
    assert!(page.contains("<h2>Backlinks</h2>"), "{page}");
}

#[test]
fn an_embedder_renders_again_when_an_open_note_is_saved() {
    let (d, server) = with_notes_env(
        &[("x.md", "![[b]]"), ("b.md", "# b"), ("bar.md", "# b0")],
        LONG_HOLD,
    );
    let mut x = server.subscribe("/x", &version(&server, "/x"), None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    let note = d.vault().join("b.md");
    stop(&server);
    // One batch removes and recreates `b.md`, so it keeps its page while held.
    fs::remove_file(&note).unwrap();
    let file = open_half(&note, "# b\n\nhalf");
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    resume(&server);
    barrier.expect("page");
    // `x` renders while `b.md` is open, so its embed of `b` is a link for now.
    fs::write(d.vault().join("x.md"), "![[b]] again").unwrap();
    assert!(!next_page(&mut x).contains("half"));
    finish(file, " and the rest");
    let page = next_page(&mut x);
    assert!(page.contains("half and the rest"), "{page}");
}

#[test]
fn a_new_folder_scan_waits_for_an_open_note() {
    let (d, server) = with_notes_env(&[("bar.md", "# b0")], LONG_HOLD);
    let mut events = server.subscribe("/d/n", "", None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    stop(&server);
    // The folder's watch is added after the note was created, so no CREATE names it.
    fs::create_dir(d.vault().join("d")).unwrap();
    let file = open_half(&d.vault().join("d/n.md"), "# n\n\nhalf");
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    resume(&server);
    barrier.expect("page");
    finish(file, " and the rest");
    let page = next_page(&mut events);
    assert!(page.contains("half and the rest"), "{page}");
}

#[test]
fn a_folder_deleted_and_recreated_publishes_the_new_note() {
    let (d, server) = with_notes(&[("d/n.md", "# old")]);
    let mut events = server.subscribe("/d/n", &version(&server, "/d/n"), None);
    stop(&server);
    // The new note is written and closed before the new folder's watch exists.
    fs::remove_dir_all(d.vault().join("d")).unwrap();
    fs::create_dir(d.vault().join("d")).unwrap();
    fs::write(d.vault().join("d/n.md"), "# new").unwrap();
    resume(&server);
    let page = next_page(&mut events);
    assert!(page.contains("\">new</h1>"), "{page}");
}

#[test]
fn a_save_by_link_is_published() {
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/n", "", None);
    let tmp = d.vault().join(".n.md.tmp");
    fs::write(&tmp, "# linked").unwrap();
    stop(&server);
    // `link()` then `unlink()`: the note appears with one link and no CLOSE_WRITE.
    fs::hard_link(&tmp, d.vault().join("n.md")).unwrap();
    fs::remove_file(&tmp).unwrap();
    resume(&server);
    let page = next_page(&mut events);
    assert!(page.contains("\">linked</h1>"), "{page}");
}

/// Permission bits do not bind root, so the permission tests skip there.
fn root() -> bool {
    rustix::process::geteuid().is_root()
}

#[test]
fn a_note_made_unreadable_and_readable_again_follows() {
    use std::os::unix::fs::PermissionsExt;
    if root() {
        return;
    }
    let (d, server) = with_notes(&[("a.md", "# a")]);
    let mut events = server.subscribe("/a", &version(&server, "/a"), None);
    let note = d.vault().join("a.md");
    fs::set_permissions(&note, fs::Permissions::from_mode(0o000)).unwrap();
    events.expect("gone");
    fs::set_permissions(&note, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(events.expect("page").data.contains("\">a</h1>"));
}

#[test]
fn a_folder_made_readable_is_published() {
    use std::os::unix::fs::PermissionsExt;
    if root() {
        return;
    }
    let (d, server) = with_notes(&[("bar.md", "# b0")]);
    let mut events = server.subscribe("/d/n", "", None);
    let mut barrier = server.subscribe("/bar", &version(&server, "/bar"), None);
    let folder = d.vault().join("d");
    stop(&server);
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("n.md"), "# n").unwrap();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o000)).unwrap();
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    resume(&server);
    barrier.expect("page");
    assert_eq!(server.get("/d/n").status, 404);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(next_page(&mut events).contains("\">n</h1>"));
}

fn set_times(path: &std::path::Path, at: std::time::SystemTime) {
    let times = fs::FileTimes::new().set_accessed(at).set_modified(at);
    fs::File::open(path).unwrap().set_times(times).unwrap();
}

#[test]
fn copied_times_reorder_recent_notes() {
    use std::time::SystemTime;
    let d = Dirs::empty();
    fs::write(d.vault().join("older.md"), "# older").unwrap();
    fs::write(d.vault().join("newer.md"), "# newer").unwrap();
    let day = Duration::from_secs(86_400);
    set_times(&d.vault().join("newer.md"), SystemTime::now() - day);
    let server = Server::start(&d.vault(), &d.out());
    let home = server.get("/").text();
    assert!(home.find("/older\"") < home.find("/newer\""), "{home}");
    let mut events = server.subscribe("/", &version_of(&home), None);
    // `cp -p` and `rsync -t` set both times after the close, with no save event.
    set_times(&d.vault().join("newer.md"), SystemTime::now() + day);
    let page = next_page(&mut events);
    assert!(page.find("/newer\"") < page.find("/older\""), "{page}");
}

#[test]
fn exchanged_folders_stay_watched() {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    let (d, server) = with_notes(&[("a/x.md", "# x"), ("b/y.md", "# y")]);
    let mut moved = server.subscribe("/b/x", "", None);
    renameat_with(
        CWD,
        d.vault().join("a"),
        CWD,
        d.vault().join("b"),
        RenameFlags::EXCHANGE,
    )
    .unwrap();
    assert!(next_page(&mut moved).contains("\">x</h1>"));
    fs::write(d.vault().join("b/x.md"), "# edited").unwrap();
    assert!(next_page(&mut moved).contains("\">edited</h1>"));
    let mut other = server.subscribe("/a/y", &version(&server, "/a/y"), None);
    fs::write(d.vault().join("a/y.md"), "# also").unwrap();
    assert!(next_page(&mut other).contains("\">also</h1>"));
}

#[test]
fn a_renamed_vault_parent_stops_the_server() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("p/vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(vault.join("a.md"), "# a").unwrap();
    let mut server = Server::start(&vault, &tmp.path().join("out"));
    fs::rename(tmp.path().join("p"), tmp.path().join("q")).unwrap();
    // A new folder is watched and scanned by path, which now names nothing.
    fs::create_dir(tmp.path().join("q/vault/d")).unwrap();
    let line = server
        .wait_line(|l| l.starts_with("error:"), Duration::from_secs(10))
        .expect("no error line");
    assert!(line.contains("vault"), "{line}");
    assert_eq!(server.child.wait().unwrap().code(), Some(1));
}

// Tag pages.

#[test]
fn a_tag_page_is_served_pushed_and_gone_with_its_last_note() {
    let (d, server) = with_notes(&[("a.md", "#Plants here"), ("b.md", "plain")]);
    let page = "/.tachylite/tags/plants";
    let mut events = server.subscribe(page, &version(&server, page), None);
    fs::write(d.vault().join("b.md"), "now #plants/fern").unwrap();
    let event = events.expect("page");
    assert!(event.data.contains("href=\"/b\""), "{}", event.data);
    fs::remove_file(d.vault().join("a.md")).unwrap();
    fs::remove_file(d.vault().join("b.md")).unwrap();
    // Each delete may land in its own batch, so the gone follows zero or one page push.
    loop {
        if events.next(Duration::from_secs(10)).expect("gone").name == "gone" {
            break;
        }
    }
    assert_eq!(server.get(page).status, 404);
}
