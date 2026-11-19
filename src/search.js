// A search worker: it holds a share of the notes' text and scans it for a query, off the
// page's thread. Its name is "<mine>/<of>": it takes the shards whose number, modulo `of`,
// is `mine`.
const MANIFEST = "/.tachylite/manifest.json";
const SHARD = /^\/\.tachylite\/search\/([0-9a-f]+)\.txt$/;
const [mine, of] = self.name.split("/").map(Number);
// Each shard held, by URL: its version and its rows.
const shards = new Map();
// Every row held, newest first: a note's URL, its modified time and its text.
let rows = [];

// Reads the shards the service worker's sync has stored, at the versions its record gives.
// It never asks the network: the sync brings each shard, and the page says when to read again.
const load = async () => {
  const cache = await caches.open("tachylite");
  const have = (await (await cache.match(MANIFEST))?.json()) ?? {};
  let changed = false;
  for (const url of shards.keys()) if (!(url in have)) changed = shards.delete(url);
  const read = async ([url, version]) => {
    const n = SHARD.exec(url);
    if (!n || parseInt(n[1], 16) % of !== mine || !version || shards.get(url)?.[0] === version) return;
    const text = await (await cache.match(url))?.text();
    if (text === undefined) return;
    // A row is three fields. Anything else is not a shard's row, and a scan would throw on it.
    const notes = text.split("\n").map((row) => row.split("\t"));
    shards.set(url, [version, notes.filter((row) => row.length === 3)]);
    changed = true;
  };
  // All at once: one read after another took twice as long.
  await Promise.all(Object.entries(have).map(read));
  const newest = (a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1);
  if (changed) rows = [...shards.values()].flatMap((shard) => shard[1]).sort(newest);
};
// A browser that gives a worker no cache has no text to search.
let ready = load().catch(() => {});

// The newest query asked. A scan that is running answers its own first.
let job = null;
// Scans once the newest read is done.
const run = () => {
  const loaded = ready;
  loaded.then(() => (loaded === ready ? scan() : run()));
};
const scan = () => {
  const { id, words } = job;
  job = null;
  const tests = words.map((word) => new RegExp(word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "i"));
  // The longest word rules a note out soonest.
  const order = [...tests].sort((a, b) => b.source.length - a.source.length);
  const hits = [];
  for (const [url, modified, text] of rows) {
    if (!order.every((test) => test.test(text))) continue;
    // The snippet: the first word's match, with a little before it and a line's worth after.
    const { 0: match, index } = tests[0].exec(text);
    const from = Math.max(0, index - 30);
    const before = text.slice(from, index);
    const end = index + match.length;
    hits.push([url, +modified, from ? "…" + before.replace(/^\S*\s/, "") : before, match, text.slice(end, end + 90)]);
    if (hits.length === 20) break;
  }
  postMessage({ id, hits });
};
onmessage = (e) => {
  // 0: the sync stored something, so read the shards again before the next scan.
  if (!e.data) return (ready = ready.then(load).catch(() => {}));
  const idle = !job;
  job = e.data;
  // A timer, so the queries that queued behind a scan land first, and one scan answers the last.
  if (idle) setTimeout(run);
};
