# Slice 3: links and exclusion. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**
- `tachylite.toml` `exclude` globs hide notes from every output.
- Wikilinks and local Markdown links resolve in the spec's order. An excluded note counts as missing.
- Each note page lists its backlinks.
- Every broken or ambiguous link is logged.
- A change re-renders the pages that depend on it, within 250 ms.

**Architecture:**
- The site keeps a link graph: each note's raw link targets and the published paths they resolve to.
- Rendering splits in two:
  - The body is parsed once. Its links are resolved against the set of published paths, and its resolved targets are returned.
  - The page wraps the body with its backlinks, and its version hashes the whole page.
- A change marks three groups for rendering:
  - the notes it touched;
  - notes whose links could now resolve differently, because a published path with the same basename appeared or vanished;
  - notes whose backlinks changed.
- The site re-reads those notes through the `Source`. A full build is still a `Sync` of the root, so build and serve share this one code path.
- Exclusion moves into one value, `Exclude`. The scan, the site and the watcher all ask it.

**Tech Stack:**
- Rust stable 1.99, edition 2024.
- New runtime crates:
  - `toml` 1.2, default features off, features `std` and `parse`. It parses into `toml::de::DeTable` with no serde. It adds 125 KB to the release binary, measured.
  - `glob` 0.3. It adds 5.5 KB, measured.

**Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`, sections Rendering (wikilinks, URLs, backlinks), Configuration and What gets published. The roadmap is `docs/plans/2026-11-03-v1-roadmap.md`.

**Branch:** `feat/links-exclusion`. **Issue:** #5.

## Decisions to agree

Each decision takes the most obvious path.

1. **Config is `tachylite.toml` at the vault root, read with the `toml` crate.**
   - Slice 3 reads `exclude`, a list of glob strings. Unknown keys are ignored, because `daily`, `pins` and `title` arrive in later slices.
   - The file is read through `VaultDir`, so it gets the same read-only, no-symlink rules as every vault read.
   - A missing file means no extra excludes.
   - At startup, a file that does not parse stops `build` and `serve` with the file, line and message.
2. **Glob rules.**
   - A pattern matches the vault-relative path. `*` stays inside one segment, `**` crosses segments, and matching is case-sensitive.
   - A path is excluded when a pattern matches the path or any folder above it. So `private` and `private/**` both hide everything under `private/`.
   - The built-in exclusions are unchanged and cannot be turned off: dot names, `target/` and `node_modules/`.
3. **A config edit while serving is a full resync.**
   - A save of `tachylite.toml` reloads it, rewatches the tree and syncs the root. That is the overflow path from slice 2.
   - A broken edit is logged as `tachylite.toml: <error>; keeping the previous config`, and nothing else changes.
4. **Link matching follows Obsidian.**
   - The comparison is case-insensitive.
   - `.md` is optional for notes. Any other target needs its extension: `[[route.html]]`, `[[dot.png]]`.
   - The order is the spec's steps 1 to 3: relative, from the root, then the nearest bare name. A tie at the nearest distance is ambiguous, and it renders and logs as broken.
   - Step 4, aliases, needs frontmatter, so it arrives with slice 4.
5. **Markdown links to vault files resolve too.**
   - A Markdown link with no URL scheme, such as `[x](people/Jane%20Doe.md)` or `[x](../a.md#part)`, is percent-decoded and goes through the same resolver. This is the link Obsidian writes when wikilinks are off.
   - A link with a scheme, or one that starts with `/` or `#`, is left as written.
6. **Heading links.**
   - Headings get ids from comrak's anchorizer, the GitHub-style slug.
   - `[[note#Some Heading]]` links to `/note#some-heading`. `[[#Some Heading]]` links within the page.
   - A heading that does not exist is not checked; the link still goes to the note.
7. **Broken links.**
   - A broken link renders as `<a class="broken-link">label</a>`, with no `href`. Slice 7's CSS will style it.
   - It logs as `broken link in <note>: <target>`.
   - An ambiguous link logs as `ambiguous link in <note>: <target> matches <n> files`.
   - The log names only published paths, so it never names an excluded note.
8. **Backlinks.**
   - Every note page with at least one backlink ends with `<section class="backlinks"><h2>Backlinks</h2><ul>…</ul></section>` inside `<main>`.
   - Each linking note appears once, by title, sorted by path. A note's links to itself do not count.
   - Only notes link back. Html pages are not rendered, so they have no links.
9. **Embeds render as links for now.** `![[x]]` resolves like `[[x]]` and renders as a link. Slice 5 turns embeds into images, transclusions and iframes.
10. **Dependents are rendered in the same batch as the change.** Their pushes leave in the same apply, so the dependent-page budget is the push budget plus the extra renders. No queue and no second pass in time.

## Global Constraints

- Tachylite opens vault files read-only, through `VaultDir`, `tachylite.toml` included.
- An excluded note reaches no output file in any form: not a page, not a backlink, not a link target, not a log line.
- One code path: `build` stays `Site::apply` of a root `Sync`.
- No sleeping in tests. A test awaits the event it expects.
- No `unwrap` or `expect` outside tests. No `unsafe`.
- Budgets:
  - dependent pages pushed ≤ 250 ms after the save;
  - the slice 2 budgets still hold: event to push ≤ 10 ms, cold build ≤ 1 s, memory ≤ 50 MB, idle CPU ~0 %.
  - Timed budgets run at `BUDGET_SCALE`.
- New crates come from `cargo add` in the task that first uses them.
- Fixture and test content is synthetic.

## Review Focus

1. **A rename of a popular note.** Every linker re-resolves. Expected: the old links break, every linker's page updates, and the push lands within budget.
2. **A note created where a bare link was broken, or where it makes a link ambiguous.** Expected: the linkers re-render, and their broken state changes both ways. Task 4 tests both.
3. **Exclusion and the graph.** An excluded note links to a public one, or a public note links to an excluded one. Expected: no backlink from it, a link to it is broken, and nothing names it. Tasks 2 and 4.
4. **Folder moves.** A folder rename moves many notes at once, and relative links inside it keep working. Expected: the output equals a fresh full build. The live-equals-full property test, Task 6.
5. **Odd link text.** `[[a|label]]`, `[[ a ]]`, `[[a#]]`, `[[../../../../x]]` that climbs above the root, `[[People/JANE DOE]]` in another case, and percent signs in names. Expected: no panic, and each case resolves or breaks as decision 4 says. Task 3's property test.

---

### Task 1: Config and `Exclude`

**Files:**
- Create: `src/config.rs`, `tests/config.rs`
- Modify: `src/vault.rs`, `src/error.rs`, `src/lib.rs`, `src/build.rs`, `Cargo.toml`, `tests/build.rs`

**Interfaces:**
- Produces:
  - `pub struct Exclude` with `pub fn new(globs: &[String]) -> Result<Exclude, String>`, `pub fn path(&self, rel: &str) -> bool`, and `pub fn segment(&self, name: &[u8]) -> bool`, which applies only the built-in names.
  - `Exclude::default()` holds the built-ins only.
  - `pub struct Config { pub exclude: Exclude }` and `pub fn load(vault: &VaultDir) -> Result<Config, Error>`.
  - `Error::Config { message: String }`, displayed as `tachylite.toml: {message}`.
  - `classify(path, &Exclude)`, `scan(vault, &Exclude)` and `scan_under(vault, under, &Exclude)` take the exclude value. `excluded_segment` is gone.

- [ ] **Step 1: Write the failing build-seam tests** in `tests/build.rs`:
  - `config_excludes_globs`: `tachylite.toml` holds `exclude = ["private", "**/*.secret.md"]`. Then `private/a.md`, `notes/x.secret.md` and `notes/y.md` produce only `pages/notes/y.html`. No output file contains the excluded notes' marker text.
  - `bad_config_stops_the_build`: `exclude = [` makes `build` return `Error::Config`, and its message contains `line 1`.
  - `config_is_not_published`: `tachylite.toml` reaches no output file.
- [ ] **Step 2: Write the failing unit tests** in `tests/config.rs` for `Exclude::path`:
  - `private` hides `private/a.md` and `private/x/b.png`, but not `privateer.md`.
  - `*.md` hides `a.md` but not `x/a.md`.
  - `**/*.md` hides both.
  - Dot names, `target/` and `node_modules/` stay hidden with no config.
- [ ] **Step 3:** Run `cargo test --test build --test config`. Expected: they fail to compile, because the API does not exist yet.
- [ ] **Step 4: Implement.**
  - `cargo add toml --no-default-features --features std,parse` and `cargo add glob`.
  - `Exclude` compiles each glob with `glob::Pattern`. It matches with `MatchOptions { require_literal_separator: true, ..Default::default() }` against the path and each folder prefix.
  - `load` reads `tachylite.toml` with `VaultDir::read`. `ReadError::Missing` gives the default config.
  - `build` loads the config and passes `&config.exclude` to the scan and to `Site::new(exclude)`.
- [ ] **Step 5:** Run `cargo test`. Expected: all green.
- [ ] **Step 6: Commit** `feat(config): tachylite.toml exclude globs`.

### Task 2: Exclusion in the site and the watcher

**Files:**
- Modify: `src/site.rs`, `src/watch.rs`, `src/serve.rs`, `tests/site.rs`, `tests/live.rs`

**Interfaces:**
- Consumes: Task 1's `Exclude`, `Config` and `load`.
- Produces:
  - `Site::new(exclude: Exclude)` and `Site::set_exclude(&mut self, exclude: Exclude)`.
  - `Watcher::start` takes a `Config`.

- [ ] **Step 1: Write the failing live tests:**
  - `excluding_a_folder_unpublishes_it`: serve a vault with `d/n.md` and a subscriber on `/d/n`. Write `tachylite.toml` with `exclude = ["d"]`. Expect `gone`, and expect `/d/n` to return 404.
  - `unexcluding_republishes`: start with `exclude = ["d"]`, subscribe to `/d/n` with an empty version, and clear the config. Expect `page`.
  - `bad_config_edit_keeps_serving`: write `exclude = [`. Expect the stderr line `tachylite.toml: … keeping the previous config`. Then edit another note and expect its push.
  - `edit_inside_excluded_folder_is_ignored`: after an edit there, a barrier note's push arrives, and no output file changes.
- [ ] **Step 2:** Run `cargo test --test live`. Expected: the four new tests fail.
- [ ] **Step 3: Implement.**
  - The site classifies with its `Exclude`.
  - The watcher skips excluded names and folders with `Exclude::path`.
  - A root event naming `tachylite.toml` reloads the config.
    - On success, the watcher calls `set_exclude`, drops every watch, rewatches the tree and applies a root `Sync`.
    - On error, it reports the error once and changes nothing.
  - `serve` loads the config before `Watcher::start`, and a bad config stops it at startup.
- [ ] **Step 4:** Run `cargo test`. Expected: all green.
- [ ] **Step 5: Commit** `feat(live): reload tachylite.toml and resync`.

### Task 3: Link resolution

**Files:**
- Create: `src/links.rs`, `tests/links.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct Index`, the published paths, with `pub fn new<'a>(paths: impl IntoIterator<Item = &'a str>) -> Index`, `insert` and `remove`.
  - `pub fn key(target: &str) -> String`: the lowercase basename, with `.md` dropped. It names the index bucket a link or a path falls in.
  - `pub enum Resolved { Found(String), Missing, Ambiguous(usize) }`.
  - `pub fn resolve(index: &Index, from: &str, target: &str) -> Resolved`. `from` is the linking note's vault path, and `target` is the link text without `|label` or `#heading`.
  - `pub fn split(raw: &str) -> (&str, Option<&str>)`, which splits the path from the heading.

- [ ] **Step 1: Write the failing tests**, one per spec rule. The fixture paths are `people/Jane Doe.md`, `notes/README.md`, `README.md`, `notes/deep/README.md`, `maps/route.html` and `img/dot.png`.
  - Relative: from `notes/a.md`, `../people/Jane Doe` finds `people/Jane Doe.md`, and `./deep/README` finds the deep one.
  - From the root: `people/Jane Doe` finds it, and so does `People/jane doe`.
  - Nearest bare name:
    - From `notes/a.md`, `README` finds `notes/README.md`.
    - From `x/a.md`, it finds `README.md`.
    - From `notes/deep/b.md`, it finds `notes/deep/README.md`.
  - A tie: with `a/README.md` and `b/README.md` only, `README` from `c/x.md` is `Ambiguous(2)`.
  - Extensions: `route.html` finds `maps/route.html`, and `route` is `Missing`. `dot.png` finds the image.
  - Climbing above the root is `Missing`, and it does not panic.
- [ ] **Step 2: Write the property test.**
  - For random vaults of up to 12 paths and random links between them, `resolve` of a root path always finds that path.
  - A bare name gives `Found(p)` only when `p` is the unique nearest match, and `Ambiguous(n)` when `n ≥ 2` tie.
  - `key(link)` always equals `key(found path)`.
- [ ] **Step 3:** Run `cargo test --test links`. Expected: it fails to compile.
- [ ] **Step 4: Implement.**
  - `Index` holds a `HashMap<String, Vec<String>>` from key to paths, plus a lowercase-path to path map for root and relative lookups.
  - A path's distance from the linking note's folder counts the steps up to their common folder plus the steps down from it.
- [ ] **Step 5:** Run `cargo test --test links`. Expected: green.
- [ ] **Step 6: Commit** `feat(links): resolve wikilinks in the spec's order`.

### Task 4: Render links and backlinks; the site's link graph

**Files:**
- Modify: `src/render.rs`, `src/site.rs`, `src/report.rs`, `src/build.rs`, `src/watch.rs`, `tests/render.rs`, `tests/site.rs`, `tests/build.rs`, fixture notes under `tests/fixtures/vault`

**Interfaces:**
- Consumes: Task 3's `Index`, `resolve`, `key` and `split`.
- Produces:
  - `pub struct Body { pub html: String, pub links: Vec<String>, pub targets: BTreeSet<String>, pub broken: Vec<Broken> }`. `links` are the raw targets, and `targets` are the paths they resolve to.
  - `pub fn render_body(from: &str, markdown: &str, index: &Index) -> Body`.
  - `pub fn render_page(title: &str, body: &str, backlinks: &[(String, String)]) -> Page`. Each backlink is a title and a URL.
  - `pub struct Broken { pub from: String, pub target: String, pub matches: usize }`. `matches` is 0 for missing and n for ambiguous.
  - `Delta` gains `pub broken: Vec<Broken>`, and `report::broken(&[Broken])` prints it.
  - `render_note` is removed.

- [ ] **Step 1: Add fixture notes and their snapshots.**
  - `notes/links.md` uses every link form: wikilink with label, heading, relative, root, bare name, an `.html` page, an image, a Markdown link, a broken link and an embed.
  - `people/Jane Doe.md` gains a backlink from it.
  - `snapshot_links` and the updated `snapshot_jane` fail first, because the snapshots are new.
- [ ] **Step 2: Write the failing site tests** in `tests/site.rs`, over `Mem`:
  - `link_adds_backlink`: an edit to `a.md` that adds `[[b]]` writes and pushes both `a` and `b`, and `b` lists `a`.
  - `rename_target_breaks_links`: renaming `b.md` re-renders `a`, with `[[b]]` now broken and reported in `delta.broken`.
  - `create_fixes_broken_link`: creating `b.md` re-renders `a`, whose link now works.
  - `create_makes_link_ambiguous`: with `x/r.md` and `y/a.md` linking `[[r]]`, creating `z/r.md` at the same distance makes `a`'s link ambiguous.
  - `excluded_note_has_no_backlink_and_breaks_links`.
  - `unchanged_backlinks_render_nothing`: an edit that keeps the same links re-renders only the edited note.
- [ ] **Step 3: Extend the site property test.** `CONTENTS` gains link-bearing bodies such as `"[[a]]"`, `"[[d/n]] and [[../x]]"` and `"[[n]]"`. Incremental output must still equal a full build.
- [ ] **Step 4:** Run `cargo test`. Expected: the new tests fail.
- [ ] **Step 5: Implement `render_body`.**
  - Parse with comrak's arena, with wikilinks enabled (`wikilinks_title_after_pipe`) and `header_id_prefix: Some(String::new())`.
  - Walk the AST:
    - A `WikiLink` resolves its `url` with `split`, `resolve` and an anchor from `Anchorizer`.
    - A `Link` whose URL has no scheme and does not start with `/` or `#` is percent-decoded and resolved the same way.
  - A found target becomes a percent-encoded URL: a note's path without `.md`, or any other file's path as is.
  - A broken link becomes an `HtmlInline` `<a class="broken-link">` around its children.
  - Then `format_html`.
- [ ] **Step 6: Implement the graph in `Site`.**
  - New state: `links: HashMap<String, Vec<String>>` (raw targets by note), `out: HashMap<String, BTreeSet<String>>`, `inbound: HashMap<String, BTreeSet<String>>`, `by_key: HashMap<String, BTreeSet<String>>` (key to the notes whose raw links use it), and `index: Index` over the winners.
  - `apply` keeps its current steps until the winners are known. Then:
    1. Update `index` from the change in winners. The affected keys are those of every added or removed winner.
    2. Set `R1` to the notes read this apply, plus every winner note in `by_key` of an affected key. Read any of `R1` not yet in `fresh`.
    3. Render the body of each note in `R1`. Store its links and `out`, then diff `out` to update `inbound`. The changed targets are the notes whose inbound set changed.
    4. Set `R2` to the changed targets that are not in `R1`. Read them and render their bodies.
    5. Assemble pages for `R1 ∪ R2` with their backlinks. Write and push those whose version changed, as today.
    6. Drop the graph entries of removed notes.
  - A dependent that cannot be read follows the existing `probe` rules.
- [ ] **Step 7:** Run `cargo test`. Expected: all green, with the property test at its default cases. Accept the new snapshots after reading them.
- [ ] **Step 8: Commit** `feat(links): render links and backlinks; re-render dependents`.

### Task 5: Budgets and the live seam

**Files:**
- Modify: `tests/budgets.rs`, `tests/live.rs`, `tests/live_equals_full.rs`

- [ ] **Step 1: Write `dependent_push_latency`** in `tests/budgets.rs`.
  - On the budget vault, pick the note with the most inbound links. Subscribe to that note, and to a note that links to it.
  - Each round, edit the linker so it adds or drops the link. Time from the write to the target's `page` event.
  - Report the median and the max over 20 rounds. The limit is 250 ms × `BUDGET_SCALE`.
- [ ] **Step 2: Write the live tests:**
  - `new_link_pushes_backlink_to_target`.
  - `renamed_target_pushes_broken_link_to_linker`.
- [ ] **Step 3: Extend the live-equals-full property test.** `CONTENTS` gains link bodies, and the ops include a `tachylite.toml` write that excludes or includes `d`.
- [ ] **Step 4:** Run `make budget` and `LIVE_CASES=64 cargo test --test live_equals_full`. Expected: all budgets pass, and 64 cases pass.
- [ ] **Step 5: Commit** `test(links): dependent-page budget and live link tests`.

### Task 6: Browser and docs

**Files:**
- Modify: `browser/live.spec.ts`, `README.md`, `docs/superpowers/specs/2026-11-03-tachylite-design.md`, `docs/plans/2026-11-03-v1-roadmap.md`

- [ ] **Step 1: Browser test** `a backlink appears on an open page`. Open `target`, write a note that links to it, and expect `main` to contain the linker's title.
- [ ] **Step 2: README.** Add a `tachylite.toml` section showing an `exclude` example, with the glob rules from decision 2.
- [ ] **Step 3: Spec.** Under Rendering, note that aliases resolve from slice 4. Add the broken-link markup, the backlinks section and the Markdown-link rule.
- [ ] **Step 4:** Run `make check`. Expected: green.
- [ ] **Step 5: Commit** `docs: links, backlinks and tachylite.toml`.
