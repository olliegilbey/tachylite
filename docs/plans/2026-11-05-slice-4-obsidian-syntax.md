# Slice 4: Obsidian syntax. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Note pages show Obsidian's syntax as Obsidian's reading view does:
- frontmatter as a properties block;
- `title` as the page and backlink title, and `aliases` as link targets;
- callouts, `==highlight==` and `#tags`;
- `%%comments%%` removed;
- footnotes;
- Mermaid diagrams.

**Architecture:**
- comrak 0.59 already parses callouts (`ofm_callouts`), comments (`ofm_comments`) and highlight (`highlight`), so those are option switches.
- Tachylite adds four things:
  - a small frontmatter reader;
  - tag links;
  - a fail-closed pass for any `%%` comrak leaves in the text;
  - a Mermaid code-fence renderer.
- Aliases and titles join the link graph. A change to either re-renders the pages that depend on it, through the same `link()` pass as slice 3.
- The Mermaid script is vendored, gzipped and embedded. `live.js` loads it only when `<main>` holds a diagram.

**Tech Stack:** Rust 1.99, comrak 0.59. No new crates. Mermaid 12.3.0, the official `@mermaid-js/tiny` build, MIT.

**Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`, section Rendering. Roadmap row 4 in `docs/plans/2026-11-03-v1-roadmap.md`.

**Branch:** `feat/obsidian-syntax`. **Issue:** #6.

## Decisions

Each takes the most obvious path. They were measured or checked first.

1. **Frontmatter is read by a small YAML-subset reader, not a YAML crate.**
   - It reads what Obsidian's Properties support:
     - `key: value` scalars, plain or quoted;
     - flow lists `[a, "b c"]`;
     - block lists of `- item` lines;
     - `|` and `>` block scalars;
     - `#` comments.
   - Any other value, such as a nested map, shows as its raw text.
   - Frontmatter is the block between a first line of `---` and the next line of `---`. Without a closing line, there is no frontmatter.
   - Why: Obsidian's own Properties UI has the same limits, and a YAML crate would be the largest dependency in the binary.
2. **The properties block** is `<dl class="properties">`, at the top of the body, with every key in file order.
   - A list value is one `<dd>` holding its items, separated by commas.
   - `tags` items render as tag links.
   - A value that is exactly one `[[link]]` renders as a link and counts as a backlink, as in Obsidian.
3. **`title`** replaces the file stem in `<title>` and in backlink lists. A retitled note re-renders the pages it links to.
4. **`aliases`** (a list, or one scalar) are step 4 of link resolution.
   - They apply only to a bare name that steps 1 to 3 do not find.
   - Among several notes with the alias, the nearest note wins, and a tie is ambiguous.
   - An alias is a lookup key, like a basename. So a change to a note's aliases is handled like a path appearing or vanishing under that key.
5. **Tags.**
   - A tag is `#` at the start of a text run or after whitespace, followed by letters, digits, `_`, `-` and `/`, with at least one non-digit. Trailing `/` is dropped.
   - No tag forms in code, link text or a URL.
   - It renders as `<a class="tag" href="/.tachylite/tags/<lowercased tag>">#Tag</a>`. Slice 7 builds the tag pages.
6. **Comments fail closed.**
   - comrak removes `%%…%%` within one line and `%%` blocks, including an unclosed block, which runs to the end of the note.
   - Any `%%` comrak leaves in text outside code is a comment it did not close: one that wraps a line or a paragraph, or one that is never closed. Tachylite hides everything from it to the next such `%%`, or to the end of the note.
   - This runs before links are read, so a link inside a comment makes no backlink.
7. **Callouts** keep comrak's markup: `<div class="callout" data-callout=… data-callout-fold=…>`. A foldable callout shows its content, because folding needs JavaScript or `<details>`, and slice 7 owns the look.
8. **Mermaid.**
   - A fence with info `mermaid` renders as `<pre class="mermaid">` with the escaped source.
   - The script is `@mermaid-js/tiny` 12.3.0: 2.85 MB, or 759 KB gzipped. The full build is 5.5 MB. The tiny build drops mindmap and architecture diagrams and KaTeX.
   - It is committed gzipped as `src/vendor/mermaid.tiny.js.gz`, beside its licence. `make mermaid` re-fetches it from the npm registry and checks its sha512.
   - It is served at `/.tachylite/mermaid.js` with `Content-Encoding: gzip`, because every browser accepts gzip. It is not decompressed.
   - Cost: about 0.76 MB of binary, and a one-time 759 KB download per device, only on a page with a diagram.
   - `live.js` loads it on first load and after a pushed swap, only when `<main>` has a `pre.mermaid`. Then it calls `mermaid.run`. The theme follows `prefers-color-scheme`.
9. **Footnotes** are already on. Slice 4 adds a fixture case only.

## Global Constraints

- Tachylite opens vault files read-only. Tests use the fixture vault or temp vaults.
- Fixtures, test names and snapshots carry synthetic content only.
- An excluded note reaches no output in any form. That includes its aliases and its title.
- No `unwrap` outside tests. No `unsafe`. No sleeps in tests.
- One code path for full and incremental builds.
- `make check` green before any push.

## Review Focus

1. **A comment comrak does not close.** It wraps a line, crosses a paragraph, or is never closed. Its text and its links must reach no output file. Task 3 builds such a vault and checks every output file.
2. **An alias change on an incremental build.** Adding or removing an alias, or a new file whose basename beats an alias, must re-render every dependent. Task 2 adds aliases to both live-equals-full property tests.
3. **Odd YAML.** A nested map, a block scalar, a quoted value with `: ` or `#`, CRLF line ends, or an unclosed `---`. Each must show something sane and never panic. Task 1 tests each.
4. **Tag false positives.** `C#`, `https://x/#a`, code, `#123`, link text and heading ids. Task 3 tests each.
5. **A Mermaid block pushed into an open page.** The script loads and the diagram draws. Task 4's browser test does this.

---

### Task 1: Frontmatter, properties and titles

**Files:** Create `src/front.rs`, `tests/front.rs`. Modify `src/render.rs`, `src/site.rs`, `src/lib.rs`, `tests/render.rs`, `tests/site.rs`, the snapshots.

**Interfaces:**
- Produces:
  - `front::split(md: &str) -> (Option<&str>, &str)`: the YAML and the body.
  - `front::parse(yaml: &str) -> Vec<(String, Value)>`, where `enum Value { Text(String), List(Vec<String>) }`.
  - `front::Meta { title: Option<String>, aliases: Vec<String> }` and `front::meta(md: &str) -> Meta`.
  - `render_body` renders the properties block. comrak's `front_matter_delimiter` is turned off, and the body goes in after `split`.

- [ ] Write `tests/front.rs`. Tests: `scalars_plain_and_quoted`, `flow_and_block_lists`, `block_scalars`, `comments_and_colons_in_quotes`, `nested_map_is_raw_text`, `crlf`, `unclosed_fence_is_no_frontmatter`, `meta_reads_title_and_aliases` (a scalar alias, a list, and no aliases).
- [ ] Run `cargo test --test front`. Expected: fails to compile.
- [ ] Implement `src/front.rs`. Run again. Expected: pass.
- [ ] Write the render tests:
  - `properties_block_lists_every_key`: welcome's `title` and `tags`, with the tags as tag links.
  - `property_wikilink_resolves_and_counts_as_target`.
- [ ] Write the site tests:
  - `title_names_page_and_backlinks`;
  - `retitled_note_updates_backlink_text`: A links to B, A's title changes, and B's page is re-pushed with the new text.
- [ ] Run them. Expected: fail.
- [ ] Implement: `render_body` renders the block, and `Site` holds `titles: HashMap<Arc<str>, String>` for notes that set one. In `link()`, read `front::meta` for every fresh winner before rendering. A changed title adds the note's current targets to `changed`. Removing a note drops its title.
- [ ] Run `cargo test`. Review and accept the welcome snapshot change with `cargo insta`. Expected: green.
- [ ] Commit `feat(render): frontmatter properties and titles`.

### Task 2: Aliases

**Files:** Modify `src/links.rs`, `src/site.rs`, `tests/links.rs`, `tests/site.rs`, `tests/live_equals_full.rs`.

**Interfaces:**
- Consumes: `front::meta` from Task 1.
- Produces:
  - `Index::set_aliases(&mut self, path: &str, aliases: &[String])`, which replaces a path's aliases.
  - `Index::aliases(&self, path) -> &[String]`.
  - `Index::alias_bucket(&self, key) -> &[Arc<str>]`.
  - `resolve` takes step 4.

- [ ] Write the links tests:
  - `alias_resolves_bare_name`;
  - `basename_beats_alias`;
  - `alias_needs_bare_name` (`[[x/JD]]` is missing);
  - `alias_tie_is_ambiguous`;
  - `nearest_alias_wins`.
- [ ] Write the site tests:
  - `adding_alias_fixes_broken_link`;
  - `removing_alias_breaks_link`;
  - `new_file_named_like_alias_takes_the_link`;
  - `excluded_note_alias_does_not_resolve`.
- [ ] Run them. Expected: fail.
- [ ] Implement:
  - Before the dependents are gathered, compare each fresh winner's aliases with `index.aliases`. Each added or removed alias's folded form joins `keys`.
  - The dependents of a key also include the `inbound` of `alias_bucket(key)`.
  - A removed winner's aliases join `keys`, and are cleared with its path.
  - Set the new aliases after the dependents are gathered, and before pass 1.
- [ ] Add alias contents to `tests/site.rs` `incremental_equals_full` and to `tests/live_equals_full.rs`: `---\naliases: [jd]\n---\n` and `[[jd]]`, plus a retitled note.
- [ ] Run `cargo test` and `LIVE_CASES=64 cargo test --test live_equals_full`. Expected: green.
- [ ] Commit `feat(links): resolve aliases`.

### Task 3: Callouts, highlight, comments and tags

**Files:** Modify `src/render.rs`, `tests/render.rs`, `tests/build.rs`. Create `tests/fixtures/vault/notes/syntax.md` and its snapshot.

- [ ] Write `notes/syntax.md`: a callout with a title, a folded callout, a custom type, `==highlight==`, an inline comment, a block comment, tags in text and a heading, a footnote, and a Mermaid fence. Add a `syntax_snapshot` test.
- [ ] Write the render tests:
  - `tags_render_as_links`: `#a/b-c,` becomes the tag `a/b-c`; `#Über` gives href `/.tachylite/tags/%C3%BCber`.
  - `not_tags`: `C#`, `#123`, `` `#x` ``, `[#x](u)` and `https://e.com/#a`.
  - `stray_comment_hides_to_its_pair`: across a soft break, and across paragraphs.
  - `unclosed_stray_comment_hides_to_end`.
- [ ] Write the build test `comments_reach_no_output`: a temp vault whose comments hold the word `zebra` and a `[[target]]`, in every form above. No output file holds `zebra`, and `target`'s page has no backlinks.
- [ ] Run them. Expected: fail.
- [ ] Implement:
  - Turn on `ofm_callouts`, `ofm_comments` and `highlight`.
  - Hide stray comments first, in document order. Detach each node strictly between the opening and closing `%%` text, except ancestors of either end, and trim the two ends.
  - Turn tags into raw `<a>` tags while walking `Text` nodes, skipping text under `Link`, `WikiLink` and `Image`.
- [ ] Run `cargo test`. Expected: green.
- [ ] Commit `feat(render): callouts, highlight, comments and tags`.

### Task 4: Mermaid

**Files:** Create `src/vendor/mermaid.tiny.js.gz` and `src/vendor/MERMAID-LICENSE`. Modify `src/render.rs`, `src/serve.rs`, `src/live.js`, `Makefile`, `tests/serve.rs`, `browser/live.spec.ts`, the syntax snapshot.

- [ ] Add a `make mermaid` target. It fetches `@mermaid-js/tiny@12.3.0` from the npm registry, checks the tarball's sha512 against the registry's `integrity`, and writes the two files with `gzip -9n`. Run it and commit the output.
- [ ] Write the tests:
  - `mermaid_fence_renders_pre`;
  - serve test `mermaid_js_is_gzipped`: status 200, `content-encoding: gzip`, `text/javascript`, an ETag, and a body that is the embedded bytes;
  - browser test "a Mermaid block draws, and only its page loads the script": a page without a diagram never requests `mermaid.js`, and a pushed edit that adds a diagram draws an `svg`.
- [ ] Run them. Expected: fail.
- [ ] Implement:
  - A `CodefenceRendererAdapter` for `mermaid`.
  - The `/.tachylite/mermaid.js` route.
  - In `live.js`, a `diagrams()` function run on load and after every swap.
- [ ] Run `cargo test` and `make browser`. Expected: green.
- [ ] Commit `feat(render): Mermaid diagrams`.

### Task 5: Docs and budgets

- [ ] Update the spec: the alias step's rules, the fail-closed comment rule, Mermaid's tiny build, and the frontmatter subset.
- [ ] Update the README feature list.
- [ ] Run `make budget` and `make size`, and record the numbers.
- [ ] Run `make check`.
- [ ] Commit `docs: Obsidian syntax`.
