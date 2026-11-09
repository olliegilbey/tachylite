import { expect, test, type Page } from "@playwright/test";
import type { ChildProcess } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { serve, stop } from "./server";

// Each test runs its own server, so it can take the network away by stopping it.
const port = 7433;
const base = `http://127.0.0.1:${port}`;
let dir = "";
let server: ChildProcess | null = null;

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

/** Opens a page and waits until the service worker controls it. */
async function visit(page: Page, path: string) {
  await page.goto(base + path);
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
}

test.beforeEach(async ({}, info) => {
  dir = join(__dirname, ".tmp", `offline-${info.title.replace(/\W+/g, "-")}-${info.project.name}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "vault"), { recursive: true });
  note("a", "# A\n\naye text\n");
  note("b", "# B\n\nbee text\n");
  server = await serve(dir, port);
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

test("an edit made while away arrives on reconnect", async ({ page }) => {
  await visit(page, "/a");
  await expect.poll(() => cached(page, "/b")).toContain("bee text");
  await stop(server!);
  note("b", "# B\n\nnew text\n");
  server = await serve(dir, port);
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
  server = await serve(dir, port);
  await expect.poll(() => cached(page, "/b"), { timeout: 15_000 }).toBeNull();
  expect(await cached(page, "/a")).toContain("aye text");
});

test("an image opened once loads offline", async ({ page }) => {
  // A 1x1 transparent gif.
  const gif = Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64");
  writeFileSync(join(dir, "vault", "dot.gif"), gif);
  note("pic", "# Pic\n\n![[dot.gif]]\n");
  await stop(server!);
  server = await serve(dir, port);
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
// start URL, a standalone display and an icon that loads, and a worker in control.
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
  expect(manifest.icons).toHaveLength(1);
  const icon = await page.evaluate(async (src) => {
    const img = new Image();
    img.src = src;
    await img.decode();
    return img.naturalWidth;
  }, manifest.icons[0].url);
  expect(icon).toBeGreaterThan(0);
});
