# Slice 2: live path. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `tachylite serve <vault> <out>` serves the output on `127.0.0.1`. A save re-renders the note, writes it and pushes it over SSE, and the open page swaps in the new HTML. `build` becomes the site applied to every file.

**Architecture:** One deep module, the **site**, is the pure core. It takes changes (a touched path, or the full set of files under a folder), reads what it needs through a `Source` trait, and returns a `Delta`: pages to write, files to copy, outputs to delete, pushes to send. A full build is `Sync` of the vault root, so build and serve share one code path. Two threads sit at the edges. The watch thread owns the site, the inotify fd and every vault read and output write. A single-threaded tokio runtime serves HTTP and SSE. They share only a broadcast sender and a map of page versions.

**Tech Stack:** Rust stable 1.99, edition 2024. New runtime crates: `axum` 0.8 (`default-features = false`, features `tokio`, `http1`), `tokio` 1.54 (`rt`, `net`, `sync`, `macros`), `rustix` 1.1 (`fs`), `futures-util` 0.3 (already in axum's tree). Browser tests: Playwright 1.64 in `browser/`, test tooling only.

**Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`. Order, output layout and routing: `docs/plans/2026-11-03-v1-roadmap.md`.

**Branch:** `feat/live-path`. **Issues:** #4, plus #16 and #18, and the first two clashes in #17.

## Decisions to agree

These are the build paths this plan takes. Each one is cheap to change now and costly later.

1. **inotify through `rustix`, not `notify`.** The spec allows `notify`. It costs 9 crates and its own thread. In recursive mode it watches `.git` and `node_modules` too, and it sends no events for files already inside a newly created folder (notify-rs/notify#124). `rustix::fs::inotify` is safe Rust over the same kernel API. We add one non-recursive watch per published folder, so excluded folders cost nothing. `rustix` also gives us `openat2`, for decision 6.
2. **No time debounce.** A timer adds its whole interval to the 10 ms push budget. The watch thread reads every queued event in one go and acts on that batch once. It reacts to `CLOSE_WRITE` and `MOVED_TO`, never `MODIFY`, so it never reads half a file. The spec's "debounce" step is rewritten to say so.
3. **Two threads, no locks on the hot path.** The watch thread renders, writes and then pushes. The HTTP thread only reads output files and forwards pushes. The one shared map (page versions) is a `Mutex<HashMap>`, touched once per push and once per SSE connect.
4. **The live protocol.** Each note page carries `<meta name="tachylite-version" content="<16 hex>">`, a hash of the page. Its script opens `/.tachylite/events?page=<pathname>&v=<version>`. The server sends `event: page` with `id: <version>` and the full new HTML, or `event: gone` when the note is deleted. When a client connects, or reconnects after a lost connection, the server compares the client's version with the current one. The reconnect version is the browser's `Last-Event-ID`, which overrides `v`. If they differ, the server sends the current page first. This catches what a sleeping phone missed, with no history on the server. The client swaps `<main>`, the title and the version. On `gone` it reloads, which shows the 404.
5. **Unchanged pages are neither written nor pushed.** The site keeps each page's version. A save that changes nothing costs one render.
6. **Safe reads need Linux 5.6+.** Every vault read uses `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` and `O_NONBLOCK`, then `fstat`. A symlink in the path, a `..` escape and a FIFO swapped in after the scan are all refused without a race (#16). On an older kernel, `serve` and `build` stop with a clear error. Raspberry Pi OS ships 6.x kernels.
7. **One clash rule for every output path (#17).** Each published file claims one URL and one output path. Two claims clash if they share a URL (`x.html.md` and `x.html`), or if one output path is a folder of the other (`x.md` gives `pages/x.html`, and `x.html/y.md` needs a folder `pages/x.html/`). The first claim in byte order wins, and the loser is skipped as `UrlClash`. The site recomputes winners from all entries on every change. That costs well under a millisecond at 2,000 notes, and incremental output provably matches a full build. #17's third case, a vault bind-mounted under the output, stays open in #17. It needs a mount to test, and nobody does it by accident.
8. **The browser suite runs in `make check`,** so a local run matches CI. It needs Node locally. `make browser` installs Chromium on first run.
9. **Port.** `serve` takes `--port <n>`, default `7420`. Port `0` picks a free one, which the tests use. The bound address prints to stderr as `serving http://127.0.0.1:<port>`.

## Global Constraints

- Tachylite opens vault files read-only. Tests write only to temp vaults or the gitignored `browser/.tmp/`.
- The server binds `127.0.0.1` and nothing else. No flag changes that.
- Tests kill only a child they spawned (`Child::kill` on drop). Never kill by name.
- No sleeping. A test waits for the event it expects: an SSE event, a line on stderr, or a socket read with a deadline. Clippy bans `std::thread::sleep`; the browser suite uses Playwright's auto-waiting `expect`, never `waitForTimeout`.
- Output layout and routing follow the roadmap. Temp files are named `.<name>.tmp`. No URL can reach a segment that starts with `.`, except the first segment `.tachylite`, so a half-written file is never served.
- Budgets: file event to push ≤ 10 ms; save to screen ≤ 100 ms on localhost; idle CPU ~0 %; resident memory ≤ 50 MB on the budget vault. Timed budgets run at `BUDGET_SCALE` (3 in CI). Memory is absolute.
- No `unwrap` or `expect` outside tests. No `unsafe`. All CLI output goes to stderr.
- New crates come from `cargo add`, at the latest version, in the task that first uses them. The PR states each crate's binary-size and compile-time cost.
- Fixture and test content is synthetic.

## Review Focus

1. **Editor save patterns.** Write in place, write a temp file and rename it over the note, or a `git checkout` touching hundreds of notes at once. Expected: each page ends with the final content, and no client ever sees a half-written note. Task 6 tests the first two. Task 7's property test covers bursts.
2. **A phone that slept through pushes.** Expected: on reconnect it gets the current page at once. Tasks 6 and 8.
3. **Folders moving.** A folder renamed, deleted, created with notes already inside, or moved in from outside the vault. Expected: pages follow, with no stale output. If the vault root itself goes away, `serve` exits with a clear error. Tasks 6 and 7.
4. **Reserved URL characters in names.** `C# notes.md`, `50% off.md`, `a+b.md` and non-ASCII names. Expected: each serves at its percent-encoded path and gets its own pushes. Tasks 5 and 6.
5. **Swaps after the scan.** A note replaced by a symlink to a file outside the vault, or by a FIFO. Expected: the page is removed and the path is listed as skipped. Nothing outside the vault is published, and nothing hangs. Tasks 1 and 6.

Known limits, accepted for this slice:
- HTTP/1.1 allows six connections per origin. A desktop with more than five Tachylite tabs on plain localhost stalls. Behind `tailscale serve` the browser speaks HTTP/2, which has no such cap. Slice 6's service worker can share one stream if it matters.
- The server reads a requested file whole before sending it. Slice 6 owns serving, with compression and caching.

---

### Task 1: Safe vault reads

**Files:**
- Modify: `src/vault.rs`, `src/error.rs`, `src/build.rs`, `Cargo.toml`
- Test: `tests/vault_read.rs`

**Interfaces:**
- Produces, in `src/vault.rs`:
  - `pub struct VaultDir` holds the vault's directory fd and its canonical root path.
  - `pub fn VaultDir::open(root: &Path) -> Result<VaultDir, Error>`. It probes `openat2` once. On `ENOSYS` it returns `Error::NoOpenat2`.
  - `pub fn VaultDir::root(&self) -> &Path`
  - `pub fn VaultDir::open_file(&self, rel: &str) -> Result<File, ReadError>`: `openat2(fd, rel, O_RDONLY | O_NONBLOCK | O_CLOEXEC | O_NOCTTY, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)`, then `fstat`.
  - `pub fn VaultDir::read(&self, rel: &str) -> Result<Vec<u8>, ReadError>`: `open_file` plus `read_to_end`.
  - `pub enum ReadError { Missing, Skip(SkipReason) }`. `ENOENT` and `ENOTDIR` map to `Missing`, and so does a file that is not regular, because the scan ignores those silently. `ELOOP` maps to `Skip(Symlink)`. Every other error maps to `Skip(Unreadable(kind))`.
- Produces: `Error::NoOpenat2`, with the text `this kernel has no openat2 (Linux 5.6+); Tachylite needs it to read the vault safely`.

- [ ] **Step 0:** `cargo add rustix -F fs`.
- [ ] **Step 1: Write the failing tests** in `tests/vault_read.rs`, each on a tempdir vault:
  - `reads_a_note`: the bytes match.
  - `missing_file_is_missing`: `Err(ReadError::Missing)`.
  - `symlinked_note_is_skipped_as_symlink`: `link.md` points to a file outside the vault. Expect `Skip(Symlink)`.
  - `symlinked_parent_is_skipped_as_symlink`: `d` is a symlink to an outside folder holding `x.md`. Reading `d/x.md` gives `Skip(Symlink)`.
  - `dotdot_cannot_escape`: reading `../outside.md` is not `Ok`.
  - `fifo_is_missing_and_does_not_hang`: `mkfifo` at `pipe.md` (use `std::process::Command::new("mkfifo")`). Expect `Missing`, returned at once.
- [ ] **Step 2:** Run `cargo test --test vault_read`. It fails to compile: `VaultDir` doesn't exist yet.
- [ ] **Step 3: Implement** the interfaces above. `build` opens a `VaultDir` after the overlap check. It reads notes through `read` and copies html pages and assets by streaming from `open_file` with `io::copy` into the temp file (#18).
- [ ] **Step 4:** Run `cargo test`. Everything passes, slice 1's tests included.
- [ ] **Step 5: Commit** `feat(vault): read through openat2, never following a symlink`.

---

### Task 2: Page version and the live script tag

**Files:**
- Modify: `src/render.rs`, `tests/render.rs`, `tests/snapshots/render__welcome_snapshot.snap`
- Create: `src/live.js` (a stub for now; Task 6 writes it)

**Interfaces:**
- Produces: `pub struct Page { pub html: String, pub version: String }` and `pub fn render_note(title: &str, markdown: &str) -> Page`.
  - `version` is 16 lower-case hex digits. It comes from `std::hash::DefaultHasher` over the title and the rendered body, so it is deterministic for one binary.
  - The page head gains `<meta name="tachylite-version" content="{version}">` and `<script src="/.tachylite/live.js" defer></script>`.

- [ ] **Step 1: Write the failing tests:**
  - `version_changes_with_content`: two different notes give different versions.
  - `version_is_stable`: the same note gives the same version twice.
  - `page_carries_version_and_live_script`: the HTML holds the meta with `page.version` and the script tag.
- [ ] **Step 2:** Run `cargo test --test render`. It fails.
- [ ] **Step 3: Implement.** Update `build.rs` for the new return type. Accept the snapshot change with `cargo insta review`, checking that only the two head lines changed.
- [ ] **Step 4:** Run `cargo test`. It passes.
- [ ] **Step 5: Commit** `feat(render): page version and live script tag`.

---

### Task 3: The site

**Files:**
- Create: `src/site.rs`, `tests/site.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `classify`, `Entry`, `Kind`, `Skipped`, `SkipReason`, `ReadError` (Task 1), `render_note` and `Page` (Task 2).
- Produces, in `src/site.rs`:
  - `pub enum Change { Touch(String), Sync { under: String, entries: Vec<Entry> } }`
    - `Touch(path)` means the path may have changed. The site classifies it and reads its current state. A path that `classify` rejects is ignored.
    - `Sync { under, entries }` means that `entries` are every publishable file under the folder `under` (`""` is the vault root). Known paths under `under` that are not in `entries` are removed.
  - `pub trait Source { fn note(&mut self, path: &str) -> Result<Vec<u8>, ReadError>; fn file(&mut self, path: &str) -> Result<(), ReadError>; }`. `file` only checks that a regular file is there. `VaultDir` implements it.
  - `pub struct Site`, with `Site::new()` and `pub fn apply(&mut self, changes: Vec<Change>, source: &mut impl Source) -> Delta`.
  - `pub struct Delta { pub deletes: Vec<String>, pub writes: Vec<Write>, pub pushes: Vec<Push>, pub skipped: Vec<Skipped> }`. Paths are output-relative, such as `pages/people/Jane Doe.html` or `files/img/dot.png`.
  - `pub enum Write { Page { out: String, html: String }, Copy { out: String, from: String } }`
  - `pub enum Push { Page { key: String, version: String, html: Arc<str> }, Gone { key: String } }`. `key` is the note path without `.md`, the decoded URL path.
- Behaviour:
  - Winners are recomputed from every known entry on each `apply`, in byte order. A claim loses if its URL is taken, if its output path is a folder that holds a claimed output, or if one of its folders is a claimed output. A note's URL is its path without `.md`; any other file's URL is its full path. The rule is decision 7.
  - Something that starts winning, or a winning note whose version changed, produces a write and, for a note, a `Push::Page`. Something that stops winning, or is removed, produces a delete and, for a note, a `Push::Gone`. A note whose version is unchanged produces nothing.
  - `Missing` from the source removes the entry. `Skip(reason)` removes it and lists it in `skipped`.
  - `deletes` come before `writes` in application order. The edge applies them in that order.

- [ ] **Step 1: Write the failing tests** in `tests/site.rs`, using an in-memory `Source` over a `BTreeMap<String, Vec<u8>>`:
  - `full_sync_writes_every_page`: two notes and a PNG give two `Write::Page`, one `Write::Copy` and two `Push::Page`.
  - `unchanged_touch_writes_and_pushes_nothing`
  - `html_page_beats_note_with_its_url`: `x.html` and `x.html.md`. The html page wins, and the note is skipped as `UrlClash`.
  - `folder_page_clash`: `x.md` and `x.html/y.md`. `x.html/y.md` wins in byte order, and `x.md` is skipped.
  - `deleting_the_winner_promotes_the_loser`: `a.md` and `a.MD`. Remove `a.MD` from the source and touch it. `a.md` is now written.
  - `excluded_touch_is_ignored`: touching `.obsidian/x.md` gives an empty `Delta`.
  - Proptest `incremental_equals_full`:
    - Paths: `a.md`, `a.MD`, `x.html`, `x.html.md`, `x.md`, `x.html/y.md`, `d/n.md`, `d/p.png`, `d/e/m.md`, `.h.md`.
    - Contents: three short Markdown strings.
    - Ops: write a path, delete a path, delete folder `d`, and move `d` to `d2` (which changes the path set). File ops apply as `Touch`, folder ops as `Sync` of the folders involved.
    - Fold every `Delta` into an in-memory output map. A `Copy` takes the source bytes at apply time.
    - Assert the map equals a fresh `Site` given `Sync { under: "", entries: all }`.
    - Assert that, for each key, the last `Push` matches the final output: a page with that version, or gone.
- [ ] **Step 2:** Run `cargo test --test site`. It fails.
- [ ] **Step 3: Implement** `src/site.rs`.
- [ ] **Step 4:** Run `cargo test --test site`. It passes, with the proptest at its default case count.
- [ ] **Step 5: Commit** `feat(site): pure core that turns vault changes into an output delta`.

---

### Task 4: Build is the site applied to every file

**Files:**
- Create: `src/output.rs`
- Modify: `src/build.rs`, `src/main.rs`, `tests/build.rs`

**Interfaces:**
- Consumes: `Site`, `Change`, `Delta`, `Write` (Task 3), `VaultDir` (Task 1).
- Produces, in `src/output.rs`:
  - `pub fn prepare(out: &Path) -> Result<(), Error>`, moved from `build.rs` unchanged.
  - `pub fn apply(out: &Path, delta: &Delta, vault: &VaultDir) -> Result<Vec<Skipped>, Error>`.
    - Deletes come first. Each removes the file, then empty parent folders up to `pages/` or `files/`.
    - Writes come next, through `.<name>.tmp` and a rename. A `Copy` that finds its source gone is skipped, not fatal. The returned list holds those skips.
- Produces: `pub fn build(vault, out) -> Result<Summary, Error>`, with the same signature. It validates paths, opens the `VaultDir`, scans, and calls `prepare`. Then it applies `Change::Sync { under: "", entries }` and calls `output::apply`. `Summary.skipped` merges the scan's, the delta's and the output's skips, sorted.

- [ ] **Step 1: Write the failing tests** in `tests/build.rs`:
  - `note_shadowing_html_page_is_a_clash`: `maps/route.html.md` next to `maps/route.html`. `files/maps/route.html` is the html page byte for byte. No `pages/maps/route.html.html` exists, and the note is skipped as `UrlClash`.
  - `note_and_folder_page_clash_is_skipped`: `x.md` and `x.html/y.md`. The build succeeds, and one of them is skipped as `UrlClash`.
  - `no_temp_files_remain`: no file in the output starts with `.` except `.tachylite-output`.
- [ ] **Step 2:** Run `cargo test --test build`. The new tests fail.
- [ ] **Step 3: Implement.** Delete the direct render loop from `build.rs`; one code path remains.
- [ ] **Step 4:** Run `cargo test`, and `make budget` for the cold build. Every slice 1 test passes. The cold build stays ≤ 1 s; record the number.
- [ ] **Step 5: Commit** `refactor(build): build is the site applied to every file`.

---

### Task 5: `tachylite serve`, static

**Files:**
- Create: `src/serve.rs`, `tests/common/mod.rs`, `tests/serve.rs`
- Modify: `src/main.rs`, `src/lib.rs`, `tests/cli.rs`, `Cargo.toml`

**Interfaces:**
- Produces: `pub fn serve(vault: &Path, out: &Path, port: u16) -> Result<(), Error>`. It builds as in Task 4, binds `127.0.0.1:port`, prints `serving http://127.0.0.1:<port>` and serves until killed. Task 6 adds the watcher.
- Produces: the usage text:
  ```
  usage: tachylite build <vault> <out>
         tachylite serve <vault> <out> [--port <port>]
  ```
- Routing, from the roadmap:
  1. Percent-decode the path. Invalid UTF-8 is a 404.
  2. Reject with a 404 any empty, `.` or `..` segment, any NUL, a trailing `/` (except `/` itself), and any segment that starts with `.` other than a first segment `.tachylite`.
  3. `/.tachylite/live.js` serves `src/live.js`, embedded with `include_str!`. `/` serves `pages/.tachylite/home.html`, which is a 404 until slice 7.
  4. Otherwise serve `pages/P.html` if it exists, else `files/P`.
- Headers:
  - `Content-Type` by extension: `html` gives `text/html; charset=utf-8`; `js` gives `text/javascript`; the images, `svg` (`image/svg+xml`) and `pdf` get their usual types. Anything else is `application/octet-stream`.
  - `ETag: W/"<mtime ns hex>-<len hex>"`. A matching `If-None-Match` gets a 304.
  - `Cache-Control: no-cache`.
- `tests/common/mod.rs` holds a test helper with no HTTP client crate:
  - `Server::start(vault, out) -> Server` spawns the binary with `--port 0` and reads stderr until the `serving` line. Dropping it kills the child.
  - `get(addr, path, headers) -> Response { status, headers, body }` speaks HTTP/1.1 over `TcpStream`, with a 5 s read timeout.

- [ ] **Step 0:** `cargo add axum --no-default-features -F tokio,http1` and `cargo add tokio -F rt,net,sync,macros`.
- [ ] **Step 1: Write the failing tests** in `tests/serve.rs`, on a temp copy of the fixture vault:
  - `serves_note_at_its_vault_path`: `/people/Jane%20Doe` gives 200 and the rendered note.
  - `serves_asset_and_html_page_with_types`: `/img/dot.png` is `image/png` and byte-identical. `/maps/route.html` is the html page unchanged.
  - `reserved_characters_in_names`: notes `C# notes.md`, `50% off.md` and `café.md` serve at `/C%23%20notes`, `/50%25%20off` and `/caf%C3%A9`.
  - `rejects_bad_paths`: 404 for `/../x`, `/%2e%2e/x`, `/a//b`, `/a/./b`, `/people/`, `/.obsidian/app.json`, `/.tachylite-output` and `/welcome.html`.
  - `etag_gives_304`
  - `binds_loopback`: the printed address is `127.0.0.1`.
  - In `tests/cli.rs`, update `no_arguments_prints_usage_and_exits_2` for both usage lines.
- [ ] **Step 2:** Run `cargo test --test serve`. It fails.
- [ ] **Step 3: Implement.** `main` builds a `current_thread` tokio runtime and blocks on `axum::serve`. File reads use `std::fs`. A bind failure is a typed error, reported once by `main`.
- [ ] **Step 4:** Run `cargo test`. It passes.
- [ ] **Step 5: Commit** `feat(serve): serve the output on 127.0.0.1`.

---

### Task 6: Watch and push

**Files:**
- Create: `src/watch.rs`, `tests/live.rs`
- Modify: `src/serve.rs`, `src/live.js`, `tests/common/mod.rs`, `Cargo.toml`

**Interfaces:**
- Consumes: `Site`, `Change`, `Push` (Task 3), `output::apply` (Task 4), `VaultDir` (Task 1).
- Produces, in `src/watch.rs`: `pub fn run(vault: VaultDir, out: PathBuf, site: Site, pushes: broadcast::Sender<Arc<Push>>, versions: Arc<Mutex<HashMap<String, String>>>) -> Result<(), Error>`. It runs on its own thread and returns only on a fatal error.
  - Startup order: init inotify, then walk and add watches, then scan, then apply the full `Sync`. Events queued during startup are applied next. They are idempotent.
  - After the first full `Sync` is written, `run` signals ready on a `std::sync::mpsc` channel. Only then does `serve` bind and print the `serving` line, so no request sees a half-built output. This replaces Task 5's build-then-serve.
  - Watch mask per folder: `CLOSE_WRITE | MOVED_TO | MOVED_FROM | CREATE | DELETE | DELETE_SELF | MOVE_SELF | ONLYDIR | DONT_FOLLOW | EXCL_UNLINK`. Excluded folders are never watched.
  - Each blocking read is one batch. Map its events to changes:

    | Event | Change |
    |---|---|
    | `CLOSE_WRITE`, `MOVED_TO`, `MOVED_FROM`, `DELETE` on a file; `CREATE` on a non-folder (catches hard links and symlinks) | `Touch(path)`, de-duplicated per batch |
    | `CREATE` or `MOVED_TO` on a folder | Add watches for its subtree first, then `Sync { under: folder, entries: scan of it }` |
    | `MOVED_FROM` or `DELETE` on a folder | Remove its watches, then `Sync { under: folder, entries: [] }` |
    | `Q_OVERFLOW` | Re-walk the vault, re-add watches, `Sync { under: "", entries: full scan }` |
    | `DELETE_SELF` or `MOVE_SELF` on the vault root | Fatal: `Error::VaultGone(path)` |

  - Per batch: `site.apply`, then `output::apply`, then update `versions` and send each push. Skips print to stderr as `build` prints them.
- Produces, in `src/serve.rs`:
  - `GET /.tachylite/events?page=<pathname>&v=<version>` returns SSE. The `page` parameter is decoded by the routing decoder into a key.
  - The handler first subscribes to the broadcast. Then it reads the client's version from the `Last-Event-ID` header, else `v`. If the key is unknown, it sends `gone`. If the version differs from `versions[key]`, it sends `page` with the current file's HTML and `id` set to the current version. Then it forwards pushes for the key.
  - On `RecvError::Lagged`, it repeats the version check.
  - Keep-alive uses axum's default 15 s comment.
  - Build the stream with `futures_util::stream::unfold`. `cargo add futures-util`, which is already in the tree.
  - `serve` spawns the watch thread. If the thread returns an error, the server shuts down gracefully and `main` reports the error once.
- Produces: `src/live.js`, under 1.5 KB unminified. It reads the version meta and opens the `EventSource`.
  - On `page`, it parses the HTML with `DOMParser` and replaces `<main>`, `document.title` and the version meta.
  - On `gone`, it calls `location.reload()`.
  - It does nothing on a page without the meta.
- `tests/common/mod.rs` gains `subscribe(addr, page, version, last_event_id) -> Events` and `Events::next(deadline) -> Option<SseEvent { name, id, data }>`. The `deadline` is a socket read timeout, not a sleep.

- [ ] **Step 1: Write the failing tests** in `tests/live.rs`. Each one starts a server on a temp vault and waits on SSE events:
  - `edit_pushes_new_html`: write in place. The `page` event's data contains the new text, and its `id` matches the new file's version meta.
  - `atomic_rename_save_pushes_final_content`: write `.n.md.tmp`, then rename it over `n.md`. One push, with the final content.
  - `create_pushes_to_a_waiting_subscriber`: subscribe to `/new` first, then create `new.md`.
  - `delete_pushes_gone_and_removes_output`
  - `rename_note_moves_page`: the old key gets `gone`, and the new path serves 200.
  - `new_folder_with_notes_is_published`: `create_dir` then write `d/a.md` at once. `/d/a` arrives.
  - `rename_folder_moves_its_pages`
  - `unchanged_save_pushes_nothing`: subscribe to `a` and `b`. Rewrite `a` with the same bytes, then edit `b`. `b`'s push arrives, and `a`'s stream has seen no event. The `b` push is the barrier: inotify keeps order, and batches apply in order.
  - `excluded_edit_publishes_nothing`: write `.obsidian/x.md`, then a barrier edit. No output, and no push for `.obsidian/x`.
  - `symlink_swap_is_unpublished`: rename a symlink to an outside file over `s.md`. `gone` arrives, and stderr lists `s.md` as skipped.
  - `fifo_swap_does_not_hang`: rename a FIFO over `f.md`. `gone` arrives.
  - `stale_version_gets_current_page_on_connect`: connect with `v=0000000000000000`. The first event is `page` with the current HTML.
  - `last_event_id_overrides_v`
  - `vault_root_removed_exits_with_error`: rename the vault directory away. The child exits non-zero, and stderr names the vault.
- [ ] **Step 2:** Run `cargo test --test live`. It fails.
- [ ] **Step 3: Implement** `watch.rs`, the SSE route and `live.js`.
- [ ] **Step 4:** Run `cargo test`. It passes, with no test sleeping.
- [ ] **Step 5: Commit** `feat(live): watch the vault and push changed pages over SSE`.

---

### Task 7: Live equals full, and the live budgets

**Files:**
- Create: `tests/live_equals_full.rs`
- Modify: `tests/budgets.rs`, `tests/common/mod.rs`

- [ ] **Step 1: Write the tests:**
  - Proptest `live_output_equals_full_build`, at the live seam:
    - Cases: `LIVE_CASES`, default 8. Each case runs up to 12 operations on a temp vault while `serve` runs.
    - Operations: write, rewrite with the same bytes, delete, rename a note, create a folder with notes, rename a folder and delete a folder, plus a burst that writes 200 notes.
    - Paths come from Task 3's clash-prone alphabet.
    - Then edit the barrier note `zz-barrier.md` with a counter, and wait for its push.
    - Run `tachylite build` on the same vault into a fresh folder. Compare the two output trees file by file, byte for byte.
  - In `tests/budgets.rs`, all `#[ignore]`d like the cold build:
    - `push_latency`: `serve` on the budget vault. Edit one note 50 times, and time each from the end of the write to the `page` event. Assert the median ≤ 10 ms × `BUDGET_SCALE`, and print the median and the max.
    - `idle_cpu`: after the `serving` line and one push, read `utime + stime` from `/proc/<pid>/stat`. Hold an SSE stream open with a 5 s read deadline, then read again. Assert ≤ 2 ticks, and that no event arrived.
    - `resident_memory`: on the budget vault, after startup and one push, assert `VmHWM` in `/proc/<pid>/status` ≤ 50 MB. This one is absolute; `BUDGET_SCALE` does not apply.
- [ ] **Step 2:** Run `cargo test --release --test live_equals_full` and `make budget`. They pass. If the proptest finds a failure, its shrunk case becomes a named test in `tests/live.rs` before the fix.
- [ ] **Step 3: Commit** `test(live): live output equals a full build; push, idle and memory budgets`.

---

### Task 8: The browser seam

**Files:**
- Create: `browser/package.json`, `browser/package-lock.json`, `browser/playwright.config.ts`, `browser/serve.sh`, `browser/live.spec.ts`
- Modify: `Makefile`, `.gitignore`, `.github/workflows/ci.yml`

- [ ] **Step 1: Set up the suite.**
  - `npm init` in `browser/`, then `npm install -D @playwright/test@1.64.1`, pinned.
  - `serve.sh` resets `browser/.tmp/`, copies `tests/fixtures/vault` to `browser/.tmp/vault`, and execs `../target/release/tachylite serve .tmp/vault .tmp/out --port 7431`.
  - `playwright.config.ts`:
    - `webServer` runs `./serve.sh` and waits for `http://127.0.0.1:7431/welcome`.
    - Projects: `phone` (Playwright's `Pixel 7` device) and `desktop` (Desktop Chrome).
    - `workers: 1`. `screenshot: 'only-on-failure'` and `trace: 'retain-on-failure'`.
  - `.gitignore` gains `browser/node_modules/`, `browser/.tmp/`, `browser/test-results/` and `browser/playwright-report/`.
- [ ] **Step 2: Write the tests** in `live.spec.ts`. Each test writes its own note into `browser/.tmp/vault`, so tests never share state. A test waits for its page with `expect.poll` on the HTTP status, never a fixed wait.
  - `push swaps main`: open the note, rewrite it, and `expect(page.locator('main')).toContainText(<new text>)`.
  - `save to screen within budget`:
    - Throttle the CPU 4x with CDP `Emulation.setCPUThrottlingRate`.
    - An `addInitScript` `MutationObserver` on `main` records `performance.timeOrigin + performance.now()` at the swap.
    - Node records `Date.now()` before the write.
    - Assert the gap ≤ 100 ms × `BUDGET_SCALE`, and print it.
  - `catches up after a lost connection`: `context.setOffline(true)`, rewrite the note, `context.setOffline(false)`, then expect the new text.
  - `gone reloads to 404`: delete the note, and expect the response status 404 after the reload.
  - `no console errors`: collect `console` errors across the suite. Expect none.
- [ ] **Step 3: Wire it up.**
  - New Makefile target `browser`: `cargo build --release`, then `cd browser && npm ci && npx playwright install --only-shell chromium && BUDGET_SCALE=$(BUDGET_SCALE) npx playwright test`.
  - `check` runs `browser` last.
  - The CI `check` job gains these steps before `make check`: `actions/setup-node` (Node 24, with the npm cache keyed on `browser/package-lock.json`), and `npx playwright install --with-deps --only-shell chromium` in `browser/`.
  - On failure, an `actions/upload-artifact` step keeps `browser/test-results/`.
- [ ] **Step 4:** Run `make check`. It passes.
- [ ] **Step 5: Commit** `test(browser): Playwright seam for push, save-to-screen and catch-up`.

---

### Task 9: Docs

**Files:**
- Modify: `docs/superpowers/specs/2026-11-03-tachylite-design.md`, `README.md`, `docs/plans/2026-11-03-v1-roadmap.md`

- [ ] **Step 1: Rewrite the spec's Architecture steps 2 and 3.** inotify via `rustix`, one watch per published folder. Each read batch is applied once, with no timer, and only `CLOSE_WRITE` and `MOVED_TO` count as saves. Add a short **Live protocol** paragraph that matches decision 4. Record the Linux 5.6 floor in the release note of the Example deployment section.
- [ ] **Step 2: README.** Add a "Run it" section: `tachylite serve <vault> <out> [--port <port>]`, loopback only, with the output outside the vault.
- [ ] **Step 3: Roadmap.** Slice 2's row stays. Under Output layout, add the hidden temp-file names and the SSE query.
- [ ] **Step 4:** Run `make check`. It passes.
- [ ] **Step 5: Commit** `docs: live path in the design, README and roadmap`.

The PR body says `Closes #4`, `Closes #16` and `Closes #18`. It says `Refs #17`. Before merging, comment on #17 that its first two cases are fixed and the bind-mount case stays open.
