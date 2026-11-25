//! A tick, Tachylite's one write to the vault: the write itself, then the route at the live
//! seam.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use common::{Dirs, Response, Server, encode, send, version_of};
use tachylite::tick::{self, To, Token};
use tachylite::vault::{TickError, VaultDir};

/// A vault with `notes`, and the config `toml`.
fn vault(toml: &str, notes: &[(&str, &str)]) -> Dirs {
    let d = Dirs::empty();
    fs::write(d.vault().join("tachylite.toml"), toml).unwrap();
    for (path, body) in notes {
        let path = d.vault().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    d
}

fn serving(toml: &str, notes: &[(&str, &str)]) -> (Dirs, Server) {
    let d = vault(toml, notes);
    let server = Server::start(&d.vault(), &d.out());
    (d, server)
}

const ON: &str = "writes = true\n";

/// The token of the box on line `number` of `text`.
fn token(text: &str, number: usize) -> Token {
    let (_, raw) = tick::lines(text.as_bytes()).nth(number - 1).unwrap();
    Token::of(number, raw).unwrap().0
}

/// The route's path for a tick.
fn task(note: &str, at: impl ToString, to: &str) -> String {
    format!(
        "/.tachylite/task?note={}&at={}&to={to}",
        encode(note),
        at.to_string()
    )
}

/// The token a page gives the task with `text`.
fn shown(html: &str, text: &str) -> String {
    let end = html
        .find(&format!("> {text}</li>"))
        .unwrap_or_else(|| panic!("no task {text:?} in {html}"));
    let item = &html[html[..end].rfind("<li ").unwrap()..end];
    let (_, rest) = item
        .split_once(" data-box=\"")
        .unwrap_or_else(|| panic!("no token on {item}"));
    rest.split_once('"').unwrap().0.to_string()
}

/// The board's task list for `note`.
fn section<'a>(board: &'a str, note: &str) -> &'a str {
    let list = &board[board.find(&format!("data-src=\"{note}\"")).unwrap()..];
    &list[..list.find("</section>").unwrap()]
}

/// Every name in a folder.
fn names(vault: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(vault)
        .unwrap()
        .map(|item| item.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// What says a file was not written: its bytes, its inode and its modified time.
fn state(path: &Path) -> (Vec<u8>, u64, SystemTime) {
    let meta = fs::metadata(path).unwrap();
    (
        fs::read(path).unwrap(),
        meta.ino(),
        meta.modified().unwrap(),
    )
}

/// A response without the header that differs between any two.
fn answer(mut response: Response) -> (u16, Vec<(String, String)>, Vec<u8>) {
    response.headers.remove("date");
    (
        response.status,
        response.headers.into_iter().collect(),
        response.body,
    )
}

#[test]
fn the_write_changes_one_byte_in_place() {
    let text = "# n\n\n- [ ] water\n- [/] repot\n- [x] sweep\n";
    let d = vault("", &[("d/n.md", text)]);
    let path = d.vault().join("d/n.md");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let dir = VaultDir::open(&d.vault()).unwrap();
    let before = fs::metadata(&path).unwrap();
    for (line, to, want) in [
        (
            3,
            To::Done,
            "# n\n\n- [x] water\n- [/] repot\n- [x] sweep\n",
        ),
        (
            4,
            To::Done,
            "# n\n\n- [x] water\n- [x] repot\n- [x] sweep\n",
        ),
        (
            5,
            To::Todo,
            "# n\n\n- [x] water\n- [x] repot\n- [ ] sweep\n",
        ),
        (
            3,
            To::Todo,
            "# n\n\n- [ ] water\n- [x] repot\n- [ ] sweep\n",
        ),
    ] {
        dir.tick("d/n.md", token(text, line), to).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), want);
    }
    let after = fs::metadata(&path).unwrap();
    assert_eq!(
        (after.ino(), after.len(), after.mode(), after.nlink()),
        (before.ino(), before.len(), before.mode(), before.nlink())
    );
    assert_eq!(names(&d.vault()), ["d", "tachylite.toml"]);
    assert_eq!(names(&d.vault().join("d")), ["n.md"]);
}

#[test]
fn a_task_already_as_asked_takes_no_write() {
    let text = "- [x] done\n- [ ] todo\n- [/] doing\n- [-] cancelled\n";
    let d = vault("", &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let dir = VaultDir::open(&d.vault()).unwrap();
    let before = state(&path);
    for (line, to) in [(1, To::Done), (2, To::Todo), (3, To::Todo), (4, To::Todo)] {
        dir.tick("n.md", token(text, line), to).unwrap();
    }
    // A cancelled task is never ticked done.
    assert!(matches!(
        dir.tick("n.md", token(text, 4), To::Done),
        Err(TickError::Conflict)
    ));
    assert_eq!(state(&path), before);
}

#[test]
fn a_changed_task_is_a_conflict_and_a_moved_one_is_found() {
    let text = "- [ ] water\n- [ ] same\n- [ ] same\n";
    let d = vault("", &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let dir = VaultDir::open(&d.vault()).unwrap();
    let (water, first, second) = (token(text, 1), token(text, 2), token(text, 3));
    // The line number picks between two lines of one text.
    dir.tick("n.md", second, To::Done).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "- [ ] water\n- [ ] same\n- [x] same\n"
    );
    // Lines above move the task, and its text still finds it.
    fs::write(&path, "intro\n\n- [ ] water\n- [ ] same\n- [x] same\n").unwrap();
    dir.tick("n.md", water, To::Done).unwrap();
    let moved = "intro\n\n- [x] water\n- [ ] same\n- [x] same\n";
    assert_eq!(fs::read_to_string(&path).unwrap(), moved);
    // Moved, the two lines of one text are either's.
    let before = state(&path);
    assert!(matches!(
        dir.tick("n.md", first, To::Done),
        Err(TickError::Conflict)
    ));
    // Reworded.
    fs::write(&path, "- [ ] water the fern\n").unwrap();
    let before_reworded = state(&path);
    assert!(matches!(
        dir.tick("n.md", water, To::Done),
        Err(TickError::Conflict)
    ));
    assert_eq!(state(&path), before_reworded);
    assert_ne!(before.0, before_reworded.0);
}

#[test]
fn only_a_regular_file_inside_the_vault_is_written() {
    let text = "- [ ] water\n";
    let d = vault("", &[("n.md", text)]);
    let outside = d.tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.md"), text).unwrap();
    symlink(outside.join("secret.md"), d.vault().join("link.md")).unwrap();
    symlink(&outside, d.vault().join("d")).unwrap();
    fs::create_dir(d.vault().join("folder.md")).unwrap();
    let fifo = Command::new("mkfifo")
        .arg(d.vault().join("pipe.md"))
        .status()
        .unwrap();
    assert!(fifo.success());
    let dir = VaultDir::open(&d.vault()).unwrap();
    for rel in [
        "link.md",
        "d/secret.md",
        "../outside/secret.md",
        "folder.md",
        "pipe.md",
        "nope.md",
        "no/dir.md",
        "n.md/x.md",
    ] {
        let ticked = dir.tick(rel, token(text, 1), To::Done);
        assert!(
            matches!(ticked, Err(TickError::Missing)),
            "{rel}: {ticked:?}"
        );
    }
    assert_eq!(fs::read_to_string(outside.join("secret.md")).unwrap(), text);
    assert_eq!(fs::read_to_string(d.vault().join("n.md")).unwrap(), text);
}

#[test]
fn a_note_that_takes_no_writes_is_read_only() {
    let text = "- [ ] water\n";
    let d = vault("", &[("n.md", text)]);
    let path = d.vault().join("n.md");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    // Root writes whatever the mode says.
    if fs::OpenOptions::new().write(true).open(&path).is_ok() {
        return;
    }
    let dir = VaultDir::open(&d.vault()).unwrap();
    assert!(matches!(
        dir.tick("n.md", token(text, 1), To::Done),
        Err(TickError::ReadOnly)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn a_tick_changes_one_byte_and_pushes_the_page() {
    let text = "# n\n\n- [ ] water\n- [/] repot\n";
    let (d, server) = serving(ON, &[("n.md", text), ("other.md", "- [ ] water\n")]);
    let path = d.vault().join("n.md");
    let page = server.get("/n").text();
    assert!(page.contains("<main data-src=\"n.md\">"), "{page}");
    let board = server.get("/.tachylite/tasks").text();
    let at = shown(&page, "water");
    assert_eq!(at, token(text, 3).to_string());
    assert_eq!(shown(section(&board, "n.md"), "water"), at);
    let mut events = server.subscribe("/n", &version_of(&page), None);
    let mut boards = server.subscribe("/.tachylite/tasks", &version_of(&board), None);
    let (inode, files) = (fs::metadata(&path).unwrap().ino(), names(&d.vault()));

    let response = server.post(&task("n.md", &at, "done"));
    assert_eq!((response.status, response.body.len()), (204, 0));

    let pushed = events.expect("page").data;
    let done = format!(
        "<li data-task=\"done\" data-box=\"{at}\"><input type=\"checkbox\" checked> water</li>"
    );
    assert!(pushed.contains(&done), "{pushed}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        text.replace("[ ] water", "[x] water")
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(names(&d.vault()), files);
    assert_eq!(
        fs::read_to_string(d.vault().join("other.md")).unwrap(),
        "- [ ] water\n"
    );
    // The board drops the done task from its note's list.
    let board = boards.expect("page").data;
    let list = section(&board, "n.md");
    assert!(list.contains("repot") && !list.contains("water"), "{list}");

    // A doing task ticks done, and a done task unticks to todo.
    let at = shown(&pushed, "repot");
    assert_eq!(server.post(&task("n.md", &at, "done")).status, 204);
    assert!(events.expect("page").data.contains("checked> repot</li>"));
    assert_eq!(server.post(&task("n.md", &at, "todo")).status, 204);
    let pushed = events.expect("page").data;
    assert!(
        pushed.contains("<input type=\"checkbox\"> repot</li>"),
        "{pushed}"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# n\n\n- [x] water\n- [ ] repot\n"
    );
}

#[test]
fn with_writes_off_the_route_is_an_unknown_path() {
    let text = "- [ ] water\n";
    for toml in ["", "writes = false\n"] {
        let (d, server) = serving(toml, &[("n.md", text)]);
        let page = server.get("/n").text();
        assert!(
            !page.contains("data-box") && !page.contains("data-src"),
            "{page}"
        );
        let before = state(&d.vault().join("n.md"));
        let route = server.post(&task("n.md", token(text, 1), "done"));
        assert_eq!(route.status, 405);
        assert_eq!(
            answer(route),
            answer(server.post("/.tachylite/no-such-route?note=n.md"))
        );
        assert_eq!(state(&d.vault().join("n.md")), before);
    }
    // With writes on, the route takes a POST alone.
    let (_d, server) = serving(ON, &[("n.md", text)]);
    let route = server.get(&task("n.md", token(text, 1), "done"));
    assert_eq!(route.status, 404);
    assert_eq!(
        answer(route),
        answer(server.get("/.tachylite/no-such-route"))
    );
}

#[test]
fn a_note_with_no_page_reads_as_a_missing_one() {
    let text = "- [ ] water\n";
    let toml = "writes = true\nexclude = [\"private\"]\n";
    let (d, server) = serving(
        toml,
        &[
            ("n.md", text),
            ("private/a.md", text),
            // `clash.MD` sorts first and takes the URL.
            ("clash.MD", text),
            ("clash.md", text),
            ("page.html", text),
            ("notes.txt", text),
        ],
    );
    let at = token(text, 1);
    let missing = server.post(&task("nope.md", at, "done"));
    assert_eq!(missing.status, 404);
    let missing = answer(missing);
    for note in [
        "private/a.md",
        "clash.md",
        "page.html",
        "notes.txt",
        "tachylite.toml",
        "../vault/n.md",
        "/n.md",
        "n",
        "",
    ] {
        let before = state(&d.vault().join("private/a.md"));
        assert_eq!(
            answer(server.post(&task(note, at, "done"))),
            missing,
            "{note}"
        );
        // A token that names nothing changes nothing about the answer.
        assert_eq!(
            answer(server.post(&task(note, "x", "done"))),
            missing,
            "{note}"
        );
        assert_eq!(state(&d.vault().join("private/a.md")), before);
    }
    for file in ["clash.md", "clash.MD", "page.html", "notes.txt"] {
        assert_eq!(
            fs::read_to_string(d.vault().join(file)).unwrap(),
            text,
            "{file}"
        );
    }
    assert_eq!(server.post(&task("n.md", at, "done")).status, 204);
}

#[test]
fn a_tick_comes_from_a_page_of_this_site() {
    let text = "- [ ] water\n";
    let toml = "writes = true\nhosts = [\"box.example.ts.net\"]\n";
    let (d, server) = serving(toml, &[("n.md", text)]);
    let path = task("n.md", token(text, 1), "done");
    let before = state(&d.vault().join("n.md"));
    let port = server.addr.port();
    let local = server.addr.to_string();
    let post = |host: &str, headers: &[(&str, &str)]| {
        send(server.addr, "POST", &path, host, headers).status
    };
    // Another site's page, a sandboxed html page, and a client that sends no origin.
    assert_eq!(post(&local, &[("Origin", "https://evil.example")]), 403);
    assert_eq!(post(&local, &[("Origin", "null")]), 403);
    assert_eq!(post(&local, &[]), 403);
    assert_eq!(
        post(
            &local,
            &[("Origin", &format!("http://127.0.0.1:{}", port ^ 1))]
        ),
        403
    );
    assert_eq!(
        post(&local, &[("Origin", &format!("http://localhost:{port}"))]),
        403
    );
    assert_eq!(post(&local, &[("Origin", &local)]), 403);
    // A name the server does not answer to, though the origin matches it.
    assert_eq!(
        post("evil.example", &[("Origin", "http://evil.example")]),
        421
    );
    assert_eq!(state(&d.vault().join("n.md")), before);
    // A listed name, in any case.
    let named = [("Origin", "https://BOX.example.ts.net")];
    assert_eq!(post("box.example.ts.net", &named), 204);
}

#[test]
fn with_no_hosts_set_a_tick_takes_the_loopback_alone() {
    let text = "- [ ] water\n";
    let (d, server) = serving(ON, &[("n.md", text)]);
    let path = task("n.md", token(text, 1), "done");
    let before = state(&d.vault().join("n.md"));
    let named = [("Origin", "http://box.example.ts.net")];
    // The site answers any name, and a tick does not.
    assert_eq!(
        send(server.addr, "GET", "/n", "box.example.ts.net", &[]).status,
        200
    );
    assert_eq!(
        send(server.addr, "POST", &path, "box.example.ts.net", &named).status,
        421
    );
    assert_eq!(state(&d.vault().join("n.md")), before);
    let host = format!("localhost:{}", server.addr.port());
    let origin = format!("http://{host}");
    assert_eq!(
        send(server.addr, "POST", &path, &host, &[("Origin", &origin)]).status,
        204
    );
}

#[test]
fn a_tick_that_names_no_task_is_refused() {
    let text = "- [ ] water\n- [-] fence\n";
    let (d, server) = serving(ON, &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let before = state(&path);
    let at = token(text, 1).to_string();
    for (at, to) in [
        ("", "done"),
        ("1", "done"),
        ("1-xyz", "done"),
        ("1-0000000", "done"),
        (at.as_str(), ""),
        (at.as_str(), "doing"),
        (at.as_str(), "DONE"),
    ] {
        assert_eq!(
            server.post(&task("n.md", at, to)).status,
            400,
            "{at:?} {to:?}"
        );
    }
    assert_eq!(server.post("/.tachylite/task?note=n.md").status, 400);
    // No line has this hash, and a cancelled task is never ticked done.
    assert_eq!(server.post(&task("n.md", "1-00000000", "done")).status, 409);
    assert_eq!(
        server.post(&task("n.md", token(text, 2), "done")).status,
        409
    );
    assert_eq!(state(&path), before);
}

#[test]
fn a_tick_follows_its_task_through_an_edit() {
    let text = "- [ ] water\n- [ ] same\n- [ ] same\n";
    let (d, server) = serving(ON, &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let page = server.get("/n").text();
    let mut events = server.subscribe("/n", &version_of(&page), None);
    let water = shown(&page, "water");
    // The page a reader holds is older than the note: lines were added above its tasks.
    fs::write(&path, format!("# title\n\nintro\n\n{text}")).unwrap();
    events.expect("page");
    assert_eq!(server.post(&task("n.md", &water, "done")).status, 204);
    let ticked = "# title\n\nintro\n\n- [x] water\n- [ ] same\n- [ ] same\n";
    assert_eq!(fs::read_to_string(&path).unwrap(), ticked);
    let pushed = events.expect("page").data;
    // Two lines of one text: each token's line number picks its own.
    let second = token(ticked, 7);
    assert_eq!(server.post(&task("n.md", second, "done")).status, 204);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# title\n\nintro\n\n- [x] water\n- [ ] same\n- [x] same\n"
    );
    events.expect("page");
    // The first page's token for either names a line that moved, and both have its text.
    let before = state(&path);
    assert_eq!(
        server.post(&task("n.md", token(text, 2), "done")).status,
        409
    );
    // The task was reworded since its page was rendered.
    fs::write(&path, "- [ ] water the fern\n").unwrap();
    events.expect("page");
    let reworded = state(&path);
    assert_eq!(
        server
            .post(&task("n.md", shown(&pushed, "water"), "todo"))
            .status,
        409
    );
    assert_eq!(state(&path), reworded);
    assert_ne!(before.0, reworded.0);
}

#[test]
fn a_repeated_tick_writes_nothing() {
    let text = "- [ ] water\n";
    let (d, server) = serving(ON, &[("n.md", text), ("bar.md", "# b0")]);
    let path = d.vault().join("n.md");
    let page = server.get("/n").text();
    let mut events = server.subscribe("/n", &version_of(&page), None);
    let mut barrier = server.subscribe("/bar", &version_of(&server.get("/bar").text()), None);
    let tick = task("n.md", shown(&page, "water"), "done");
    assert_eq!(server.post(&tick).status, 204);
    events.expect("page");
    let before = state(&path);
    assert_eq!(server.post(&tick).status, 204);
    assert_eq!(state(&path), before);
    // The barrier's push means the watcher has handled the second tick's close.
    fs::write(d.vault().join("bar.md"), "# b1").unwrap();
    barrier.expect("page");
    fs::write(&path, "- [x] water\n\nmore\n").unwrap();
    // Events on one stream arrive in order, so the first is the edit: the tick pushed none.
    assert!(events.expect("page").data.contains("<p>more</p>"));
}

#[test]
fn a_config_save_turns_writes_off_and_on() {
    let text = "- [ ] water\n";
    let (d, server) = serving(ON, &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let page = server.get("/n").text();
    let at = shown(&page, "water");
    let mut events = server.subscribe("/n", &version_of(&page), None);
    let mut boards = server.subscribe(
        "/.tachylite/tasks",
        &version_of(&server.get("/.tachylite/tasks").text()),
        None,
    );

    fs::write(d.vault().join("tachylite.toml"), "writes = false\n").unwrap();
    let off = events.expect("page").data;
    assert!(
        off.contains("<li data-task=\"todo\"><input type=\"checkbox\" disabled> water</li>"),
        "{off}"
    );
    assert!(
        !off.contains("data-box") && !off.contains("data-src"),
        "{off}"
    );
    let board = boards.expect("page").data;
    assert!(
        !board.contains("data-box") && !board.contains("data-src"),
        "{board}"
    );
    assert_eq!(server.post(&task("n.md", &at, "done")).status, 405);
    assert_eq!(fs::read_to_string(&path).unwrap(), text);

    fs::write(d.vault().join("tachylite.toml"), ON).unwrap();
    let on = events.expect("page").data;
    assert_eq!(shown(&on, "water"), at);
    assert!(
        boards
            .expect("page")
            .data
            .contains(&format!("data-box=\"{at}\""))
    );
    assert_eq!(server.post(&task("n.md", &at, "done")).status, 204);
    assert!(events.expect("page").data.contains("checked> water</li>"));
}

#[test]
fn a_note_swapped_for_a_symlink_is_refused() {
    let text = "- [ ] water\n";
    let (d, server) = serving(ON, &[("n.md", text)]);
    let outside = d.tmp.path().join("secret.md");
    fs::write(&outside, text).unwrap();
    let page = server.get("/n").text();
    let at = shown(&page, "water");
    let mut events = server.subscribe("/n", &version_of(&page), None);
    fs::remove_file(d.vault().join("n.md")).unwrap();
    symlink(&outside, d.vault().join("n.md")).unwrap();
    events.expect("gone");
    assert_eq!(server.post(&task("n.md", &at, "done")).status, 404);
    assert_eq!(fs::read_to_string(&outside).unwrap(), text);
}
