import type { Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { expect, lost, run, test } from "./fixture";
import { serve, stop, type Server } from "./server";

// Each test runs its own server, so it can save a note or take the network away.
let base = "";
let dir = "";
let server: Server | null = null;

const MANIFEST = "/.tachylite/manifest.json";
const scale = Number(process.env.BUDGET_SCALE ?? "1");
// The search budget, in ms for one key at 4x CPU, on the budget vault.
const BUDGET = 50;

/** Writes a note. `age` is how many days ago it was last saved. */
function note(name: string, body: string, age = 0) {
  const path = join(dir, "vault", `${name}.md`);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, body);
  const at = new Date(Date.UTC(2026, 0, 20 - age));
  utimesSync(path, at, at);
}

/** Opens a page, and waits until the worker's sync has stored every listed page and shard. */
async function synced(page: Page, open: string) {
  await page.goto(base + open);
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  const { pages } = await (await page.request.get(base + MANIFEST)).json();
  // `waitForFunction` takes a promise for true, so the page answers and the test polls.
  const stored = () =>
    page.evaluate(
      async ([manifest, want]) => {
        const have = (await (await caches.match(manifest))?.json()) ?? {};
        return Object.keys(want).every((url) => have[url] === want[url]);
      },
      [MANIFEST, pages] as const,
    );
  await expect.poll(stored, { timeout: 120_000 }).toBe(true);
}

const rows = (page: Page) => page.locator("dialog.switcher li");
/** Waits until every search worker has answered the query in the box. */
const answered = (page: Page) => expect(page.locator("dialog.switcher ul")).toHaveAttribute("aria-busy", "false");

async function find(page: Page, query: string) {
  await page.locator("header button.find").click();
  await page.locator("dialog.switcher input").fill(query);
  await answered(page);
}

test.beforeEach(async ({ allowedErrors }, info) => {
  allowedErrors.push(lost);
  dir = join(run, `search-${info.title.replace(/\W+/g, "-").slice(0, 80)}-${info.project.name}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "vault"), { recursive: true });
  note("a", "# A\n\naye text\n", 9);
  note("garden/Fern care", "# Fern care\n\nA fern likes shade. Water weekly.\n", 5);
  note("garden/Rose", "# Rose\n\nPrune in spring, well away from the fern bed.\n", 3);
  note("garden/Field round", "# Field round\n\nwalk the hedge line\n", 1);
});

test.afterEach(async () => {
  if (server) await stop(server);
  server = null;
});

test("a word from a note's text finds the note, with the match marked in a snippet", async ({ page }) => {
  server = await serve(dir);
  base = server.base;
  await synced(page, "/a");
  await page.locator("header button.find").click();
  // Typed a key at a time: an answer to an earlier key never shows under a later query.
  await page.locator("dialog.switcher input").pressSequentially("SPRING pru");
  await answered(page);
  await expect(rows(page)).toHaveCount(1);
  const row = rows(page).first();
  await expect(row.locator("a")).toHaveAttribute("href", "/garden/Rose");
  await expect(row).toContainText("Rose");
  await expect(row.locator(".meta")).toHaveText("garden/Rose");
  await expect(row.locator(".snippet")).toHaveText("Rose Prune in spring, well away from the fern bed.");
  await expect(row.locator("mark")).toHaveText("spring");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(base + "/garden/Rose");
});

test("every word must be in the text, and a query no note holds shows no row", async ({ page }) => {
  server = await serve(dir);
  base = server.base;
  await synced(page, "/a");
  await find(page, "weekly shade");
  await expect(rows(page).locator("a")).toHaveText([/^Fern care/]);
  await page.locator("dialog.switcher input").fill("weekly spring");
  await answered(page);
  await expect(rows(page)).toHaveCount(0);
});

test("whole name matches come first, then text matches newest first, then loose name matches, each note once", async ({ page }) => {
  note("garden/Moss", "# Moss\n\nGrows under a fern.\n", 4);
  server = await serve(dir);
  base = server.base;
  await synced(page, "/a");
  await find(page, "fern");
  // `Fern care` holds the query in its title and its text. `Field round` holds only its letters, in order.
  await expect(rows(page).locator("a")).toHaveText([/^Fern care/, /^Rose/, /^Moss/, /^Field round/]);
  await expect(rows(page).locator(".snippet")).toHaveCount(2);
  await expect(rows(page).nth(2).locator(".snippet")).toHaveText("Moss Grows under a fern.");
});

test("past 20 matches, the 20 newest notes show, newest first", async ({ page }) => {
  const two = (n: number) => String(n).padStart(2, "0");
  // The newest note comes last in path order, so path order is not the answer.
  for (let n = 0; n < 24; n++) note(`logs/day ${two(n)}`, "harvest tally\n", 40 - n);
  server = await serve(dir);
  base = server.base;
  // Two cores give one search worker, so it alone picks which 20 to answer with.
  await page.addInitScript(() => Object.defineProperty(navigator, "hardwareConcurrency", { value: 2 }));
  await synced(page, "/a");
  await find(page, "harvest");
  await expect(rows(page).locator("a")).toHaveText(Array.from({ length: 20 }, (_, n) => new RegExp(`^day ${two(23 - n)}`)));
});

test("loose name matches wait for the text matches, so no row moves once drawn", async ({ browser }) => {
  server = await serve(dir);
  base = server.base;
  const context = await browser.newContext({ serviceWorkers: "block" });
  const page = await context.newPage();
  let release = () => {};
  const held = new Promise<void>((resolve) => (release = resolve));
  await page.route("**/.tachylite/search.js", async (route) => {
    await held;
    await route.continue();
  });
  await page.goto(base + "/a");
  await page.locator("header button.find").click();
  await page.locator("dialog.switcher input").fill("fern");
  await expect(rows(page).locator("a")).toHaveText([/^Fern care/]);
  await expect(page.locator("dialog.switcher ul")).toHaveAttribute("aria-busy", "true");
  release();
  await expect(rows(page).locator("a")).toHaveText([/^Fern care/, /^Field round/]);
  await answered(page);
  await context.close();
});

test("search works with the server stopped", async ({ page }) => {
  server = await serve(dir);
  base = server.base;
  await synced(page, "/a");
  await stop(server);
  await page.goto(base + "/garden/Fern%20care");
  await find(page, "hedge");
  await expect(rows(page).locator("a")).toHaveText([/^Field round/]);
  await expect(rows(page).locator("mark")).toHaveText("hedge");
});

test("a saved note is found the next time the switcher opens", async ({ page }) => {
  server = await serve(dir);
  base = server.base;
  await synced(page, "/a");
  await find(page, "quince");
  await expect(rows(page)).toHaveCount(0);
  await page.keyboard.press("Escape");
  note("garden/Rose", "# Rose\n\nNow beside the quince tree.\n");
  // The open asks for a sync, and the rows follow it with no new key.
  await page.locator("header button.find").click();
  await page.locator("dialog.switcher input").fill("quince");
  await expect(rows(page).locator("a")).toHaveText([/^Rose/]);
  await expect(rows(page).locator(".snippet")).toHaveText("Rose Now beside the quince tree.");
});

test("a page with no service worker still matches names", async ({ browser }) => {
  server = await serve(dir);
  base = server.base;
  const context = await browser.newContext({ serviceWorkers: "block" });
  const page = await context.newPage();
  await page.goto(base + "/a");
  await find(page, "frnc");
  await expect(rows(page).locator("a")).toHaveText([/^Fern care/]);
  await context.close();
});

// The clock runs in the page, from the key going down to the paint of the rows that answer it.
// Chromium's CPU throttle does not reach a worker. So the page runs at a quarter speed and has
// two cores, which gives it one search worker at full speed. That worker scans the whole vault
// in the time four workers take on cores four times slower: the pool a phone with five cores
// or more gets. The log gives the scan and the page's share apart, so a reader can redo the
// sum for another pool: the page's share, plus the scan times four, over the workers.
test("answers a key within budget on the budget vault", async ({ page }, info) => {
  test.skip(info.project.name !== "phone", "The budget is the phone's.");
  test.setTimeout(240_000);
  const at = join(dir, "budget");
  execFileSync(join(__dirname, "..", "target", "release", "examples", "budget_vault"), [join(at, "vault")], { stdio: "ignore" });
  server = await serve(at);
  base = server.base;
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "hardwareConcurrency", { value: 2 });
    const gaps: number[] = ((window as any).gaps = []);
    let down = 0;
    addEventListener("keydown", () => (down = performance.now()), true);
    // Element Timing reports a paint on an element that carried the attribute before it painted.
    new MutationObserver(() => {
      const list = document.querySelector("dialog.switcher ul");
      if (down && list?.getAttribute("aria-busy") === "false")
        for (const e of list.querySelectorAll("*")) e.setAttribute("elementtiming", "row");
    }).observe(document, { attributes: true, subtree: true, attributeFilter: ["aria-busy"] });
    new PerformanceObserver((list) => {
      const first = list.getEntries().find((e) => (e as any).renderTime > down);
      if (down && first) gaps.push((first as any).renderTime - down);
      if (first) down = 0;
    }).observe({ type: "element" });
  });
  await synced(page, "/");
  // `queue` is the chain of jobs in `src/sw.js`. Once it settles, the first sync is over.
  await page.context().serviceWorkers()[0].evaluate("queue");
  // Each query ends in rows, so a paint ends its clock. The last scans every note for two rows.
  const queries = ["kalo", "zanquinne", "dorten mifelbar", "zanquinne tuvomar"];
  // One worker's scan of every note at full speed, from the ask to the answer.
  const scans: number[] = await page.evaluate(async (queries) => {
    const worker = new Worker("/.tachylite/search.js", { name: "0/1" });
    let id = 0;
    const ask = (query: string) =>
      new Promise<number>((done) => {
        const from = performance.now();
        worker.onmessage = () => done(performance.now() - from);
        worker.postMessage({ id: ++id, words: query.split(" ") });
      });
    await ask("first");
    const scans = [];
    for (const query of queries) {
      const times = [];
      for (let i = 0; i < 9; i++) times.push(await ask(query));
      scans.push(times.sort((a, b) => a - b)[4]);
    }
    worker.terminate();
    return scans;
  }, queries);
  const input = page.locator("dialog.switcher input");
  // The first answer waits for the worker to read its text. The budget is for a key after it.
  await find(page, "first");
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 4 });
  const rounds = 5;
  for (let i = 0; i < rounds * queries.length; i++) {
    const query = queries[i % queries.length];
    await input.fill(query.slice(0, -1));
    await answered(page);
    await input.press(query.slice(-1));
    await page.waitForFunction((n) => (window as any).gaps.length === n, i + 1);
  }
  const gaps: number[] = await page.evaluate(() => (window as any).gaps);
  queries.forEach((query, q) => {
    const mine = gaps.filter((_, i) => i % queries.length === q).sort((a, b) => a - b);
    const median = mine[rounds >> 1];
    console.log(
      `search key, ${info.project.name}, 4x CPU throttle, one worker at full speed, "${query}": median ${median.toFixed(1)} ms of ${mine.map((g) => g.toFixed(0))}; the scan ${scans[q].toFixed(1)} ms, the page's share ${(median - scans[q]).toFixed(1)} ms`,
    );
    expect(median, query).toBeLessThanOrEqual(BUDGET * scale);
  });
});
