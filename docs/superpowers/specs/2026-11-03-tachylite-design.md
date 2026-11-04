---
created: 2026-11-03
type: design
status: approved
---

# Tachylite: design

Tachylite serves a folder of Obsidian-style Markdown as a fast, offline-capable website. It is a read-only viewer. A phone on a bad connection is the main client.

The name: tachylite is a volcanic glass, a cousin of obsidian. It comes from the Greek *tachys*, "swift". It ends in "lite".

## Why

A typical user keeps a vault of a few thousand Markdown notes, and AI agents write most of them. The user reads the vault in a browser, on a laptop and on a phone, and rarely opens Obsidian. Obsidian is slow to navigate at that size: finding the latest daily note or meeting note takes too many steps. The user also wants richer notes, with maps, diagrams and static mockups inside them.

The vault format stays exactly as it is: Markdown, YAML frontmatter, `[[wikilinks]]`. Agents know this format from training data and need nothing new. Tachylite changes how the vault is read, not how it is written.

The main client is Chrome on an Android phone, with the site installed as a PWA. Desktop browsers come second. iOS is not a v1 target.

Success: the user opens a note on a phone with no signal, and it loads at once. An agent saves a note on the server, and the open page on the phone updates in about 100 ms.

## Goals

1. Render any Obsidian-style vault to HTML, close to how Obsidian shows it.
2. Make speed the first feature, and hold it with tests (see Performance budgets).
3. Work offline after the first sync: every note and full search, on the device.
4. Push live updates to open pages.
5. Show rich content: raw HTML in notes, and standalone `.html` files as embedded pages.
6. Make navigation fast: today's daily note, recent notes by folder, a quick switcher.
7. Run light. One small Rust binary, no runtime dependencies, comfortable on a Raspberry Pi Zero 2.

## Non-goals for v1

- Editing in the browser, including ticking a task. Every write needs conflict handling with edits synced from other machines, and a write path the tailnet can reach. It is a later version.
- Authentication or multiple users. The network boundary (Tailscale) is the access control.
- Graph view, canvas, themes, plugins.
- Public internet hosting.

## Anti-goals

These make Tachylite a failure even if it works:

- Tachylite writes to the vault. It reads the vault only. All output goes to a directory outside the vault.
- An excluded note reaches the output in any form: page, search index, backlink, manifest or metadata file. (Perlite, an existing PHP viewer, leaks the full vault structure through a `metadata.json`. Exclusion applies before anything is written.)
- A client JavaScript framework or a Node build step.
- A page needs the network after the first sync.
- The project grows into a general static-site framework.

## Approaches considered

1. **Static builder with live push (chosen).** A Rust binary renders the vault to static files, re-renders only what changes, and pushes updates to open pages over Server-Sent Events. Least to break, lowest resource use, files remain valid if the process stops.
2. **Live server that renders on request.** Always fresh, no output directory. It holds more work per request, and offline caching still needs the same manifest. More moving parts for no visible gain.
3. **Off the shelf: Quartz (Node) or Perlite (PHP).** Quartz rebuilds the whole site with Node on every change. Agents write all day, so it rebuilds all day. Perlite needs PHP and leaks metadata. Neither gives control over offline sync or latency.

Docusaurus was rejected early. It compiles notes as MDX, where a stray `<` or `{` in a hand-written note fails the build. It ships a React bundle and has no wikilinks.

Existing Rust prior art: `Mathijssch/oxidian` renders Obsidian Markdown to HTML. It has no licence, so Tachylite does not reuse its code.

## Decided

### Architecture

One binary, `tachylite`. It runs as a long-lived process:

1. Full build on start.
2. Watch the vault with inotify (the `notify` crate is acceptable).
3. On change: debounce, re-render the changed note, update the link index, re-render pages whose backlinks changed, update the manifest.
4. Push the new HTML of each changed page to subscribed clients over SSE.
5. Serve the output directory and the SSE endpoint over HTTP on `127.0.0.1`.

The binary serves the files itself, so it controls compression (precompressed Brotli and gzip), `ETag` and `Cache-Control`. Tailscale proxies one local port.

`tachylite build <vault> <out>` runs a single build and exits, for tests and CI.

### Rendering

- CommonMark plus GFM: tables, task lists, strikethrough, footnotes, autolinks.
- `comrak` is the expected Markdown crate. It has a wikilinks extension.
- YAML frontmatter shown as a properties block. `title`, `tags` and `aliases` are used for display and lookup.
- Wikilinks: `[[note]]`, `[[note|label]]`, `[[note#heading]]`. Obsidian's "new link format" setting writes three link forms, and a vault often mixes them. All must resolve: relative to the linking note (`[[../people/jane-doe]]`), from the vault root (`[[people/jane-doe]]`), and bare basename (`[[jane-doe]]`). Resolution order:
  1. A path starting `./` or `../` resolves relative to the linking note.
  2. A path with a `/` resolves from the vault root.
  3. A bare name matches by basename. When several files share it (`README` in many folders), the nearest one wins: same folder, then the fewest folder steps away. A tie is a broken link.
  4. Then aliases.
  An unresolved link renders as a visibly broken link. The build logs every broken and ambiguous link.
  An excluded note does not exist to the resolver. A link to it renders and logs exactly like a link to a missing note, so the output never reveals that it exists.
- URLs keep the vault path. A note drops `.md` and keeps its case, and the path is percent-encoded: `people/Jane Doe.md` is served at `/people/Jane%20Doe`. An `.html` page keeps its extension. There are no slugs. Two notes whose paths differ only in the case of `.md` (`a.md`, `a.MD`) would share a URL: the first in byte order wins, and the build logs the other.
- Everything Tachylite generates lives under one reserved prefix, `/.tachylite/`: index pages, the SSE endpoint, the manifest, the service worker and client assets. A vault path never starts with `.`, because dot names are always excluded, so no note can collide with a generated page. A vault may hold `tasks.md`, `Today.md`, or a folder note `people.md` beside `people/`, and each keeps its plain URL. The prefix mirrors `.obsidian`: a dot folder belongs to the app.
- `/` is the home page: the folder index of the vault root, with the pins at the top.
- Embeds: `![[image.png]]` shows an image. `![[note]]` transcludes a note, depth limited. `![[page.html]]` embeds a sandboxed iframe.
- Links to HTML: `[[page.html]]` and `[label](page.html)` open the page full screen, scripts running. The page runs exactly as written, with no Tachylite shell around it. The browser's back button returns to the note.
- Obsidian callouts (`> [!note]`) and `==highlight==`.
- `%%comments%%` are stripped and never published.
- No math rendering in v1; LaTeX is a later version. `$` is a literal character: notes are full of prices and shell snippets.
- Dataview, block references and image sizing are out of scope for v1.
- Tags: `#tag` in the body and frontmatter `tags`. Each tag gets an index page at `/.tachylite/tags/<tag>`.
- Mermaid blocks render on the client. The Mermaid script loads only on pages that contain a Mermaid block.
- Backlinks list at the foot of each note.

### Rich content and security

Notes are trusted content, but agents write them, and a prompt-injected agent could plant a script. A script running on the main origin could read every cached note. So:

- Inline HTML in Markdown passes through an allowlist sanitizer (the `ammonia` crate fits). It keeps layout and formatting tags and drops `script`, `iframe`, `object`, `embed`, event-handler attributes and `javascript:` URLs.
- Anything that needs JavaScript (a map, an interactive mockup) is a standalone `.html` file. It renders inside `<iframe sandbox="allow-scripts">`, with no `allow-same-origin`. It cannot read the main origin's storage or cache.
- Every `.html` file is also served with the header `Content-Security-Policy: sandbox allow-scripts`. Opened directly as a full page, it still runs in an opaque origin.
- Embedded `.html` pages may load from the network, for example map tiles or a library from a CDN. The service worker caches the `.html` file itself. Its external assets work offline only if the browser happens to have them cached. A map can show blank tiles with no signal; that is accepted.
- The main origin sends a Content-Security-Policy that blocks outbound requests (`connect-src 'self'`) and inline script.

### Navigation

- `/.tachylite/today` redirects to the newest daily note dated on or before today. The daily folder and date format come from `.obsidian/daily-notes.json` when present, else from config.
- `/.tachylite/recent` lists notes by modified time. `/.tachylite/recent/<folder>` scopes it to a folder, for example a meetings folder.
- Quick switcher on a keyboard shortcut and a visible button: fuzzy match on title, path and aliases, offline.
- Folder index pages mirror the vault tree, at `/.tachylite/folders/<folder>`.
- Pins: config lists links shown at the top of every page.

### Tasks

A vault already holds todo lists, so Tachylite shows them. It adds no format of its own.

- A task is an Obsidian checkbox line anywhere in a note: `- [ ]` todo, `- [/]` doing, `- [x]` done, `- [-]` cancelled.
- The note a task lives in is its task list. Line order is priority.
- `#tags` on a task line filter it. A `[[link]]` on the line points to detail.
- No due dates and no plugin fields (Tasks or Dataview syntax).
- `/.tachylite/tasks` is an index page of every open task. `/.tachylite/tasks/<tag>` filters by tag. A wide screen shows a board, with todo and doing columns. A narrow screen shows the same page as a list grouped by task list. CSS picks the layout; there is no toggle. Done and cancelled tasks collapse to a count per task list.
- Tasks are read-only. On a note page they render as disabled checkboxes. A task changes when someone edits its note.
- A public skill in the repo, `skills/tachylite-vault/SKILL.md`, tells an agent how to write tasks and notes that Tachylite renders well.

### Look and feel

Inspired by Obsidian's reading view and developer admin panels. Dark mode first, with a light theme from `prefers-color-scheme`. The system font stack, and a monospace stack for code. No web fonts, no decoration, no animation beyond instant state changes. Readable on a phone first, then on a desktop browser. Style is a means: the page should be functional and fast, and need little thought.

### Search

- Full-text search across all notes, offline, results as the user types.
- The index lives on the device with the notes.
- Search freshness may lag page freshness: the index updates within 5 s of a change.
- Pagefind (Rust, chunked index) is one candidate. A small custom index is another. Measure both against the search budget.

### What gets published

Only these file types reach the output: `.md` (rendered), `.html` (sandboxed), images (`png`, `jpg`, `jpeg`, `gif`, `svg`, `webp`, `avif`), and `pdf`. Every other file stays out, whatever its folder: code, `.json`, `.env`, `.sql`, scripts. Exclusion globs apply on top. An `.svg` is served as an image, never inline, so its scripts cannot run on the main origin.

### Offline sync

- A service worker caches the app shell, every note page and the search index.
- The build writes a manifest listing every page with a content hash.
- On reconnect, the client fetches the manifest and downloads only pages whose hash changed. A first sync is the whole set: a few MB compressed for a few thousand notes. Images and PDFs load on demand and stay cached once opened.
- The site is installable as a PWA. The service worker script lives under `/.tachylite/`, so the server sends it with `Service-Worker-Allowed: /` to let it control the whole site.

### Configuration

Optional `tachylite.toml` in the vault root:

- `exclude`: globs, added to the defaults. Any file or folder whose name starts with `.` is always excluded (`.git`, `.obsidian`, `.trash`, `.draft.md`); config cannot publish it, because the reserved prefix depends on it. `target/` and `node_modules/` are excluded by default. Excluded means never published. Tachylite may still read config from an excluded folder, such as `.obsidian/daily-notes.json`.
- `daily`: folder and date format, overriding the Obsidian config.
- `pins`: list of links.
- `title`: site name.

### Performance budgets

Budgets run against a synthetic vault of 2,000 notes and about 20 MB of text. A seeded generator builds it on demand; it is not committed. A small hand-written fixture vault is committed for snapshot tests.

| Budget | Target |
|---|---|
| File event to SSE push sent for the changed note itself | ≤ 10 ms |
| Dependent pages (backlinks, transclusions) and manifest updated and pushed | ≤ 250 ms |
| Save to screen, phone on a direct connection | ≤ 100 ms end to end. Headless Chrome measures it on localhost in CI; the reference phone, by hand |
| Cold full build, 2,000 notes | ≤ 1 s on a modern 16-core x86 machine |
| Typical note page, HTML + CSS + JS, compressed | ≤ 30 KB, no web fonts |
| Search, per keystroke, mid-range phone, offline | ≤ 50 ms |
| Idle CPU | ~0 % |
| Resident memory, 2,000-note vault | ≤ 50 MB |
| Release binary | ≤ 10 MB, stripped |

CI enforces the deterministic budgets as absolute limits: page weight, binary size and memory. Timed budgets are set on the reference host. `make budget` measures them there, and each PR reports the numbers before and after. CI also runs the timed benches with a ceiling at three times each budget, to catch gross regressions on noisy runners. Headless Chrome measures save-to-screen and search time in CI, on localhost with CPU throttling. Both are also measured by hand on the reference phone.

### Testing and QA

The binary is light. The repo is heavy on purpose: it carries every test and check that keeps the binary light. Three seams carry the tests, highest first:

- **Build seam.** A vault goes in, the output directory comes out. Snapshot tests of rendering, whole-output checks for exclusion and the publish list, page weight, the cold-build budget.
- **Live seam.** `tachylite serve` against a temp vault, over HTTP and SSE. Push latency, headers, idle CPU, memory. A property test applies random create, edit, rename and delete sequences and checks the live output equals a fresh full build.
- **Browser seam.** Playwright drives headless Chromium against a real server, emulating a mid-range phone and a desktop. Push to DOM, offline reads and sync, search latency under CPU throttling, layout, the sandbox, zero CSP violations, transfer sizes, and screenshots as CI artifacts. Playwright is test tooling only and never ships.

The reference phone and a Raspberry Pi are checked by hand each release. An agent skill guides an integrated QA pass in a real browser.

Release binaries are static musl builds for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, with no runtime dependencies. CI checks both builds. A Raspberry Pi is a portability aim, checked by hand once per release. It has no budgets of its own.

### Repository

- Public on GitHub as `olliegilbey/tachylite`. Licence: MIT OR Apache-2.0.
- Tests and screenshots use a synthetic fixture vault in the repo. No content from a real vault goes into code, issues, fixtures or images.
- The `tachylite-vault` skill is generic, for any agent writing any vault.
- README stance: for any Obsidian-style vault, tuned on one large, agent-written vault. PRs welcome. Generic means standard Obsidian features and its daily-notes config. Obsidian plugins are out of scope.

### Example deployment

The README documents this setup as the default route. It is an example, not built into the code.

- Host: a Linux devbox on Tailscale.
- `tachylite` runs as a systemd user service and binds `127.0.0.1`.
- `tailscale serve` proxies it on a tailnet-only HTTPS port. A port that carries a public Funnel route is public for every path on it, so Tachylite gets a port of its own.
- If the vault syncs to the host from another machine, edits made there add the sync delay before the file event.

## Left to the builder

- Crate choices beyond those named, and client code structure (vanilla JS, no framework). The HTTP stack is axum on a single-threaded tokio runtime.
- Debounce length, within the 10 ms push budget.
- The exact search implementation, chosen by measurement.
- Transclusion depth limit and cycle handling.
- How the 100 ms end-to-end figure is measured and recorded.
