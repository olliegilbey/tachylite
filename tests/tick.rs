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

/// The token of the box on line `number` of `text`, counted from 1.
fn token(text: &str, number: usize) -> Token {
    let boxes = tick::boxes(text.as_bytes());
    boxes.iter().find(|b| b.line == number).unwrap().token
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
fn a_tick_finds_its_task_by_its_text_and_its_place() {
    let text = "- [ ] water\n# a\n- [ ] ship\n# b\n- [ ] ship\n";
    let d = vault("", &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let dir = VaultDir::open(&d.vault()).unwrap();
    let (water, first, second) = (token(text, 1), token(text, 3), token(text, 5));
    // The place picks between two lines of one text.
    dir.tick("n.md", second, To::Done).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "- [ ] water\n# a\n- [ ] ship\n# b\n- [x] ship\n"
    );
    // Three lines added above: each token still names its own task.
    fs::write(
        &path,
        "one\ntwo\nthree\n- [ ] water\n# a\n- [ ] ship\n# b\n- [x] ship\n",
    )
    .unwrap();
    dir.tick("n.md", second, To::Todo).unwrap();
    dir.tick("n.md", first, To::Done).unwrap();
    dir.tick("n.md", water, To::Done).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "one\ntwo\nthree\n- [x] water\n# a\n- [x] ship\n# b\n- [ ] ship\n"
    );
    // A line of the text added or removed: no place says which line is which.
    for changed in [
        "- [ ] ship\n- [ ] water\n# a\n- [ ] ship\n# b\n- [ ] ship\n",
        "- [ ] water\n# a\n- [ ] ship\n# b\n- [ ] ship\n\n```\n- [x] ship\n```\n",
        "- [ ] water\n# a\n# b\n- [ ] ship\n",
        "- [ ] water\n# a\n- [ ] ship\n# b\n",
    ] {
        fs::write(&path, changed).unwrap();
        let before = state(&path);
        for stale in [first, second] {
            let ticked = dir.tick("n.md", stale, To::Done);
            assert!(
                matches!(ticked, Err(TickError::Conflict)),
                "{changed:?}: {ticked:?}"
            );
        }
        assert_eq!(state(&path), before, "{changed:?}");
        // The task with a text of its own is still found.
        dir.tick("n.md", water, To::Todo).unwrap();
        assert_eq!(state(&path), before, "{changed:?}");
    }
    // Reworded.
    fs::write(&path, "- [ ] water the fern\n").unwrap();
    let reworded = state(&path);
    assert!(matches!(
        dir.tick("n.md", water, To::Done),
        Err(TickError::Conflict)
    ));
    assert_eq!(state(&path), reworded);
}

#[test]
fn a_note_too_large_to_render_is_a_conflict() {
    let limit = usize::try_from(tachylite::vault::NOTE_MAX).unwrap();
    let task = "- [ ] water\n";
    let large = format!("{task}{}", "x".repeat(limit + 1 - task.len()));
    let d = vault("", &[("large.md", &large), ("full.md", &large[..limit])]);
    let dir = VaultDir::open(&d.vault()).unwrap();
    let before = state(&d.vault().join("large.md"));
    assert!(matches!(
        dir.tick("large.md", token(task, 1), To::Done),
        Err(TickError::Conflict)
    ));
    assert_eq!(state(&d.vault().join("large.md")), before);
    // A note at the limit renders, so it ticks.
    dir.tick("full.md", token(task, 1), To::Done).unwrap();
    assert!(
        fs::read(d.vault().join("full.md"))
            .unwrap()
            .starts_with(b"- [x] water\n")
    );
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
    // A symlink that stays inside the vault is still no note.
    symlink("n.md", d.vault().join("inside.md")).unwrap();
    symlink(d.vault(), d.vault().join("loop")).unwrap();
    fs::create_dir(d.vault().join("folder.md")).unwrap();
    let fifo = Command::new("mkfifo")
        .arg(d.vault().join("pipe.md"))
        .status()
        .unwrap();
    assert!(fifo.success());
    let dir = VaultDir::open(&d.vault()).unwrap();
    for rel in [
        "link.md",
        "inside.md",
        "loop/n.md",
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
    // A page of the same name over another scheme, behind a proxy on the scheme's own port:
    // its origin reads like this site's, and the browser says it is not.
    let origin = format!("http://{local}");
    for site in ["cross-site", "same-site", "none", "Same-Origin", ""] {
        assert_eq!(
            post(&local, &[("Origin", &origin), ("Sec-Fetch-Site", site)]),
            403,
            "{site:?}"
        );
    }
    // What a browser says is no stand-in for the origin.
    assert_eq!(post(&local, &[("Sec-Fetch-Site", "same-origin")]), 403);
    // A name the server does not answer to, though the origin matches it.
    assert_eq!(
        post("evil.example", &[("Origin", "http://evil.example")]),
        421
    );
    assert_eq!(state(&d.vault().join("n.md")), before);
    // A listed name, in any case.
    let named = [
        ("Origin", "https://BOX.example.ts.net"),
        ("Sec-Fetch-Site", "same-origin"),
    ];
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
        ("0000000", "done"),
        ("0000000g", "done"),
        ("1-00000000", "done"),
        ("00000000.0", "done"),
        ("00000000.0.1", "done"),
        ("00000000.2.2", "done"),
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
    assert_eq!(server.post(&task("n.md", "00000000", "done")).status, 409);
    assert_eq!(
        server.post(&task("n.md", "00000000.1.2", "done")).status,
        409
    );
    // One line has the task's hash, and the token says two did.
    assert_eq!(
        server
            .post(&task("n.md", format!("{at}.0.2"), "done"))
            .status,
        409
    );
    assert_eq!(
        server.post(&task("n.md", token(text, 2), "done")).status,
        409
    );
    assert_eq!(state(&path), before);
}

#[test]
fn a_tick_follows_its_task_through_an_edit() {
    let text = "- [ ] water\n# a\n- [ ] ship\n# b\n- [ ] ship\n";
    let (d, server) = serving(ON, &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let page = server.get("/n").text();
    let mut events = server.subscribe("/n", &version_of(&page), None);
    let water = shown(&page, "water");
    // Two tasks of one text: each carries its place among them.
    let tokens: Vec<&str> = page
        .split(" data-box=\"")
        .skip(1)
        .map(|rest| rest.split_once('"').unwrap().0)
        .collect();
    let ship = token(text, 3).hash;
    assert_eq!(
        tokens,
        [
            water.clone(),
            format!("{ship:08x}.0.2"),
            format!("{ship:08x}.1.2")
        ]
    );
    let second = tokens[2];
    // The page a reader holds is older than the note: lines were added above its tasks.
    fs::write(&path, format!("# title\n\nintro\n\n{text}")).unwrap();
    events.expect("page");
    assert_eq!(server.post(&task("n.md", &water, "done")).status, 204);
    events.expect("page");
    // The second task's token ticks the second task, not the first.
    assert_eq!(server.post(&task("n.md", second, "done")).status, 204);
    let ticked = "# title\n\nintro\n\n- [x] water\n# a\n- [ ] ship\n# b\n- [x] ship\n";
    assert_eq!(fs::read_to_string(&path).unwrap(), ticked);
    let pushed = events.expect("page").data;
    // A third line of the text: the first page's tokens name two, and are refused.
    fs::write(&path, format!("{ticked}- [ ] ship\n")).unwrap();
    events.expect("page");
    let before = state(&path);
    for stale in [tokens[1], second] {
        assert_eq!(server.post(&task("n.md", stale, "done")).status, 409);
    }
    assert_eq!(state(&path), before);
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
fn a_task_deleted_beside_its_text_in_a_code_block_is_refused() {
    let text = "- [ ] ship\n\n```\n- [ ] ship\n```\n";
    let (d, server) = serving(ON, &[("n.md", text)]);
    let path = d.vault().join("n.md");
    let page = server.get("/n").text();
    let mut events = server.subscribe("/n", &version_of(&page), None);
    // The line in the code block counts, on the page as in the write.
    let at = shown(&page, "ship");
    assert_eq!(at, format!("{:08x}.0.2", token(text, 1).hash));
    // The task is deleted, and the code block keeps a line of its text.
    fs::write(&path, &text[11..]).unwrap();
    events.expect("page");
    let before = state(&path);
    assert_eq!(server.post(&task("n.md", &at, "done")).status, 409);
    assert_eq!(state(&path), before);
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
    assert_eq!(post_after_rescan(&server, &task("n.md", &at, "done")), 204);
    assert!(events.expect("page").data.contains("checked> water</li>"));
}

/// Posts a tick once the rescan of a new config is over: until then the answer is 503.
fn post_after_rescan(server: &Server, path: &str) -> u16 {
    loop {
        let status = server.post(path).status;
        if status != 503 {
            return status;
        }
        std::thread::yield_now();
    }
}

#[test]
fn a_config_that_does_not_load_shuts_the_route() {
    let text = "- [ ] water\n";
    // The config is deleted, or saved with a typo, or with no boolean.
    for case in [None, Some("writes = flase\n"), Some("writes = \"off\"\n")] {
        let (d, server) = serving(ON, &[("n.md", text), ("bar.md", "# b0")]);
        let (path, toml) = (d.vault().join("n.md"), d.vault().join("tachylite.toml"));
        let page = server.get("/n").text();
        let tick = task("n.md", shown(&page, "water"), "done");
        let mut barrier = server.subscribe("/bar", &version_of(&server.get("/bar").text()), None);
        // Events apply in order, so the barrier's push means the config's were handled.
        let mut settle = |mark: &str| {
            fs::write(d.vault().join("bar.md"), format!("# {mark}")).unwrap();
            barrier.expect("page");
        };

        match case {
            Some(broken) => fs::write(&toml, broken).unwrap(),
            None => fs::remove_file(&toml).unwrap(),
        }
        settle("b1");
        // The previous config stays for the pages, and no tick lands.
        assert_eq!(server.get("/n").text(), page, "{case:?}");
        let route = server.post(&tick);
        assert_eq!(route.status, 405, "{case:?}");
        assert_eq!(
            answer(route),
            answer(server.post("/.tachylite/no-such-route?note=n.md")),
            "{case:?}"
        );
        // A later save of a note does not open the route again.
        settle("b2");
        assert_eq!(server.post(&tick).status, 405, "{case:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), text, "{case:?}");

        // A config that loads and says so opens it.
        fs::write(&toml, ON).unwrap();
        settle("b3");
        assert_eq!(post_after_rescan(&server, &tick), 204, "{case:?}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "- [x] water\n",
            "{case:?}"
        );
    }
}

/// Every file of an output's `pages/`, by its path below it.
fn pages(out: &Path) -> std::collections::BTreeMap<String, String> {
    let mut found = std::collections::BTreeMap::new();
    let mut stack = vec![out.join("pages")];
    while let Some(dir) = stack.pop() {
        for item in fs::read_dir(&dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(out).unwrap().to_str().unwrap();
                found.insert(rel.to_string(), fs::read_to_string(&path).unwrap());
            }
        }
    }
    found
}

#[test]
fn a_page_with_no_task_to_tick_is_the_same_with_writes_on_and_off() {
    let notes = [
        ("plain.md", "# Plain\n\ntext [[tasks]]\n"),
        ("cancelled.md", "- [-] paint the fence\n"),
        ("embeds.md", "![[tasks]]\n"),
        ("d/tasks.md", "- [ ] water\n- [x] sweep\n"),
    ];
    let (off, on) = (vault("", &notes), vault(ON, &notes));
    tachylite::build::build(&off.vault(), &off.out()).unwrap();
    tachylite::build::build(&on.vault(), &on.out()).unwrap();
    let (off, on) = (pages(&off.out()), pages(&on.out()));
    assert_eq!(
        off.keys().collect::<Vec<_>>(),
        on.keys().collect::<Vec<_>>()
    );
    let mut ticked = Vec::new();
    for (path, page) in &on {
        if page.contains(" data-box=\"") {
            assert!(page.contains(" data-src=\"d/tasks.md\""), "{path}: {page}");
            ticked.push(path.as_str());
        } else {
            assert_eq!(page, &off[path], "{path}");
        }
    }
    assert_eq!(
        ticked,
        ["pages/.tachylite/tasks.html", "pages/d/tasks.html"]
    );
}

#[test]
fn the_board_escapes_the_path_of_a_task_list() {
    let name = "a \"b\" & <c>.md";
    let d = vault(ON, &[(name, "- [ ] water\n")]);
    tachylite::build::build(&d.vault(), &d.out()).unwrap();
    let board = fs::read_to_string(d.out().join("pages/.tachylite/tasks.html")).unwrap();
    let src = " data-src=\"a &quot;b&quot; &amp; &lt;c&gt;.md\"";
    assert!(board.contains(src), "{board}");
    assert_eq!(board.matches(" data-src=").count(), 1, "{board}");
    let page = pages(&d.out())
        .into_values()
        .find(|page| page.contains("<main data-src="))
        .unwrap();
    assert!(page.contains(&format!("<main{src}>")), "{page}");
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
