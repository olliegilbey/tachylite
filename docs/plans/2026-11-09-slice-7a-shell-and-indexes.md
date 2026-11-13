# Slice 7a: page shell and index pages

**Issue:** #9, first half. **Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`, "Navigation" and "Look and feel".

**Goal:** the maintainer can install the app on a phone and use it. It opens on a home page, every page is styled, and any note is two taps or a few keystrokes away, offline.

Slice 7b brings the rest of #9: `/.tachylite/today`, `/.tachylite/recent/<folder>`, pins and the `title` config.

## Global constraints

- Typical note page, HTML + CSS + JS, compressed: ≤ 30 KB, no web fonts.
- Dark mode first, light from `prefers-color-scheme`. The system font stack, and a monospace stack for code. No animation.
- Index pages live under `/.tachylite/`, except the home page at `/`.
- An excluded note reaches no index page and no switcher entry.
- One code path: a full build and an incremental one derive index pages the same way.

## Decisions

- **The shell.** Every page, note or index, shares one template:
  - The head links `/.tachylite/style.css`.
  - The body is `<header>`, holding Home, Recent and a Find button, then `<main>`.
  - A note page starts its `<main>` with breadcrumbs to its folders' index pages. A push swaps `<main>`, so the crumbs follow a rename.
- **The stylesheet** is embedded and served from memory. The service worker precaches it with the shell. It styles what the render emits:
  - callouts, properties, tags, broken links, embeds, footnotes, backlinks, code, tables, Mermaid;
  - the header and the switcher.
- **Index pages are derived, not tracked.** At the end of every apply, the site builds every index page from its state:
  - the winners;
  - the titles;
  - each note's modified time.
  It compares each page's version with the last one, writes and pushes only the pages that changed, and deletes the pages of folders that are gone. That costs O(notes) per apply and needs no dependency tracking, which is exactly what keeps it correct incrementally. It runs after the note pages' pushes, so the push budget for the changed note is untouched. The cost is measured. Gating it on what changed waits until a number asks for it. Tag pages, added later, are gated: an apply rebuilds only the pages of the tags that changed.
- **The pages:**
  - `/`: the 10 most recent notes with a link to all recent, then the root's folders, notes and html pages.
  - `/.tachylite/folders/<folder>`: the folder's subfolders, notes and html pages, sorted by title, ignoring case.
  - `/.tachylite/recent`: the 50 most recent notes, newest first, each with its folder and date (UTC, `YYYY-MM-DD`). Equal times sort by path.
- **Modified time.** `Source::note` returns the note's bytes and its mtime in seconds. The vault read takes it from the `fstat` it already makes. A fresh build reads the same mtimes, so live equals full.
- **The quick switcher.**
  - `/.tachylite/notes.json` is `[[title, url, path, alias…], …]`, one entry per winning note. It is a page in the manifest, so sync keeps it offline.
  - The switcher is part of `live.js`, a `<dialog>`. Ctrl-K, Cmd-K or `/` outside a text field opens it, and so does the Find button.
  - It fuzzy-matches the query as a subsequence of the title, an alias or the path, and ranks the title first, then contiguous runs. It shows the top 20. Enter or a tap opens the first match.
- **Manifest.** Index pages and `notes.json` join the note pages in `pages`. Their URLs are `/` + the encoded key.

## Tasks

1. **Mtime.** `Source::note -> Result<Note, ReadError>`, where `pub struct Note { pub bytes: Vec<u8>, pub modified: i64 }`. The site keeps `modified` per note. Existing tests pass unchanged, with mtime 0 in the in-memory sources.
2. **Index pages and `notes.json`.** Add `render::render_index(title, body) -> Page` and the shell, and `Site::indexes(sink)`.
   - **Site tests:**
     - the home page lists root folders and notes, and the recent notes;
     - a folder index lists its children by title;
     - creating a note re-renders its folder index;
     - deleting a folder's last note deletes its index and pushes gone;
     - a title change re-renders the folder index;
     - recent orders by mtime, then path;
     - `notes.json` escapes a quote in a title;
     - the manifest lists every index page;
     - an excluded note is on no index page.
   - **Build test:** an excluded note's name appears in no generated file.
3. **Shell and stylesheet.** The header, crumbs and `style.css`, with a route for it, and SHELL in `sw.js`.
   - **Serve tests:** `style.css` is CSS; `/` is the home page.
   - **Snapshots:** updated.
   - **Page-weight test:** includes the CSS.
4. **Switcher.** In `live.js`.
   - **Browser tests:**
     - typing part of a title opens the note;
     - the switcher works with the server stopped;
     - the home page opens offline;
     - a wide table does not widen a phone page;
     - the header's links work.
5. **Docs.** The spec's Navigation section, for the derived index pages and the switcher. The README, for a test deployment as a systemd user service.

## Paths

- **Full build:** every index page.
- **Incremental:** create, edit, rename and delete re-derive index pages. The property tests cover them.
- **Push:** open index pages update.
- **Manifest and offline:** index pages and `notes.json` are listed.
- **Exclusion:** checked by test.
- **Phone:** the browser tests run in the phone project.
- **Search:** slice 9.
