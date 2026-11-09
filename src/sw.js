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
        if (r.status === 200) e.waitUntil(cache.put(url.pathname, r.clone()));
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
    if (held[url] === version) have[url] = version;
    else stale.push(url);
  }
  await Promise.all(Object.keys(held).filter((url) => !(url in want)).map((url) => cache.delete(url)));
  const workers = Array.from({ length: 6 }, async () => {
    for (let url; (url = stale.shift()); ) {
      const r = await fetch(url).catch(() => null);
      if (r?.status !== 200) continue;
      await cache.put(url, r);
      have[url] = want[url];
    }
  });
  await Promise.all(workers);
  // A page that failed stays out of `have`, so the next sync fetches it again.
  await cache.put(MANIFEST, new Response(JSON.stringify(have), { headers: { "content-type": "application/json" } }));
}
