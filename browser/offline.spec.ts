import type { Page, Route } from "@playwright/test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, lost, run, setHidden, test } from "./fixture";
import { serve, stop, type Server } from "./server";

// Each test runs its own server, so it can take the network away by stopping it.
let base = "";
let dir = "";
let server: Server | null = null;

function note(name: string, body: string) {
  writeFileSync(join(dir, "vault", `${name}.md`), body);
}

/** The text the service worker's cache holds for a path, or null. */
function cached(page: Page, path: string): Promise<string | null> {
  return page.evaluate(async (p) => {
    const hit = await caches.match(p);
    return hit ? hit.text() : null;
  }, path);
}

const MANIFEST = "/.tachylite/manifest.json";
const gif = Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64");

/** Whether the service worker's cache holds a path. */
function has(page: Page, path: string): Promise<boolean> {
  return page.evaluate(async (p) => (await caches.match(p)) !== undefined, path);
}

/** The server's manifest. */
async function listed(page: Page): Promise<{ pages: Record<string, string>; files: string[] }> {
  return (await page.request.get(base + MANIFEST)).json();
}

/** The worker's record of what it holds, by URL. */
function stored(page: Page): Promise<Record<string, string>> {
  return page.evaluate(async (m) => (await (await caches.match(m))?.json()) ?? {}, MANIFEST);
}

/** Waits until a sync has stored every listed page at its listed version. */
async function synced(page: Page) {
  const { pages } = await listed(page);
  await expect.poll(() => stored(page), { timeout: 15_000 }).toEqual(pages);
}

/** Asks the worker for a sync, as a page load does. */
function sync(page: Page): Promise<void> {
  return page.evaluate(() => navigator.serviceWorker.controller!.postMessage("sync"));
}

/** Opens a page and waits until the service worker controls it. */
async function visit(page: Page, path: string) {
  await page.goto(base + path);
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
}

test.beforeEach(async ({ allowedErrors }, info) => {
  allowedErrors.push(lost);
  dir = join(run, `offline-${info.title.replace(/\W+/g, "-")}-${info.project.name}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "vault"), { recursive: true });
  note("a", "# A\n\naye text\n");
  note("b", "# B\n\nbee text\n");
  server = await serve(dir);
  base = server.base;
});

test.afterEach(async () => {
  if (server) await stop(server);
  server = null;
});

test("a note never opened opens with the server stopped", async ({ page }) => {
  await visit(page, "/a");
  await expect.poll(() => cached(page, "/b")).toContain("bee text");
  await stop(server!);
  await page.goto(base + "/b");
  await expect(page.locator("main")).toContainText("bee text");
  await page.goto(base + "/a");
  await expect(page.locator("main")).toContainText("aye text");
});

test("a synced file without a version meta is not fetched again", async ({ page }) => {
  await visit(page, "/a");
  const listed = (await (await page.request.get(base + "/.tachylite/manifest.json")).json()).pages;
  const json = "/.tachylite/notes.json";
  const stored = () =>
    page.evaluate(async (m) => (await (await caches.match(m))?.json())?.[ "/.tachylite/notes.json"], "/.tachylite/manifest.json");
  await expect.poll(stored).toBe(listed[json]);
});

test("an edit made while away arrives on reconnect", async ({ page }) => {
  await visit(page, "/a");
  await expect.poll(() => cached(page, "/b")).toContain("bee text");
  await stop(server!);
  note("b", "# B\n\nnew text\n");
  server = await serve(dir, server!.port);
  await expect.poll(() => cached(page, "/b"), { timeout: 15_000 }).toContain("new text");
  await stop(server);
  await page.goto(base + "/b");
  await expect(page.locator("main")).toContainText("new text");
});

test("a deleted page leaves the cache", async ({ page }) => {
  await visit(page, "/a");
  await expect.poll(() => cached(page, "/b")).toContain("bee text");
  await stop(server!);
  rmSync(join(dir, "vault", "b.md"));
  server = await serve(dir, server!.port);
  await expect.poll(() => cached(page, "/b"), { timeout: 15_000 }).toBeNull();
  expect(await cached(page, "/a")).toContain("aye text");
});

test("a page whose sync failed still leaves the cache once deleted", async ({ page, context }) => {
  await visit(page, "/a");
  await expect.poll(() => cached(page, "/b")).toContain("bee text");
  // The worker's own fetch of /b fails during the next sync.
  let refused = 0;
  await context.route(/\/b$/, (route) => {
    refused++;
    return route.abort();
  });
  await stop(server!);
  note("b", "# B\n\nnew text\n");
  server = await serve(dir, server!.port);
  await expect.poll(() => refused, { timeout: 15_000 }).toBeGreaterThan(0);
  await context.unroute(/\/b$/);
  await stop(server);
  rmSync(join(dir, "vault", "b.md"));
  server = await serve(dir, server!.port);
  await expect.poll(() => cached(page, "/b"), { timeout: 15_000 }).toBeNull();
});

test("a sync records the version it fetched, not the one listed", async ({ page, context }) => {
  // The page changes between the manifest read and the page fetch: here, a page whose
  // version is not the listed one.
  await context.route(/\/b$/, (route) =>
    route.fulfill({
      contentType: "text/html",
      body: '<!doctype html><meta name="tachylite-version" content="0000000000000000"><main>raced text</main>',
    }),
  );
  await visit(page, "/a");
  await expect.poll(() => cached(page, "/b")).toContain("raced text");
  await context.unroute(/\/b$/);
  await stop(server!);
  server = await serve(dir, server!.port);
  await expect.poll(() => cached(page, "/b"), { timeout: 15_000 }).toContain("bee text");
});

test("a deleted image leaves the cache once a page asks for it online", async ({ page, allowedErrors }) => {
  allowedErrors.push(/status of 404/);
  const gif = Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64");
  writeFileSync(join(dir, "vault", "dot.gif"), gif);
  note("pic", "# Pic\n\n![[dot.gif]]\n");
  await expect.poll(async () => (await page.request.get(base + "/pic")).status()).toBe(200);
  await visit(page, "/a");
  await page.goto(base + "/pic");
  await expect.poll(() => cached(page, "/dot.gif")).not.toBeNull();
  rmSync(join(dir, "vault", "dot.gif"));
  await expect.poll(async () => (await page.request.get(base + "/dot.gif")).status()).toBe(404);
  // The delete also re-renders /pic without the image, and a sync may store that page
  // first, so the page asks for the image itself.
  await page.evaluate(() => fetch("/dot.gif"));
  await expect.poll(() => cached(page, "/dot.gif")).toBeNull();
});

test("an image opened once loads offline", async ({ page }) => {
  // A 1x1 transparent gif.
  const gif = Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64");
  writeFileSync(join(dir, "vault", "dot.gif"), gif);
  note("pic", "# Pic\n\n![[dot.gif]]\n");
  await stop(server!);
  server = await serve(dir, server!.port);
  await visit(page, "/a");
  await page.goto(base + "/pic");
  await expect.poll(() => cached(page, "/dot.gif")).not.toBeNull();
  await stop(server);
  await page.goto(base + "/pic");
  const width = await page.locator("main img").evaluate(async (img: HTMLImageElement) => {
    await img.decode();
    return img.naturalWidth;
  });
  expect(width).toBe(1);
});

// The headless shell skips Chromium's install check (`Page.getInstallabilityErrors` is
// always empty), so this asserts what the check reads: a parsed manifest with a name, a
// start URL, a standalone display, two icons that load, and a worker in control. The protocol
// drops each icon's purpose, so `tests/serve.rs` checks the maskable entry.
test("the app manifest meets the install criteria", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "the manifest is read through Chromium's protocol");
  await visit(page, "/a");
  const cdp = await page.context().newCDPSession(page);
  const { url, errors, manifest } = (await cdp.send("Page.getAppManifest")) as any;
  expect(url).toBe(base + "/.tachylite/app.webmanifest");
  expect(errors).toEqual([]);
  expect(manifest.name).toBe("Tachylite");
  expect(manifest.startUrl).toBe(base + "/");
  expect(manifest.display).toBe("kStandalone");
  expect(manifest.icons).toHaveLength(2);
  for (const { url } of manifest.icons) {
    const width = await page.evaluate(async (src) => {
      const img = new Image();
      img.src = src;
      await img.decode();
      return img.naturalWidth;
    }, url);
    expect(width).toBeGreaterThan(0);
  }
});

test("a sync fetches only the pages that changed", async ({ page, context }) => {
  await visit(page, "/a");
  await synced(page);
  const fetched: string[] = [];
  await context.route(/\/[ab]$/, (route) => {
    fetched.push(new URL(route.request().url()).pathname);
    return route.continue();
  });
  await stop(server!);
  note("b", "# B\n\nnew text\n");
  server = await serve(dir, server!.port);
  await synced(page);
  expect(fetched).toEqual(["/b"]);
  expect(await has(page, "/.tachylite/mermaid.js"), "a vault with no diagram").toBe(false);
});

test("an interrupted sync resumes, home and index pages first", async ({ page, context }) => {
  const count = 150;
  for (let i = 0; i < count; i++) note(`n${String(i).padStart(3, "0")}`, `# Note ${i}\n\ntext ${i}\n`);
  await expect.poll(async () => Object.keys((await listed(page)).pages).length).toBeGreaterThan(count);
  const isNote = (path: string) => /^\/n\d{3}$/.test(path);
  // The worker's fetches, in the order it made them. After 80 notes, the rest hang.
  let fetched: string[] = [];
  let limit = 80;
  let hold = false;
  const held: Route[] = [];
  await context.route(/.*/, (route) => {
    if (!route.request().serviceWorker()) return route.continue();
    if (hold) return void held.push(route);
    fetched.push(new URL(route.request().url()).pathname);
    hold = fetched.filter(isNote).length === limit;
    return route.continue();
  });
  const cdp = await context.newCDPSession(page);
  await cdp.send("ServiceWorker.enable");
  await visit(page, "/");
  await expect.poll(() => hold).toBe(true);
  const recorded = () => stored(page).then((have) => Object.entries(have).filter(([u, v]) => isNote(u) && v).length);
  await expect.poll(recorded).toBeGreaterThanOrEqual(40);
  await cdp.send("ServiceWorker.stopAllWorkers");
  const kept = await recorded();

  const { pages } = await listed(page);
  const indexes = Object.keys(pages).filter((u) => u === "/" || u.startsWith("/.tachylite/"));
  const order = [...new Set(fetched.filter((u) => u in pages))];
  expect(order.slice(0, indexes.length).sort()).toEqual(indexes.sort());

  fetched = [];
  limit = Infinity;
  hold = false;
  for (const route of held) await route.abort().catch(() => {});
  await page.reload();
  await synced(page);
  expect(fetched.filter(isNote)).toHaveLength(count - kept);
});

test("an open browser tab stores pushed pages, and syncs once visible again", async ({ page }) => {
  await visit(page, "/a");
  await synced(page);
  note("a", "# A\n\npushed text\n");
  await expect(page.locator("main")).toContainText("pushed text");
  await expect.poll(() => cached(page, "/a")).toContain("pushed text");
  note("b", "# B\n\nlater text\n");
  await expect.poll(async () => (await page.request.get(base + "/b")).text()).toContain("later text");
  await setHidden(page, true);
  await setHidden(page, false);
  await expect.poll(() => cached(page, "/b")).toContain("later text");
  await stop(server!);
  const response = await page.reload();
  await expect(page.locator("main")).toContainText("pushed text");
  expect(response?.headers()["content-security-policy"], "a stored push keeps its policy").toContain("default-src 'self'");
  await page.goto(base + "/b");
  await expect(page.locator("main")).toContainText("later text");
});

test("a page that 404s once is fetched again", async ({ page, context, allowedErrors }) => {
  allowedErrors.push(/status of 404/);
  await visit(page, "/a");
  await synced(page);
  await context.route(/\/b$/, (route) => route.fulfill({ status: 404, body: "" }));
  await page.evaluate(() => fetch("/b"));
  await expect.poll(() => has(page, "/b")).toBe(false);
  await context.unroute(/\/b$/);
  await sync(page);
  await expect.poll(() => cached(page, "/b")).toContain("bee text");
});

test("the first sync caches Mermaid for a page with a diagram", async ({ page }) => {
  note("m", "# M\n\n```mermaid\ngraph TD\nA-->B\n```\n");
  await expect.poll(async () => (await page.request.get(base + "/m")).status()).toBe(200);
  await visit(page, "/a");
  await expect.poll(() => has(page, "/.tachylite/mermaid.js"), { timeout: 15_000 }).toBe(true);
  await stop(server!);
  await page.goto(base + "/m");
  await expect(page.locator("main pre.mermaid svg")).toBeVisible();
});

test("a sync asked for during a sync runs after it", async ({ page, context }) => {
  let release = () => {};
  const gate = new Promise<void>((r) => (release = r));
  let asked = false;
  await context.route(/\/b$/, async (route) => {
    asked = true;
    await gate;
    await route.continue();
  });
  await visit(page, "/a");
  await expect.poll(() => asked).toBe(true);
  note("c", "# C\n\nsea text\n");
  await expect.poll(async () => Object.keys((await listed(page)).pages)).toContain("/c");
  await sync(page);
  release();
  await expect.poll(() => cached(page, "/c")).toContain("sea text");
});

test("the switcher lists a note made after the last sync", async ({ page }) => {
  await visit(page, "/a");
  await synced(page);
  note("fresh", "# Fresh\n\nfresh text\n");
  await expect.poll(async () => Object.keys((await listed(page)).pages)).toContain("/fresh");
  await page.locator("header button.find").click();
  await page.locator("dialog.switcher input").fill("fresh");
  await expect(page.locator("dialog.switcher li").first()).toContainText("fresh");
});

test("an html page or asset excluded while away leaves the cache", async ({ page }) => {
  mkdirSync(join(dir, "vault", "private"));
  writeFileSync(join(dir, "vault", "private", "map.html"), "<p>map</p>");
  writeFileSync(join(dir, "vault", "private", "dot.gif"), gif);
  writeFileSync(join(dir, "vault", "dot.gif"), gif);
  const files = ["/private/map.html", "/private/dot.gif", "/dot.gif"];
  await expect.poll(async () => (await listed(page)).files).toEqual(files.toSorted());
  await visit(page, "/a");
  for (const path of files) {
    await page.evaluate((p) => fetch(p), path);
    await expect.poll(() => has(page, path)).toBe(true);
  }
  await stop(server!);
  writeFileSync(join(dir, "vault", "tachylite.toml"), 'exclude = ["private"]\n');
  server = await serve(dir, server!.port);
  await expect.poll(() => has(page, "/private/map.html"), { timeout: 15_000 }).toBe(false);
  expect(await has(page, "/private/dot.gif")).toBe(false);
  expect(await has(page, "/dot.gif")).toBe(true);
});

// A sandboxed frame has an opaque origin, so the worker never sees the embed's request and
// it stays network-only. The same page opened full screen is cached.
test("offline, a note with an html embed renders, and its link opens the page opened once", async ({ page }) => {
  writeFileSync(
    join(dir, "vault", "widget.html"),
    `<!doctype html><p id="out">waiting</p><script>document.getElementById("out").textContent = "ran";</script>`,
  );
  note("host", "# Host\n\n![[widget.html]]\n");
  await expect.poll(async () => (await page.request.get(base + "/host")).status()).toBe(200);
  await visit(page, "/a");
  await synced(page);
  await page.goto(base + "/widget.html");
  await expect(page.locator("#out")).toHaveText("ran");
  await stop(server!);
  await page.goto(base + "/host");
  await expect(page.locator("main h1")).toHaveText("Host");
  await page.locator("main a", { hasText: "Open widget.html full screen" }).click();
  await expect(page).toHaveURL(base + "/widget.html");
  await expect(page.locator("#out")).toHaveText("ran");
});

// One engine runs both projects, and the wait here is the worker's fetch timeout itself.
test("a hung page fetch gives way to a push, and the next sync fetches it", async ({ page, context }, info) => {
  test.skip(info.project.name === "phone", "the desktop project covers it");
  test.setTimeout(90_000);
  await visit(page, "/a");
  await synced(page);
  // The worker's fetch of /b never answers.
  let hung = false;
  await context.route(/\/b$/, () => void (hung = true));
  note("b", "# B\n\nnew text\n");
  await expect.poll(async () => (await page.request.get(base + "/b")).text()).toContain("new text");
  await sync(page);
  await expect.poll(() => hung).toBe(true);
  note("a", "# A\n\npushed text\n");
  await expect(page.locator("main")).toContainText("pushed text");
  await expect.poll(() => cached(page, "/a"), { timeout: 60_000 }).toContain("pushed text");
  await context.unroute(/\/b$/);
  await sync(page);
  await expect.poll(() => cached(page, "/b")).toContain("new text");
});

test("a refresh that lands after a push does not store over it", async ({ page, context }) => {
  await visit(page, "/a");
  await synced(page);
  const older = await (await page.request.get(base + "/a")).text();
  // The worker's refresh of /a waits until the push is stored, then brings the older copy.
  let held: Route | null = null;
  await context.route(/\/a$/, (route) => {
    if (!route.request().serviceWorker()) return route.continue();
    held = route;
  });
  await page.evaluate(() => fetch("/a"));
  await expect.poll(() => held).not.toBeNull();
  note("a", "# A\n\npushed text\n");
  await expect(page.locator("main")).toContainText("pushed text");
  await expect.poll(() => cached(page, "/a")).toContain("pushed text");
  const landed = context.waitForEvent("requestfinished", (r) => r === held!.request());
  await held!.fulfill({ status: 200, contentType: "text/html", body: older });
  await landed;
  // A round trip through the worker's cache, queued behind any store the refresh made.
  await page.evaluate(() => fetch("/b"));
  expect(await cached(page, "/a")).toContain("pushed text");
});

test("an upgraded worker caches Mermaid for a diagram page synced before", async ({ page }) => {
  note("m", "# M\n\n```mermaid\ngraph TD\nA-->B\n```\n");
  await expect.poll(async () => (await page.request.get(base + "/m")).status()).toBe(200);
  await visit(page, "/a");
  await synced(page);
  // What an older worker left: every page, and no script.
  const script = "/.tachylite/mermaid.js";
  await page.evaluate((m) => caches.open("tachylite").then((c) => c.delete(m)), script);
  // A new worker arrives with a deploy, over the cache the old one left.
  await page.evaluate(() => navigator.serviceWorker.getRegistration().then((r) => r!.unregister()));
  await page.goto(base + "/a");
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  expect(await has(page, "/m"), "the old cache stays").toBe(true);
  await sync(page);
  await expect.poll(() => has(page, script), { timeout: 15_000 }).toBe(true);
  await stop(server!);
  await page.goto(base + "/m");
  await expect(page.locator("main pre.mermaid svg")).toBeVisible();
});

// A sync downloads an html page as it does a note page, and the worker's copy keeps the
// sandbox policy. So a page never opened runs offline, in an opaque origin.
test("an html page never opened opens offline after a sync, in an opaque origin", async ({ page }) => {
  writeFileSync(
    join(dir, "vault", "widget.html"),
    `<!doctype html><p id="out">waiting</p><script>
      const tried = (read) => { try { return String(read()); } catch (e) { return e.name; } };
      Promise.resolve()
        .then(() => caches.keys())
        .then((keys) => "read " + keys, (e) => e.name)
        .then((cache) => {
          const found = [origin, tried(() => localStorage.getItem("secret")), tried(() => document.cookie), cache];
          document.getElementById("out").textContent = found.join(" | ");
        });
    </script>`,
  );
  await expect.poll(async () => Object.keys((await listed(page)).pages)).toContain("/widget.html");
  await visit(page, "/a");
  await page.evaluate(() => localStorage.setItem("secret", "main origin"));
  await synced(page);
  await stop(server!);
  const response = await page.goto(base + "/widget.html");
  expect(response?.fromServiceWorker()).toBe(true);
  expect(response?.headers()["content-security-policy"]).toBe("sandbox allow-scripts");
  await expect(page.locator("#out")).toHaveText("null | SecurityError | SecurityError | SecurityError");
});

test("a sync downloads an html page again only when it changes, and never one past the size limit", async ({ page, context }) => {
  // The page carries a version meta and a diagram block of its own. The worker reads an
  // html page for neither: its version is the listed one, and it draws with no script of ours.
  const chart = (text: string) =>
    `<!doctype html><meta name="tachylite-version" content="0000000000000000"><pre class="mermaid">graph TD</pre><p>${text}</p>`;
  writeFileSync(join(dir, "vault", "chart.html"), chart("first chart"));
  writeFileSync(join(dir, "vault", "huge.html"), "x".repeat(1024 * 1024 + 1));
  await expect.poll(async () => (await listed(page)).files).toEqual(["/chart.html", "/huge.html"]);
  expect(Object.keys((await listed(page)).pages)).not.toContain("/huge.html");
  await visit(page, "/a");
  await synced(page);
  expect(await cached(page, "/chart.html")).toContain("first chart");
  expect(await has(page, "/huge.html"), "a large html page loads on demand").toBe(false);
  expect(await has(page, "/.tachylite/mermaid.js"), "an html page draws no diagram of ours").toBe(false);
  await page.evaluate(() => fetch("/huge.html"));
  await expect.poll(() => has(page, "/huge.html")).toBe(true);

  const fetched: string[] = [];
  await context.route(/\/(b|chart\.html|huge\.html)$/, (route) => {
    if (route.request().serviceWorker()) fetched.push(new URL(route.request().url()).pathname);
    return route.continue();
  });
  await stop(server!);
  note("b", "# B\n\nnew text\n");
  server = await serve(dir, server!.port);
  await synced(page);
  expect(fetched).toEqual(["/b"]);
  expect(await has(page, "/huge.html"), "a listed file stays cached").toBe(true);

  await stop(server);
  writeFileSync(join(dir, "vault", "chart.html"), chart("second chart"));
  server = await serve(dir, server!.port);
  await expect.poll(() => cached(page, "/chart.html"), { timeout: 15_000 }).toContain("second chart");
  await synced(page);
  expect(fetched).toEqual(["/b", "/chart.html"]);
});

// One engine runs both projects, and the wait here is the worker's fetch timeout itself.
test("an html page has longer to download than a note page", async ({ page, context }, info) => {
  test.skip(info.project.name === "phone", "the desktop project covers it");
  test.setTimeout(90_000);
  writeFileSync(join(dir, "vault", "slow.html"), "<p>first</p>");
  await expect.poll(async () => Object.keys((await listed(page)).pages)).toContain("/slow.html");
  await visit(page, "/a");
  await synced(page);
  // In the next sync, the note page never answers, and the html page waits to.
  let held: Route | null = null;
  await context.route(/\/slow\.html$/, (route) => {
    if (!route.request().serviceWorker()) return route.continue();
    held = route;
  });
  await context.route(/\/b$/, (route) => (route.request().serviceWorker() ? undefined : route.continue()));
  const gaveUp = context.waitForEvent("requestfailed", {
    predicate: (r) => !!r.serviceWorker() && new URL(r.url()).pathname === "/b",
    timeout: 60_000,
  });
  const before = (await listed(page)).pages;
  note("b", "# B\n\nnew text\n");
  writeFileSync(join(dir, "vault", "slow.html"), "<p>second</p>");
  await expect
    .poll(async () => {
      const { pages } = await listed(page);
      return pages["/b"] !== before["/b"] && pages["/slow.html"] !== before["/slow.html"];
    })
    .toBe(true);
  await sync(page);
  await expect.poll(() => held).not.toBeNull();
  // The worker gives up on the note page after its 30 s. The html page is still wanted.
  await gaveUp;
  await held!.continue();
  await expect.poll(() => cached(page, "/slow.html")).toContain("second");
});
