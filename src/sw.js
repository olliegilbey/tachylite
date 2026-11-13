// Offline: every page from the cache, and a sync that fetches only the pages that changed.
const CACHE = "tachylite";
const MANIFEST = "/.tachylite/manifest.json";
const SHELL = [
  "/.tachylite/live.js",
  "/.tachylite/style.css",
  "/.tachylite/app.webmanifest",
  "/.tachylite/icon.svg",
  "/.tachylite/icon-maskable.svg",
];
const PASS = new Set(["/.tachylite/events", "/.tachylite/sw.js", MANIFEST]);
const NOTES = "/.tachylite/notes.json";
const MERMAID = "/.tachylite/mermaid.js";
const KEEP = [...SHELL, MANIFEST, MERMAID];

self.addEventListener("install", (e) => {
  e.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

// Stale-while-revalidate: the cached copy at once, and a fresh one stored behind it. The
// switcher asks for its list with `no-cache` while connected, and gets the fresh one first.
self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== location.origin || PASS.has(url.pathname)) return;
  e.respondWith(
    (async () => {
      const cache = await caches.open(CACHE);
      const hit = await cache.match(url.pathname);
      const fresh = fetch(e.request).then((r) => {
        if (r.status === 200) e.waitUntil(cache.put(url.pathname, r.clone()).catch(() => {}));
        // The server binds only after its first build, so a 404 means the file is gone.
        if (r.status === 404) e.waitUntil(cache.delete(url.pathname));
        return r;
      });
      if (hit && !(url.pathname === NOTES && e.request.cache === "no-cache")) {
        e.waitUntil(fresh.catch(() => {}));
        return hit;
      }
      return fresh.then(
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

// A page pushed to an open tab, stored with the headers of the copy it replaces, so its
// policy stays. The page has the HTML already, so the worker fetches nothing.
async function store({ url, version, html }) {
  const cache = await caches.open(CACHE);
  const old = await cache.match(url);
  if (!old) return;
  const headers = new Headers(old.headers);
  headers.delete("content-encoding");
  headers.delete("content-length");
  await cache.put(url, new Response(html, { headers }));
  const have = await (await cache.match(MANIFEST))?.json();
  if (have) await cache.put(MANIFEST, json({ ...have, [url]: version }));
}

// The stored manifest maps each listed page to the version its cached copy carries, or ""
// for one not fetched yet. It is saved before the fetches and every 50 pages, so a worker
// stopped mid-sync resumes where it stopped. A listed page with no cached copy is fetched
// again. A cached key the manifest does not list, as page or file, is dropped.
async function sync() {
  const cache = await caches.open(CACHE);
  const response = await fetch(MANIFEST).catch(() => null);
  if (!response?.ok) return;
  const { pages: want, files = [] } = await response.json();
  const held = (await (await cache.match(MANIFEST))?.json()) ?? {};
  const keys = (await cache.keys()).map((r) => new URL(r.url).pathname);
  const cached = new Set(keys);
  const keep = new Set([...KEEP, ...files]);
  const have = {};
  const stale = [];
  for (const [url, version] of Object.entries(want)) {
    have[url] = cached.has(url) ? (held[url] ?? "") : "";
    if (have[url] !== version) stale.push(url);
  }
  // The home page, the index pages and the switcher list first: they reach the rest.
  const index = (url) => url === "/" || url.startsWith("/.tachylite/");
  stale.sort((a, b) => index(b) - index(a));
  await Promise.all(keys.filter((url) => !(url in want) && !keep.has(url)).map((url) => cache.delete(url)));
  const save = () => cache.put(MANIFEST, json(have));
  await save();
  let stored = 0;
  let diagram = false;
  const workers = Array.from({ length: 6 }, async () => {
    for (let url; (url = stale.shift()); ) {
      try {
        const r = await fetch(url);
        if (r.status === 404) await cache.delete(url);
        if (r.status !== 200) continue;
        // A page may have changed since the manifest was read: record what came. A file
        // with no version meta, such as notes.json, records the listed version.
        const html = r.headers.get("content-type")?.startsWith("text/html");
        const text = html ? await r.clone().text() : "";
        const version = html
          ? (/<meta name="tachylite-version" content="([0-9a-f]+)">/.exec(text)?.[1] ?? "")
          : want[url];
        await cache.put(url, r);
        have[url] = version;
        diagram ||= text.includes('<pre class="mermaid">');
        if (++stored % 50 === 0) await save();
      } catch {
        // A dropped connection or a full disk: the next sync retries it.
      }
    }
  });
  await Promise.all(workers);
  await save();
  // A diagram draws offline only with the script. It is large, so it comes after the pages.
  if (diagram && !cached.has(MERMAID)) await cache.add(MERMAID).catch(() => {});
}
