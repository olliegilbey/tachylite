// Offline: every page from the cache, and a sync that fetches only the pages that changed.
// A listed page that is also a listed file is an html page from the vault.
const CACHE = "tachylite";
const MANIFEST = "/.tachylite/manifest.json";
const SHELL = [
  "/.tachylite/live.js",
  "/.tachylite/search.js",
  "/.tachylite/style.css",
  "/.tachylite/app.webmanifest",
  "/.tachylite/icon.svg",
  "/.tachylite/icon-maskable.svg",
];
const PASS = new Set(["/.tachylite/events", "/.tachylite/sw.js", MANIFEST]);
const NOTES = "/.tachylite/notes.json";
const SEARCH = "/.tachylite/search/";
// Asked for with `no-cache`, these come from the network first: a cached copy can miss a new note.
const FRESH = new Set([NOTES, "/.tachylite/today"]);
const MERMAID = "/.tachylite/mermaid.js";
const KEEP = [...SHELL, MANIFEST, MERMAID];
const DIAGRAM = '<pre class="mermaid">';
// A fetch the worker queues for gives up after 30 s, so a hung one cannot hold the queue; the
// next sync retries it. Six pages share a slow link, so a working one finishes well inside it.
// An html page gets 120 s: it can be 1 MB, sent uncompressed. Its fetch skips the HTTP
// cache: a changed page can keep its size and modified time, the server's validator.
// An engine without `AbortSignal.timeout`, as iOS 15, fetches with no limit.
const get = (url, s = 30, cache) => fetch(url, { cache, signal: AbortSignal.timeout?.(s * 1e3) });
// A response rebuilt around a body, keeping its headers but the encoding and length of the old body.
const copy = (body, from) => {
  const headers = new Headers(from.headers);
  headers.delete("content-encoding");
  headers.delete("content-length");
  return new Response(body, { headers });
};

self.addEventListener("install", (e) => {
  e.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(SHELL))
      .then(() => self.skipWaiting()),
  );
});

// A worker from before Mermaid was synced may have left diagram pages with no script, so
// the first sync after an upgrade looks for one.
self.addEventListener("activate", (e) =>
  e.waitUntil(
    Promise.all([
      self.clients.claim(),
      later(async () => {
        const cache = await caches.open(CACHE);
        const have = await (await cache.match(MANIFEST))?.json();
        if (have && !(await cache.match(MERMAID))) await cache.put(MANIFEST, json({ ...have, [MERMAID]: "" }));
      }),
    ]),
  ),
);

// The version of each page a push or a sync stored, so a refresh begun before it keeps off it.
const pushed = new Map();

// Stale-while-revalidate: the cached copy at once, and a fresh one stored behind it. A page
// asks for a `FRESH` URL again with `no-cache` when it needs the network's copy, and gets it
// first, so a cached copy of one is not refreshed behind.
self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== location.origin || PASS.has(url.pathname)) return;
  e.respondWith(
    (async () => {
      const cache = await caches.open(CACHE);
      const hit = await cache.match(url.pathname);
      const before = pushed.get(url.pathname);
      const fresh = () => fetch(e.request).then((r) => {
        // The body is read before the check, so a push during a slow download still wins.
        const keep = (b) => pushed.get(url.pathname) === before && cache.put(url.pathname, copy(b, r));
        if (r.status === 200) e.waitUntil(r.clone().blob().then(keep).catch(() => {}));
        // The server binds only after its first build, so a 404 means the file is gone. The
        // manifest can still list it, so the next sync makes a full pass.
        if (r.status === 404) {
          done = null;
          e.waitUntil(cache.delete(url.pathname));
        }
        return r;
      });
      if (hit && !FRESH.has(url.pathname)) {
        e.waitUntil(fresh().catch(() => {}));
        return hit;
      }
      if (hit && e.request.cache !== "no-cache") return hit;
      return fresh().then(
        (r) => (r.ok || !hit ? r : hit),
        () => hit ?? new Response("Offline, and this page is not cached yet.", { status: 503 }),
      );
    })(),
  );
});

// One job runs at a time: a sync, or a pushed page to store. A sync asked for while one
// runs runs again after it, since the running one read an older manifest.
let queue = Promise.resolve();
let queued = false;
const later = (job) => (queue = queue.then(job).catch(() => {}));
self.addEventListener("message", (e) => {
  if (e.data === "sync" && !queued) {
    queued = true;
    later(() => {
      queued = false;
      return sync();
    });
  } else if (e.data?.html) later(() => store(e.data));
  e.waitUntil(queue);
});

const json = (value) => new Response(JSON.stringify(value), { headers: { "content-type": "application/json" } });

// A page pushed to an open browser tab, stored with the headers of the copy it replaces, so its
// policy stays. The page has the HTML already, so the worker fetches nothing.
async function store({ url, version, html }) {
  const cache = await caches.open(CACHE);
  const old = await cache.match(url);
  if (!old) return;
  pushed.set(url, version);
  done = null;
  await cache.put(url, copy(html, old));
  const have = await (await cache.match(MANIFEST))?.json();
  if (have) await cache.put(MANIFEST, json({ ...have, [url]: version }));
}

// The manifest's ETag when the last sync left every listed page at its listed version, and
// no push or 404 has changed the cache since. A worker restart forgets it.
let done = null;

// The stored manifest maps each listed page to the version its cached copy carries, or ""
// for one not fetched yet. It is saved before the fetches and every 50 pages, so a worker
// stopped mid-sync resumes where it stopped. A listed page with no cached copy is fetched
// again. A cached key the manifest does not list, as page or file, is dropped. An html page
// records the listed version, and is never read for a version or a diagram: its content is its own.
async function sync() {
  const cache = await caches.open(CACHE);
  const response = await get(MANIFEST).catch(() => null);
  if (!response?.ok) return;
  const tag = response.headers.get("etag");
  // A cache the browser evicted lost its stored manifest too.
  if (tag && tag === done && (await cache.match(MANIFEST))) return;
  done = null;
  const { pages: want, files = [] } = await response.json();
  const held = (await (await cache.match(MANIFEST))?.json()) ?? {};
  const keys = (await cache.keys()).map((r) => new URL(r.url).pathname);
  const cached = new Set(keys);
  const keep = new Set([...KEEP, ...files]);
  // A pending Mermaid entry outlives a sync cut short, so the script is fetched in the end.
  const have = MERMAID in held ? { [MERMAID]: "" } : {};
  const stale = [];
  for (const [url, version] of Object.entries(want)) {
    have[url] = cached.has(url) ? (held[url] ?? "") : "";
    if (have[url] !== version) stale.push(url);
  }
  // The home page, the index pages and the switcher list first: they reach the rest. The
  // search shards last: a note is read before it is searched.
  const index = (url) => (url === "/" || url.startsWith("/.tachylite/")) - 2 * url.startsWith(SEARCH);
  stale.sort((a, b) => index(b) - index(a));
  await Promise.all(keys.filter((url) => !(url in want) && !keep.has(url)).map((url) => cache.delete(url)));
  const save = () => cache.put(MANIFEST, json(have));
  await save();
  let stored = 0;
  let drawn = false;
  const workers = Array.from({ length: 6 }, async () => {
    for (let url; (url = stale.shift()); ) {
      try {
        const r = keep.has(url) ? await get(url, 120, "reload") : await get(url);
        if (r.status === 404) await cache.delete(url);
        if (r.status !== 200) continue;
        // A page may have changed since the manifest was read: record what came. A file
        // with no version meta, such as notes.json, records the listed version.
        const html = !keep.has(url) && r.headers.get("content-type")?.startsWith("text/html");
        const text = html ? await r.clone().text() : "";
        const version = html
          ? (/<meta name="tachylite-version" content="([0-9a-f]+)">/.exec(text)?.[1] ?? "")
          : want[url];
        await cache.put(url, r);
        pushed.set(url, version);
        have[url] = version;
        if (!cached.has(MERMAID) && text.includes(DIAGRAM)) {
          have[MERMAID] = "";
          drawn = true;
        }
        if (++stored % 50 === 0) await save();
      } catch {
        // A dropped connection or a full disk: the next sync retries it.
      }
    }
  });
  await Promise.all(workers);
  await save();
  // An open page holds parsed copies of the other tabs, and reads them again.
  if (stored) for (const client of await self.clients.matchAll()) client.postMessage(0);
  // A diagram draws offline only with the script. It is large, so it comes after the pages,
  // with time for 760 KB on a slow link. A pending entry with no page fetched this sync to
  // show a diagram looks through the cached pages.
  const draws = async () => {
    for (const url in want)
      if (!keep.has(url) && (await (await cache.match(url))?.text())?.includes(DIAGRAM)) return true;
  };
  if (MERMAID in have) {
    if (!cached.has(MERMAID) && (drawn || (await draws()))) {
      const r = await get(MERMAID, 120);
      if (!r.ok) return;
      await cache.put(MERMAID, r);
    }
    delete have[MERMAID];
    await save();
  }
  done = Object.entries(want).every(([url, version]) => have[url] === version) ? tag : null;
}
