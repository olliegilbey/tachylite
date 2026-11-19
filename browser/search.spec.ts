import type { Page } from "@playwright/test";
import { mkdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { expect, lost, run, test } from "./fixture";
import { serve, stop, type Server } from "./server";

// Each test runs its own server, so it can save a note or take the network away.
let base = "";
let dir = "";
let server: Server | null = null;

const MANIFEST = "/.tachylite/manifest.json";

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
