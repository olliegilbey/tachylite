// Offline: every page from the cache, and a sync that fetches only the pages that changed.
const CACHE = "tachylite";
const MANIFEST = "/.tachylite/manifest.json";
const SHELL = ["/.tachylite/live.js", "/.tachylite/app.webmanifest", "/.tachylite/icon.svg"];
const PASS = new Set(["/.tachylite/events", "/.tachylite/sw.js", MANIFEST]);

self.addEventListener("install", (e) => {
  e.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

// Stale-while-revalidate: the cached copy at once, and a fresh one stored behind it.
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
      if (hit) {
        e.waitUntil(fresh.catch(() => {}));
        return hit;
      }
      return fresh.catch(() => new Response("Offline, and this page is not cached yet.", { status: 503 }));
    })(),
  );
});

// The page asks for a sync on load and on every reconnect. One runs at a time.
let syncing = null;
self.addEventListener("message", (e) => {
  if (e.data !== "sync") return;
  syncing ??= sync().finally(() => (syncing = null));
  e.waitUntil(syncing);
});

// The stored manifest maps each listed URL to the version its cached copy carries, or ""
// for one not fetched yet. It is stored before the fetches, so a page whose fetch fails,
// or a worker stopped mid-sync, stays listed: retried next time, and dropped once gone.
async function sync() {
  const cache = await caches.open(CACHE);
  const response = await fetch(MANIFEST).catch(() => null);
  if (!response?.ok) return;
  const want = (await response.json()).pages;
  const stored = await cache.match(MANIFEST);
  const held = stored ? await stored.json() : {};
  const have = {};
  const stale = [];
  for (const [url, version] of Object.entries(want)) {
    have[url] = held[url] ?? "";
    if (have[url] !== version) stale.push(url);
  }
  await Promise.all(Object.keys(held).filter((url) => !(url in want)).map((url) => cache.delete(url)));
  const save = () =>
    cache.put(MANIFEST, new Response(JSON.stringify(have), { headers: { "content-type": "application/json" } }));
  await save();
  const workers = Array.from({ length: 6 }, async () => {
    for (let url; (url = stale.shift()); ) {
      try {
        const r = await fetch(url);
        if (r.status === 404) await cache.delete(url);
        if (r.status !== 200) continue;
        // The page may have changed since the manifest was read: record what came.
        const text = await r.clone().text();
        const version = /<meta name="tachylite-version" content="([0-9a-f]+)">/.exec(text)?.[1] ?? "";
        await cache.put(url, r);
        have[url] = version;
      } catch {
        // A dropped connection or a full disk: the next sync retries it.
      }
    }
  });
  await Promise.all(workers);
  await save();
}
