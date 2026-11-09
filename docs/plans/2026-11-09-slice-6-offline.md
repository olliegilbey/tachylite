# Slice 6: offline

**Issue:** #8. **Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`, "Architecture" (serving) and "Offline sync".

**Goal:** after one visit, every note page opens on the phone with no network, and a reconnect fetches only the pages that changed.

## Global constraints

- A service worker caches the app shell and every note page. The search index joins in slice 9.
- The build writes a manifest that lists every page with a content hash. An excluded note is not in it.
- On reconnect the client fetches the manifest and downloads only the pages whose hash changed.
- Images and PDFs load on demand and stay cached once opened.
- The service worker lives under `/.tachylite/`, is served with `Service-Worker-Allowed: /`, and is registered with `scope: '/'`.
- The site is installable as a PWA.
- Typical note page, HTML + CSS + JS, compressed: ≤ 30 KB.

## Decisions

- **gzip, compressed per response.** The spec says "precompressed Brotli and gzip". Both parts were measured:
  - **Brotli.** It adds 1.09 MB to the binary, which pushes the 5 MB size limit over the edge. It saves about 120 B on a 2 KB page. It is left out, and the maintainer decides.
  - **gzip.** `flate2` with its pure-Rust backend adds 61 KB. A 2 KB page compresses in 0.2 ms.
  - **Why per response.** The service worker fetches each page once per change per device. So compressing on the way out costs the same CPU as precompressing, and the writer, the full build and the delete path stay as they are.
  - **Scope.** The server gzips text responses (html, js, json, webmanifest, svg) when the request accepts gzip, and adds `Vary: Accept-Encoding`. The weak `ETag` stays the same for both encodings, as weak tags allow. `Cache-Control: no-cache` stays: the service worker is the cache, and every revalidation is a cheap 304.
- **The manifest is a site output.** `Site` already holds every note page's version.
  - After the page writes of an apply that changed a version, it writes `files/.tachylite/manifest.json`: `{"pages":{"<url>":"<version>",…}}`, sorted by URL.
  - The first apply always writes it, so an empty vault has one. It comes after the page writes, so a client that reads it finds pages at least that new.
  - The live-equals-full property covers it with no new code.
- **Html pages are not in the manifest.** The site copies an `.html` file without reading it, so it has no hash for it. An html page is cached on first open and revalidated in the background, like an asset. An html page never opened online is missing offline, which costs one embed iframe. Its content stays as written.
- **The page version covers the template.** Today it hashes the title and `<main>`. The head is hashed too, so a binary upgrade that changes the head changes every version, and clients re-sync.
- **Service worker strategy:**
  - **Install.** Precache the shell (`live.js`, the web manifest, the icon), then `skipWaiting`. Activate calls `clients.claim`.
  - **Fetch.** It handles same-origin GETs, except `/.tachylite/events`, `/.tachylite/manifest.json` and `/.tachylite/sw.js`. Every one is stale-while-revalidate: serve the cached copy, and refetch it in the background. A miss goes to the network and is stored. A miss with no network returns 503. Built as one path for pages and assets. A cache-first path for manifest pages would save a 304 per open, at the cost of a second path.
  - **Sync.** The page posts a `sync` message on load and on every SSE `open`. The worker then:
    - fetches the manifest;
    - fetches the pages whose version differs, 6 at a time;
    - deletes the pages that are gone;
    - stores the manifest with only the versions it got.
    One sync runs at a time. A failed page is retried on the next sync.
- **PWA.** `/.tachylite/app.webmanifest` and `/.tachylite/icon.svg` are embedded and served from memory. Each page's head links the web manifest. The headless shell skips Chromium's install check, so `Page.getInstallabilityErrors` is always empty. The test reads `Page.getAppManifest` instead: no errors, a name, a start URL, standalone display, an icon that loads, and a worker in control.

## Tasks

1. **Compression.** Add `flate2` (default features), and gzip in `vault_file`.
   - **Serve tests:**
     - a page requested with `Accept-Encoding: gzip` comes back gzipped, decodes to the plain body, and carries `Vary`;
     - a png is never gzipped;
     - a request without gzip gets the plain body;
     - the `ETag` is the same with and without gzip, and `If-None-Match` gives a 304 for both.
2. **Manifest.** `Site` writes it as `Write::Manifest { json }`, which `Writer` puts at `files/.tachylite/manifest.json`. Add `json` to `content_type`. Hash the head into the version.
   - **Site tests:** the manifest lists every note page with its version; an edit changes one entry; a delete removes one; an asset change writes no manifest.
   - **Build test:** an excluded note is not in the manifest.
   - **Serve test:** `/.tachylite/manifest.json` is JSON.
3. **Service worker and PWA.** Add `src/sw.js`, `src/app.webmanifest` and `src/icon.svg`, with routes for each. `sw.js` is served with `Service-Worker-Allowed: /`. `live.js` registers the worker and posts `sync`, and the head links the manifest.
   - **Serve test:** the headers of `sw.js`.
   - **Browser tests:**
     - a page opens with the server stopped;
     - a note never opened opens offline after one sync;
     - an edit made while the client was away arrives after reconnect;
     - a deleted page leaves the cache;
     - the app manifest meets the install criteria;
     - an image opened once loads offline.
4. **Page-weight budget.** A deterministic test in `tests/budgets.rs`, not ignored: the gzipped size of the median fixture page plus `live.js` is ≤ 30 KB. The weight of the budget vault's median page is printed too.
5. **Docs.** Rewrite the spec's serving line, the Offline sync section and the budget row to match these decisions.

## Paths

- **Apply:** full build (the manifest); incremental rebuild (the manifest follows every edit, create, rename and delete); manifest and offline (this slice); exclusion (the manifest); phone (the browser tests run in the phone project).
- **Push:** unchanged, but the push must not wait for the manifest write. It follows the page write as before.
- **Search:** arrives in slice 9.
