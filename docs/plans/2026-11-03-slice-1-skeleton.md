# Slice 1: skeleton. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `tachylite build <vault> <out>` renders every note in a vault to plain HTML and copies html pages and assets. The gate, CI, the musl builds, the fixture vault and the budget vault all exist.

**Architecture:** One crate with a library and a thin binary. The core is pure: `vault::classify` decides what to publish, and `render::render_note` turns Markdown into a page. `vault::scan` and `build::build` hold the file I/O. The budget vault generator is a library module, so tests, benches and an example share it.

**Tech Stack:** Rust stable 1.99, edition 2024. `comrak` 0.59 with default features off. Dev only: `insta`, `proptest`, `tempfile`. Error types are hand-written; no `thiserror`. The CLI parses `std::env::args`; no `clap`.

**Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`. Order and output layout: `docs/plans/2026-11-03-v1-roadmap.md`.

**Branch:** `feat/skeleton`. **Issue:** #3.

## Global Constraints

- Tachylite opens vault files read-only. Nothing in this slice writes inside the vault, tests included.
- Publish list: `.md` (rendered); `.html`; images `png`, `jpg`, `jpeg`, `gif`, `svg`, `webp`, `avif`; `pdf`. Extensions match case-insensitively. Every other file stays out.
- Default exclusions: any path segment that starts with `.`, and any segment equal to `target` or `node_modules`.
- Output layout, from the roadmap: `out/.tachylite-output` marker; `out/pages/<vault path without .md>.html`; `out/files/<vault path>`.
- Budgets: cold full build of 2,000 notes ≤ 1 s on the reference host. Release binary ≤ 10,000,000 bytes, stripped. CI runs timed budgets at `BUDGET_SCALE=3`.
- No `unwrap` or `expect` outside tests. No `unsafe`. Lints enforce both (Task 1).
- Fixture and generated content is synthetic. No real names, places or vault content.
- New crates come from `cargo add`, at the latest version, in the task that first uses them. The PR states each crate's binary-size and compile-time cost.
- Each integration test file starts with `#![allow(clippy::unwrap_used, clippy::expect_used)]`, so test helpers outside `#[test]` functions pass the lints.
- All CLI output goes to stderr.

## Review Focus

1. **A wrong output path.** The output is inside the vault, contains the vault, or is a full directory that isn't Tachylite's. Expected: a refusal before any write or delete. The vault stays untouched. Task 4 tests all three.
2. **A symlink in the vault pointing outside it,** for example to a home directory. Expected: not followed, and listed as skipped. Task 2.
3. **Odd names and bytes.** A non-UTF-8 file name is skipped and listed. A note with invalid UTF-8 renders lossily and does not stop the build. Tasks 2 and 4.
4. **Upper-case extensions** (`Notes.MD`, `photo.PNG`). Expected: published. Tasks 2 and 4.
5. **A note deleted between two full builds.** Expected: its page is gone after the second build. Task 4.

---

### Task 1: Crate, gate and CI

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `deny.toml`, `Makefile`, `.github/workflows/ci.yml`, `src/main.rs`, `src/lib.rs`, `src/error.rs`, `LICENSE-MIT`, `LICENSE-APACHE`, `README.md`
- Test: `tests/cli.rs`

**Interfaces:**
- Produces: `tachylite::Error`, an enum in `src/error.rs`, re-exported from `lib.rs`. It implements `Display` and `std::error::Error`, with `source()`. Variants: `Io { path: PathBuf, source: io::Error }`, `VaultNotDir(PathBuf)`, `Overlap { vault: PathBuf, out: PathBuf }`, `OutNotOurs(PathBuf)`, `OutNotClean(PathBuf)`. Display texts:
  - `Io`: `{path}: {source}`
  - `VaultNotDir`: `vault {path} is not a directory`
  - `Overlap`: `output {out} and vault {vault} overlap; the output must live outside the vault`
  - `OutNotOurs`: `output {path} is not empty and has no .tachylite-output marker; refusing to write there`
  - `OutNotClean`: `output {path} has . or .. after its last existing directory; give a plain path`
- Produces: the binary prints `usage: tachylite build <vault> <out>` to stderr and exits 2 on any other arguments.

- [ ] **Step 1: Scaffold.** Run `cargo init --name tachylite`. Add no crates yet: each task adds the crates it first uses, so `cargo machete` passes at every commit. Set these in `Cargo.toml`:
  - `edition = "2024"`, `rust-version = "1.99"`, `license = "MIT OR Apache-2.0"`, `repository = "https://github.com/olliegilbey/tachylite"`, and a one-line description.
  - `[profile.release]`: `lto = true`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
  - `[lints.rust] unsafe_code = "forbid"`, and `[lints.clippy] unwrap_used = "deny"`, `expect_used = "deny"`.

  `clippy.toml` sets `allow-unwrap-in-tests = true` and `allow-expect-in-tests = true`, and lists `std::thread::sleep` under `disallowed-methods`. `deny.toml` allows only permissive licences compatible with MIT OR Apache-2.0, and denies known advisories. `rust-toolchain.toml` sets `channel = "stable"` and `components = ["rustfmt", "clippy"]`.

- [ ] **Step 2: Write the failing test** in `tests/cli.rs`:

```rust
#[test]
fn no_arguments_prints_usage_and_exits_2() {
    let out = Command::new(env!("CARGO_BIN_EXE_tachylite")).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: tachylite build <vault> <out>"));
}
```

- [ ] **Step 3: Run it.** `cargo test --test cli` fails, because `cargo init`'s main prints "Hello, world!" and exits 0.

- [ ] **Step 4: Implement** `fn main() -> ExitCode` in `src/main.rs`. Any arguments that are not `build <vault> <out>` print the usage line and return `ExitCode::from(2)`. Add `src/error.rs` with `Error` as specified above.

- [ ] **Step 5: Write the Makefile.** Targets:
  - `check`, the gate CI runs: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo machete`, `cargo deny check`, `cargo test`, then `BUDGET_SCALE=$(BUDGET_SCALE) cargo test --release --test budgets -- --ignored`, with `BUDGET_SCALE ?= 3`.
  - `budget`: `BUDGET_SCALE=1 cargo test --release --test budgets -- --ignored --nocapture`.
  - `size`: `cargo build --release --target $(TARGET)`, with `TARGET ?= x86_64-unknown-linux-musl`. It prints the binary's byte count and fails above 10000000.
  - `upgrade`: `rustup update stable`, then `cargo upgrade --incompatible`, then `cargo update`.

  Until Task 7, `tests/budgets.rs` holds no tests, so create it empty now.

- [ ] **Step 6: Write `.github/workflows/ci.yml`.** It runs on pushes to `main` and on every pull request.
  - Job `check` on `ubuntu-24.04`: `actions/checkout`, `rustup toolchain install`, `Swatinem/rust-cache`, `taiki-e/install-action` with `tool: cargo-machete,cargo-deny`, then `make check`.
  - Job `musl`, a matrix of `{os: ubuntu-24.04, target: x86_64-unknown-linux-musl}` and `{os: ubuntu-24.04-arm, target: aarch64-unknown-linux-musl}`: checkout, toolchain, `rustup target add ${{ matrix.target }}`, cache, then `make size TARGET=${{ matrix.target }}`.

- [ ] **Step 7: Add the licences and README.** Copy the standard MIT and Apache-2.0 texts into `LICENSE-MIT` and `LICENSE-APACHE`; the MIT copyright line is `Copyright (c) 2026 Oliver Gilbey`. `README.md` gets four short sections:
  - What Tachylite is (the spec's first paragraph).
  - Status: pre-release, built in slices; see the `v1` milestone.
  - The stance: any Obsidian-style vault, PRs welcome.
  - Licence.

- [ ] **Step 8: Verify.** `make check` passes. `make size` prints a byte count under 10000000.

- [ ] **Step 9: Commit.** Stage the files by path. Message: `build(skeleton): crate, gate, CI and licences`.

---

### Task 2: Vault scan and the fixture vault

**Files:**
- Create: `src/vault.rs`, `tests/fixtures/vault/**`
- Test: unit tests in `src/vault.rs`; `tests/scan.rs`

**Interfaces:**
- Consumes: `Error` (Task 1).
- Produces, in `src/vault.rs` (each type derives `Debug, Clone, PartialEq, Eq`):
  - `pub enum Kind { Note, Html, Asset }`
  - `pub struct Entry { pub path: String, pub kind: Kind }`, where `path` is vault-relative with `/` separators.
  - `pub enum SkipReason { Symlink, NonUtf8Name, UrlClash }`
  - `pub struct Skipped { pub path: PathBuf, pub reason: SkipReason }`, where `path` is vault-relative.
  - `pub struct Scan { pub entries: Vec<Entry>, pub skipped: Vec<Skipped> }`, with entries sorted by `path`.
  - `pub fn classify(path: &str) -> Option<Kind>`: pure. It applies the publish list and the default exclusions.
  - `pub fn scan(vault: &Path) -> Result<Scan, Error>`

- [ ] **Step 0: Add crates.** `cargo add --dev proptest tempfile`.

- [ ] **Step 1: Write the fixture vault** in `tests/fixtures/vault/`. All content is synthetic.
  - `welcome.md`: YAML frontmatter (`title`, `tags`). Two heading levels. A GFM table. A task list using `[ ]` and `[x]`. A footnote. A fenced `rust` code block. The text `costs $5 or $10`. A strikethrough. A bare autolink. A raw `<script>alert(1)</script>` line.
  - `people/Jane Doe.md`: a short note (the space in the name is deliberate).
  - `Upper.MD`: a one-line note.
  - `maps/route.html`, a small HTML page with a `<script>`. `img/dot.png`, a valid 1x1 PNG. `docs/guide.pdf`, any bytes.
  - These must stay out of the output: `.obsidian/app.json`, `.env`, `.draft.md`, `scripts/tool.py`, `data/table.json`, `target/stale.md`, `node_modules/pkg/readme.md` and `notes/.hidden/secret.md`.
  - Common global gitignores drop `.env` and `node_modules/`, so stage the fixture vault with `git add -f`. The test `fixture_vault_has_exclusion_cases` asserts that each of these files exists, so a fixture that lost one fails loudly.

- [ ] **Step 2: Write the failing tests.**
  - Unit `classify_publish_list`, a table that expects:
    - `Note` for `a.md`, `A.MD` and `people/Jane Doe.md`.
    - `Html` for `maps/route.html`.
    - `Asset` for each image extension, in lower and upper case, and for `x.pdf`.
    - `None` for `x.py`, `.env`, `x.json`, `x.sql`, `x.txt`, `README` (no extension), `x.htm`, `.obsidian/a.md`, `a/.hidden/b.md`, `target/a.md`, `a/node_modules/b.md` and `.md`.
  - Proptest `classified_paths_are_published_and_not_excluded`. For paths built from segments drawn from `["a", "B c", ".git", ".x", "target", "node_modules", "targets"]` and a final name with an extension from `["md", "MD", "html", "png", "PDF", "py", "json", ""]`: `classify(p).is_some()` holds exactly when no segment starts with `.`, no segment is `target` or `node_modules`, and the lower-cased extension is on the publish list.
  - `tests/scan.rs::fixture_vault_entries`: `scan(fixture)` returns exactly these entries, in this order:
    - `Upper.MD` (Note)
    - `docs/guide.pdf` (Asset)
    - `img/dot.png` (Asset)
    - `maps/route.html` (Html)
    - `people/Jane Doe.md` (Note)
    - `welcome.md` (Note)
  - `tests/scan.rs::symlinks_are_skipped_not_followed`: in a tempdir vault, `link.md` is a symlink to a file outside, and `out/` is a symlinked dir to a folder holding `x.md`. Neither is an entry. Both are in `skipped` with `SkipReason::Symlink`.
  - `tests/scan.rs::case_clash_keeps_first`: a vault with `a.MD` and `a.md` yields one `Note` entry, `a.MD` (first in byte order). `a.md` is in `skipped` with `UrlClash`, because both would be served at the same URL.
  - `tests/scan.rs::non_utf8_names_are_skipped`: a file named with the bytes `b"bad\xff.md"`, created through `OsStr::from_bytes`, is in `skipped` with `NonUtf8Name`.

- [ ] **Step 3: Run them.** `cargo test vault` and `cargo test --test scan` fail: the functions don't exist.

- [ ] **Step 4: Implement** `classify` and `scan`.
  - `scan` recurses with `std::fs::read_dir`. It checks each directory segment against the default exclusions before descending, so an excluded tree is never read.
  - It uses `DirEntry::file_type()` and never follows a symlink.
  - It sorts entries by `path`. Of two notes whose paths differ only in the case of `.md`, the first in byte order is kept and the second is skipped as `UrlClash`.

- [ ] **Step 5: Run the tests.** `cargo test` passes.

- [ ] **Step 6: Commit.** Message: `feat(vault): scan with publish list and default exclusions`.

---

### Task 3: Render a note

**Files:**
- Create: `src/render.rs`
- Test: `tests/render.rs`, `tests/snapshots/`

**Interfaces:**
- Produces:
  - `pub fn note_title(path: &str) -> &str`: the file stem of the last segment. `people/Jane Doe.md` gives `Jane Doe`.
  - `pub fn render_note(title: &str, markdown: &str) -> String`: a complete HTML document.

- [ ] **Step 0: Add crates.** `cargo add comrak --no-default-features` and `cargo add --dev insta`.

- [ ] **Step 1: Write the failing tests** in `tests/render.rs`:
  - `welcome_snapshot`: `insta::assert_snapshot!(render_note("welcome", &fixture("welcome.md")))`.
  - `dollar_is_literal`: the output contains `costs $5 or $10`.
  - `frontmatter_is_not_in_body`: the output does not contain `title:` or `tags:`.
  - `raw_html_is_dropped`: the output does not contain `<script`. Raw HTML stays off until the sanitizer slice.
  - `title_is_escaped`: `render_note("a<b & \"c\"", "x")` contains `<title>a&lt;b &amp; &quot;c&quot;</title>`.
  - `gfm_renders`: the welcome output contains `<table>`, `type="checkbox"`, `<del>` and `class="footnotes"`.

- [ ] **Step 2: Run them.** `cargo test --test render` fails.

- [ ] **Step 3: Implement.** Use `comrak::markdown_to_html` with these `Options`:
  - `extension`: `strikethrough`, `table`, `autolink`, `tasklist` and `footnotes`, plus `front_matter_delimiter = Some("---".into())`.
  - `render.r#unsafe` stays `false`.

  Escape the title with `comrak::html::escape`. Wrap the body in this shell exactly. Later slices extend it.

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
</head>
<body>
<main>
{body}</main>
</body>
</html>
```

- [ ] **Step 4: Run the tests.** Run `cargo insta review`, read the snapshot, and accept it if it renders the fixture faithfully. `cargo test` passes.

- [ ] **Step 5: Commit.** Message: `feat(render): render a note to an HTML page`.

---

### Task 4: Full build

**Files:**
- Create: `src/build.rs`
- Test: `tests/build.rs`

**Interfaces:**
- Consumes: `scan`, `Kind`, `Skipped` (Task 2); `note_title`, `render_note` (Task 3); `Error` (Task 1).
- Produces:
  - `pub struct Summary { pub notes: usize, pub files: usize, pub skipped: Vec<Skipped> }`
  - `pub fn build(vault: &Path, out: &Path) -> Result<Summary, Error>`
  - `pub fn page_path(out: &Path, note: &str) -> PathBuf`: `out/pages/<note without its .md, matched case-insensitively>.html`.
  - `pub fn file_path(out: &Path, file: &str) -> PathBuf`: `out/files/<file>`.

- [ ] **Step 1: Write the failing tests** in `tests/build.rs`. Each test copies the fixture vault into a tempdir and builds to a sibling tempdir.
  - `builds_fixture`: `Summary { notes: 3, files: 3, .. }`. These all exist: `pages/welcome.html`, `pages/people/Jane Doe.html`, `pages/Upper.html`, `files/maps/route.html`, `files/img/dot.png`, `files/docs/guide.pdf` and `.tachylite-output`.
  - `output_holds_only_published_files`: walk the output. Every file is the marker, a `.html` under `pages/`, or a file under `files/` whose vault path `classify` accepts as `Html` or `Asset`.
  - `html_pages_and_assets_are_byte_identical`: each file under `files/` matches its vault source.
  - `vault_is_unchanged`: the vault's relative path, length, modified time and bytes are identical before and after the build.
  - `refuses_out_inside_vault`: `build(vault, vault/"site")` returns `Err(Error::Overlap { .. })`, and `vault/site` does not exist afterwards.
  - `refuses_vault_inside_out`: `build(dir/"v", dir)` returns `Err(Error::Overlap { .. })`.
  - `refuses_out_inside_vault_through_a_symlink`: `out` is a symlink to a folder inside the vault. Expect `Err(Error::Overlap { .. })`.
  - `refuses_dot_dot_after_missing_dir`: `build(vault, dir/"nope/../v/site")`, where `dir/v` is the vault, returns `Err(Error::OutNotClean(_))`. Neither `dir/nope` nor `vault/site` exists afterwards.
  - `refuses_foreign_non_empty_out`: the output holds `keep.txt` and no marker. Expect `Err(Error::OutNotOurs(_))`, and `keep.txt` survives.
  - `rebuild_drops_deleted_note`: build, delete `people/Jane Doe.md` from the vault copy, build again. `pages/people/Jane Doe.html` is gone.
  - `invalid_utf8_note_still_renders`: a note holding `b"ok \xff ok"` produces a page that contains `ok`.

- [ ] **Step 2: Run them.** `cargo test --test build` fails.

- [ ] **Step 3: Implement `build`.**
  1. Error with `VaultNotDir` unless the vault is a directory. Canonicalise it.
  2. Resolve the output without creating it. Canonicalise its deepest existing ancestor, then re-append the missing components. A missing component that is `.` or `..` is an error, `OutNotClean`, because the check could not see where it leads. Error with `Overlap` if either path starts with the other.
  3. If the output exists, is not empty, and has no `.tachylite-output`, error with `OutNotOurs`.
  4. Otherwise create the output and the marker, and remove `pages/` and `files/` if present.
  5. `scan` the vault. For each `Note`, read the bytes, decode with `String::from_utf8_lossy`, render, and write to `page_path`. For each `Html` or `Asset`, `std::fs::copy` it to `file_path`.

  Every write goes through one private `write_file(path, bytes)`. It creates the parent directories, writes `<path>.tmp`, then renames, so a reader never sees half a file. Map every `io::Error` to `Error::Io` with its path.

- [ ] **Step 4: Run the tests.** `cargo test` passes.

- [ ] **Step 5: Commit.** Message: `feat(build): full build to the output layout`.

---

### Task 5: The `build` command

**Files:**
- Modify: `src/main.rs`
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: `build`, `Summary` (Task 4).

- [ ] **Step 1: Write the failing tests** in `tests/cli.rs`:
  - `build_command_builds_fixture`: `tachylite build <fixture copy> <tempdir out>` exits 0. The stderr contains `built 3 notes, 3 files, 0 skipped`, and `pages/welcome.html` exists.
  - `build_error_exits_1`: `tachylite build <missing path> <out>` exits 1. The stderr starts with `error: `.

- [ ] **Step 2: Run them.** `cargo test --test cli` fails.

- [ ] **Step 3: Implement.** `main` calls `build`.
  - It prints one `skipped <path>: <reason>` line per skipped entry to stderr, where `reason` is `symlink`, `non-UTF-8 name` or `URL clash`.
  - It then prints `built {notes} notes, {files} files, {skipped} skipped`.
  - On `Err(e)`, it prints `error: {e}` once and returns exit code 1.

- [ ] **Step 4: Run the tests.** `cargo test` passes.

- [ ] **Step 5: Commit.** Message: `feat(cli): tachylite build`.

---

### Task 6: Budget vault generator

**Files:**
- Create: `src/budget_vault.rs`, `examples/budget_vault.rs`
- Test: unit tests in `src/budget_vault.rs`

**Interfaces:**
- Produces:
  - `pub fn generate(seed: u64, count: usize) -> Vec<(String, String)>`: vault-relative path and Markdown, sorted by path.
  - `pub fn write(dir: &Path, seed: u64, count: usize) -> io::Result<()>`
  - The example: `cargo run --release --example budget_vault -- <dir> [seed=1] [count=2000]`. It refuses a non-empty `<dir>`.

- [ ] **Step 1: Write the failing tests:**
  - `same_seed_same_vault`: `generate(1, 200) == generate(1, 200)`.
  - `different_seed_differs`: `generate(1, 200) != generate(2, 200)`.
  - `budget_vault_size`: `generate(1, 2000)` has 2,000 entries. Its total content length is between 18,000,000 and 22,000,000 bytes.
  - `paths_unique_and_publishable`: all paths are distinct, and `classify` gives `Some(Kind::Note)` for every one.
  - `exercises_link_forms`: across `generate(1, 2000)`, there is at least one of each: `[[../`, a root path with `/`, a bare `[[name]]`, `|label]]`, `#heading]]`, and a link to a note that doesn't exist.
  - `exercises_syntax`: across the vault there are at least 10 `README.md` files in different folders, and all four task states (`- [ ]`, `- [/]`, `- [x]`, `- [-]`). There are also `$` prices, fenced code, tables, `> [!note]` callouts, `==highlight==`, `%%comment%%`, `#tags`, frontmatter with `aliases`, and a `daily/` folder of `YYYY-MM-DD.md` notes.

- [ ] **Step 2: Run them.** `cargo test budget_vault` fails.

- [ ] **Step 3: Implement.** The generator must not depend on `rand`, whose output changes between releases. Use splitmix64:

```rust
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
```

  Shape:
  - About 50 folders, nested up to 3 deep, plus `daily/` holding about 300 notes.
  - Note sizes skewed between 2 KB and 30 KB, averaging about 10 KB.
  - Words come from syllables, so no real text enters the repo.

  A change to the generator moves every budget number. Say so in the PR that makes one.

- [ ] **Step 4: Run the tests.** `cargo test` passes. Then `cargo run --release --example budget_vault -- /tmp/<fresh dir>` writes 2,000 notes.

- [ ] **Step 5: Commit.** Message: `test(budget): seeded budget vault generator`.

---

### Task 7: Cold build budget

**Files:**
- Modify: `tests/budgets.rs`
- Possibly modify: `src/build.rs`

**Interfaces:**
- Consumes: `budget_vault::write` (Task 6), `build` (Task 4).

- [ ] **Step 1: Write the test:**

```rust
#[test]
#[ignore = "budget: run with make budget or make check"]
fn cold_full_build_2000_notes() {
    // generate(1, 2000) into a tempdir, time build() once, print ms and limit
    assert!(elapsed <= Duration::from_secs_f64(1.0 * scale()));
}
```

  `scale()` reads `BUDGET_SCALE` and defaults to `1.0`.

- [ ] **Step 2: Run it.** `make budget`. Record the milliseconds.

- [ ] **Step 3: Parallelise only if it is over 1 s.** Render notes in parallel inside `build`: split the entries into `std::thread::available_parallelism()` chunks under `std::thread::scope`. Add no crate for this. Run `make budget` again. CI's runners have 4 vCPUs against the reference host's 16, so check the CI run's printed time sits well under its 3 s ceiling. If it doesn't, raise it in the PR.

- [ ] **Step 4: Verify.** `make check` passes.

- [ ] **Step 5: Commit.** Message: `test(budget): cold full build budget`.

---

### Finish

- [ ] Run `make check` and `make size` green. Then run `make budget` on the reference host.
- [ ] Push `feat/skeleton` and open a PR that closes #3. The body follows AGENTS.md:
  - the problem, and the fix;
  - the budgets: cold build in ms, and binary size in bytes for both musl targets;
  - the cost of each crate in binary size and compile time;
  - the "Hit every path" entries that applied (full build and exclusion; the rest arrive in later slices);
  - the model and harness.
