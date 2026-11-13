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
2. Watch the vault with inotify, through `rustix`: one watch per published folder. A new folder gets its watches, then a scan.
3. On change: read every queued event in one go and apply that batch once. No debounce: an interval would spend the push budget. A save is `CLOSE_WRITE` or `MOVED_TO`, never `MODIFY`, so an edit is read once, after it is saved. `ATTRIB` re-reads the path, so a permission change or a time copied by `cp -p` is seen. A folder is rescanned only when it becomes readable or unreadable, since `rsync -t` sets every folder's times. A path the site would not publish is ignored. A path's last create, delete, move or save in the batch decides its state. After `CREATE`, the path is held as mid-save: a regular file there with one link keeps the published page, in this batch and in any later one that re-renders it, until its save arrives. A file found by the scan of a new folder is held the same way, since its `CREATE` came before the folder's watch. A save by `link()` sends no `CLOSE_WRITE`, and nor does a file closed before its folder's watch existed, so a hold ends after one second and the path is read as saved. Only while a path is held does the watcher wait with a timeout; otherwise it blocks, so idle costs nothing. After `DELETE` or `MOVED_FROM`, a file found at the path is mid-save for that batch, as its `CREATE` is still queued. A symlink, FIFO or hard link gets no `CLOSE_WRITE`, so it is read at once. The checks run on the file the watcher would read. So no page is published from half of a new file within its hold. A note rewritten in place, as `tool > note.md` does, is read at its `CLOSE_WRITE` but may be read half written if another note's change re-renders it first. The batch re-renders the changed notes, updates the link index, re-renders pages whose backlinks changed, and updates the manifest. A config reload rewatches the tree without dropping a watch still in place, so an event queued behind the reload is still read.
4. Push the new HTML of each changed page to subscribed clients over SSE.
5. Serve the output directory and the SSE endpoint over HTTP on `127.0.0.1`.

The binary serves the files itself, so it controls compression, `ETag` and `Cache-Control`. It gzips each text response as it sends it. The service worker fetches a page once per change, so precompressing would save no CPU. Brotli is left out: it adds 1.1 MB to the binary and saves about 120 B on a typical page. Every response is `no-cache` with a content `ETag`, so a revalidation is a 304. Tailscale proxies one local port.

`tachylite build <vault> <out>` runs a single build and exits, for tests and CI.

**Live protocol.** Each note page carries `<meta name="tachylite-version">`, a hash of the page, and a script that opens `/.tachylite/events?page=<pathname>&v=<version>`. The server sends `event: page` with `id: <version>` and the full new HTML, or `event: gone` when the page is removed. On connect and on every reconnect, the server compares the client's version with the current one and sends the current page if they differ. On a reconnect the browser's `Last-Event-ID` gives the client's version and overrides `v`. So a phone that slept catches up, and the server keeps no history. The client swaps `<main>`, the title and the version. On `gone` it keeps listening, and after a second it shows a notice in `<main>`. Vim and `git checkout` save by removing the note and then writing it again. Their `page` event follows within milliseconds and cancels the notice, so the page neither flashes nor reloads.

### Rendering

- CommonMark plus GFM: tables, task lists, strikethrough, footnotes, autolinks.
- `comrak` is the expected Markdown crate. It has a wikilinks extension.
- YAML frontmatter shown as a properties block, every key in file order. `title` names the page and its backlink entries. `tags` render as tag links. A value that is exactly one `[[link]]` renders as a link and counts as a backlink, as in Obsidian.
  Tachylite reads the YAML subset that Obsidian's Properties support: scalars, flow and block lists, and `|` or `>` block scalars. Any other value, such as a nested map, shows as its raw text. A full YAML crate would be the largest dependency in the binary, for values Obsidian itself does not edit. Frontmatter is the block between a first line of `---` and the next line of `---`.
- Wikilinks: `[[note]]`, `[[note|label]]`, `[[note#heading]]`. Obsidian's "new link format" setting writes three link forms, and a vault often mixes them. All must resolve: relative to the linking note (`[[../people/jane-doe]]`), from the vault root (`[[people/jane-doe]]`), and bare basename (`[[jane-doe]]`). Resolution order:
  1. A path starting `./` or `../` resolves relative to the linking note.
  2. A path with a `/` resolves from the vault root.
  3. A bare name matches by basename. When several files share it (`README` in many folders), the nearest one wins: same folder, then the fewest folder steps away. A tie is a broken link.
  4. Then aliases, for a bare name only. The nearest note with the alias wins, and a tie is a broken link.
  Matching ignores case, and a note's `.md` is optional. Any other file needs its extension: `[[route.html]]`, `[[dot.png]]`. A link whose last segment names a folder (`[[people/..]]`) is missing.
  A Markdown link with no URL scheme, such as `[Jane](people/Jane%20Doe.md)`, is percent-decoded and resolved the same way. A link with a scheme, or one that starts with `/` or `#`, stays as written.
  `[[note#Heading]]` links to the heading's id, a GitHub-style slug. Tachylite writes the ids itself, without comrak's anchor link per heading.
  An unresolved link renders as `<a class="broken-link">label</a>`, with no `href`. The build logs every broken and ambiguous link: `broken link in <note>: <target>`, or `ambiguous link in <note>: <target> matches <n> files`.
  An excluded note does not exist to the resolver. A link to it renders and logs exactly like a link to a missing note, so the output never reveals that it exists.
- URLs keep the vault path. A note drops `.md` and keeps its case, and the path is percent-encoded: `people/Jane Doe.md` is served at `/people/Jane%20Doe`. An `.html` page keeps its extension. There are no slugs. Two notes whose paths differ only in the case of `.md` (`a.md`, `a.MD`) would share a URL: the first in byte order wins, and the build logs the other.
- Everything Tachylite generates lives under one reserved prefix, `/.tachylite/`: index pages, the SSE endpoint, the manifest, the service worker and client assets. Dot names are always excluded, so no vault path starts with `.`. No vault file can collide with a generated page. A vault may hold `tasks.md`, `Today.md`, or a folder note `people.md` beside `people/`. Each keeps its plain URL. The prefix mirrors `.obsidian`: a dot folder belongs to the app.
- `/` is the home page: the folder index of the vault root, with the pins at the top.
- Embeds: `![[image.png]]` shows an image, with the label as its alt text unless the label is an Obsidian size. `![[page.html]]` embeds a sandboxed iframe. `![[note]]` transcludes the note's body, without its properties, under a link to it. A heading embed (`![[note#Heading]]`), a pdf and any other file render as a plain link.
  Transclusion nests 3 levels deep, and a note transcludes once per page. A deeper embed, or a second embed of a note already on the page, renders as a link, so a cycle ends and three small notes cannot multiply into a huge page. An embed is a link: it resolves the same way, adds a backlink, and a missing or excluded target renders and logs as broken. The links inside an embedded note stay that note's own.
  When a note changes, every note that embeds it, at any depth, re-renders with it.
  A Markdown image with no scheme, `![alt](dot.png)`, resolves like a link.
- Links to HTML: `[[page.html]]` and `[label](page.html)` open the page full screen, scripts running. The page runs exactly as written, with no Tachylite shell around it. The browser's back button returns to the note.
- Obsidian callouts (`> [!note]`) and `==highlight==`.
- `%%comments%%` are stripped and never published. Obsidian does not specify every case, so Tachylite fails closed. Before parsing, it removes each comment from the note's text in note order: from a `%%` to the next `%%`, or to the end of the note. Fenced code and code spans keep their `%%`, unless a comment holds them. So a link inside a comment makes no backlink. If the parser still finds `%%` in text afterwards, the stripper and the parser disagree about code. The note's text is then withheld, and a notice shows in its place.
- No math rendering in v1; LaTeX is a later version. `$` is a literal character: notes are full of prices and shell snippets.
- Dataview, block references and image sizing are out of scope for v1.
- Tags: `#tag` in the body and frontmatter `tags`. A body tag starts a text run or follows whitespace. It holds letters, digits, `_`, `-` and `/`, with at least one non-digit. Each tag gets an index page at `/.tachylite/tags/<tag>`, keyed by the lowercased tag.
- Mermaid blocks render on the client. The Mermaid script loads only on pages that contain a Mermaid block. It is Mermaid's official tiny build, embedded gzipped: about 760 KB, against 1.6 MB for the full build. It has every diagram type except mindmap and architecture, and no KaTeX.
- Backlinks list at the foot of each note, in `<section class="backlinks">`, one entry per linking note, sorted by path. A note's links to itself do not count. When a link changes, the pages that depend on it re-render in the same batch: the linker, the old and new targets, and any note whose link now resolves differently.

### Rich content and security

Notes are trusted content, but agents write them, and a prompt-injected agent could plant a script. A script running on the main origin could read every cached note. So:

- Inline HTML in Markdown passes through Tachylite's allowlist sanitizer. It keeps layout and formatting tags and drops `script`, `iframe`, `object`, `embed`, `style` attributes, event-handler attributes and `javascript:` URLs. A link or image URL survives only with a web or mail scheme, as a path, or as a relative URL with no scheme and no character reference.
  The sanitizer rebuilds each raw HTML fragment from the allowlist: it emits only tags it builds itself, and escapes every other `<` and `>`. A tag it misreads is dropped, never run. A crate such as `ammonia` was measured and declined: it added 1.24 MB to the binary, an MPL-2.0 dependency, and 0.47 ms per note.
- Anything that needs JavaScript (a map, an interactive mockup) is a standalone `.html` file. It renders inside `<iframe sandbox="allow-scripts">`, with no `allow-same-origin`. It cannot read the main origin's storage or cache.
- Every vault file but a pdf, `.html` and `.svg` included, is served with the header `Content-Security-Policy: sandbox allow-scripts`. Opened directly as a full page, it runs in an opaque origin, so an `.svg` with a `<script>` cannot reach the main origin. A pdf keeps the main policy, since the browser's viewer needs it. Every response sends `X-Content-Type-Options: nosniff`, so no vault file loads as a script.
- Embedded `.html` pages may load from the network, for example map tiles or a library from a CDN. The service worker caches the `.html` file itself. Its external assets work offline only if the browser happens to have them cached. A map can show blank tiles with no signal; that is accepted.
- Markdown links and images with a script URL (`javascript:`, `vbscript:`, `file:`, `data:` but images) lose it, as comrak's own filter would.
- The main origin sends a Content-Security-Policy that blocks inline script and every outbound request but images: `default-src 'self'; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; form-action 'none'`. Mermaid injects `<style>`, so styles may be inline. Remote images in notes keep working.

### Navigation

- Every page shares one shell: a header with the site name, which links home, Recent and a Find button, then `<main>`. The home page's heading is for screen readers only, so the list starts near the top of a phone screen. A note page opens with breadcrumbs to its folders' index pages. A push swaps `<main>`, so the crumbs follow a rename.
- `/` is the home page: the 10 most recent notes, a link to all recent notes, then the vault root's folders, notes and html pages.
- `/.tachylite/folders/<folder>` mirrors one folder: its subfolders, then its notes and html pages. Titles and folder names that start with an ISO date (`YYYY-MM-DD`) come first, newest first, as daily notes and meeting notes are read newest first. The rest follow by title, ignoring case.
- `/.tachylite/recent` lists the 50 most recent notes by modified time, each with its folder and date (UTC). Equal times sort by path. `/.tachylite/recent/<folder>` scopes it to a folder, for example a meetings folder.
- Index pages are derived, not tracked. Every apply rebuilds each index page from the site's state, and writes and pushes only those whose version changed. That is O(notes) per apply, about 4 ms for 2,000 notes on the reference host, and it runs after the changed note's push. It needs no dependency tracking, so a full build and an incremental one cannot disagree.
- The quick switcher opens on Ctrl-K, Cmd-K, `/` outside a text field, or the Find button. It reads `/.tachylite/notes.json`, one `[title, url, path, alias…]` entry per note. That file is in the manifest, so the switcher works offline. A query matches as a subsequence of a title, an alias or the path. A title match ranks first, then fewer runs, then an earlier start. It shows the top 20, and Enter opens the first.
- `/.tachylite/today` redirects to the newest daily note dated on or before today. The daily folder and date format come from `.obsidian/daily-notes.json` when present, else from config.
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

Inspired by Obsidian's reading view and developer admin panels. Dark mode first, with a light theme from `prefers-color-scheme`. The dark theme is pure black for OLED screens, with warm text and cream links, as under a blue-light filter. The system font stack, and a monospace stack for code. No web fonts, no decoration, no animation beyond instant state changes. Readable on a phone first, then on a desktop browser. Style is a means: the page should be functional and fast, and need little thought.

### Search

- Full-text search across all notes, offline, results as the user types.
- The index lives on the device with the notes.
- Search freshness may lag page freshness: the index updates within 5 s of a change.
- Pagefind (Rust, chunked index) is one candidate. A small custom index is another. Measure both against the search budget.

### What gets published

Only these file types reach the output: `.md` (rendered), `.html` (sandboxed), images (`png`, `jpg`, `jpeg`, `gif`, `svg`, `webp`, `avif`), and `pdf`. Every other file stays out, whatever its folder: code, `.json`, `.env`, `.sql`, scripts. Exclusion globs apply on top. An `.svg` is served as an image, never inline, so its scripts cannot run on the main origin.

### Offline sync

- A service worker caches the app shell, every note page and the search index.
- The build writes `/.tachylite/manifest.json`, which lists every note page's URL and version. It follows the page writes of each apply, so a client that reads it finds pages at least that new. An `.html` page is not listed, because Tachylite copies it without reading it. It is cached on first open, like an image.
- A page asks the worker to sync on load and each time its event stream reconnects. The worker fetches the manifest, downloads the pages whose version changed, six at a time, and drops the pages that are gone. A first sync is the whole set: a few MB compressed for a few thousand notes.
- The worker serves every same-origin request stale-while-revalidate: the cached copy at once, and a fresh copy stored behind it. Images and PDFs load on demand and stay cached once opened.
- A page's version hashes its head template too, so an upgrade that changes the template re-syncs every client.
- The site is installable as a PWA. The web manifest and its two SVG icons are built into the binary. The second is maskable, so Android crops it to the launcher shape instead of drawing it on a white tile. The service worker script lives under `/.tachylite/`, so the server sends it with `Service-Worker-Allowed: /`, and the page registers it with `scope: '/'`. Both are needed for it to control the whole site.

### Configuration

Optional `tachylite.toml` in the vault root:

- `exclude`: globs, added to the defaults. Any file or folder whose name starts with `.` is always excluded (`.git`, `.obsidian`, `.trash`, `.draft.md`); config cannot publish it, because the reserved prefix depends on it. `target/` and `node_modules/` are always excluded as well. Excluded means never published. Tachylite may still read config from an excluded folder, such as `.obsidian/daily-notes.json`.
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
| Resident memory, 2,000-note vault | ≤ 65 MB |
| Release binary | ≤ 5 MB, stripped |

CI enforces the deterministic budgets as absolute limits: page weight, binary size and memory. Timed budgets are set on the reference host. `make budget` measures them there, and each PR reports the numbers before and after. CI also runs the timed benches with a ceiling at three times each budget, to catch gross regressions on noisy runners. Headless Chrome measures save-to-screen and search time in CI, on localhost with CPU throttling. Both are also measured by hand on the reference phone.

### Testing and QA

The binary is light. The repo is heavy on purpose: it carries every test and check that keeps the binary light. Three seams carry the tests, highest first:

- **Build seam.** A vault goes in, the output directory comes out. Snapshot tests of rendering, whole-output checks for exclusion and the publish list, page weight, the cold-build budget.
- **Live seam.** `tachylite serve` against a temp vault, over HTTP and SSE. Push latency, headers, idle CPU, memory. A property test applies random create, edit, rename and delete sequences and checks the live output equals a fresh full build.
- **Browser seam.** Playwright drives headless Chromium against a real server, emulating a mid-range phone and a desktop. Push to DOM, offline reads and sync, search latency under CPU throttling, layout, the sandbox, zero CSP violations, transfer sizes, and screenshots as CI artifacts. Playwright is test tooling only and never ships.

The reference phone and a Raspberry Pi are checked by hand each release. An agent skill guides an integrated QA pass in a real browser.

Release binaries are static glibc builds for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, linked with `crt-static`, with no runtime dependencies. They are not musl builds: musl's allocator serialises the render threads. On the budget vault, the musl cold build took 1.9-2.1 s against 0.33 s for static glibc. Static glibc costs about 0.9 MB of binary size over musl. CI checks both builds. A Raspberry Pi is a portability aim, checked by hand once per release. It has no budgets of its own.

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
- The host runs Linux 5.6 or later. Tachylite opens vault files with `openat2`, which confines every read to the vault, and exits with an error on an older kernel.

## Left to the builder

- Crate choices beyond those named, and client code structure (vanilla JS, no framework). The HTTP stack is axum on a single-threaded tokio runtime.
- Debounce length, within the 10 ms push budget.
- The exact search implementation, chosen by measurement.
- How the 100 ms end-to-end figure is measured and recorded.
