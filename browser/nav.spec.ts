import type { Page } from "@playwright/test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { expect, lost, run, test } from "./fixture";
import { serve, stop, type Server } from "./server";

// Each test runs its own server, so it can take the network away by stopping it.
let base = "";
let dir = "";
let server: Server | null = null;

function note(name: string, body: string) {
  const path = join(dir, "vault", `${name}.md`);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, body);
}

/** Opens a page, waits for the worker, and waits until a sync has cached `path`. */
async function synced(page: Page, open: string, path: string) {
  await page.goto(base + open);
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  await expect
    .poll(() => page.evaluate(async (p) => (await caches.match(p)) !== undefined, path))
    .toBe(true);
}

async function find(page: Page, query: string) {
  await page.locator("header button.find").click();
  await page.locator("dialog.switcher input").fill(query);
}

test.beforeEach(async ({ allowedErrors }, info) => {
  allowedErrors.push(lost);
  dir = join(run, `nav-${info.title.replace(/\W+/g, "-")}-${info.project.name}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "vault"), { recursive: true });
  note("a", "# A\n\naye text\n");
  note("garden/Fern care", "---\naliases: [Ferns]\n---\n# Fern care\n\nwater weekly\n");
  note("garden/Rose", "# Rose\n\nprune in spring\n");
  server = await serve(dir);
  base = server.base;
});

test.afterEach(async () => {
  if (server) await stop(server);
  server = null;
});

test("typing part of a title opens the note", async ({ page }) => {
  await page.goto(base + "/a");
  await find(page, "frnc");
  await expect(page.locator("dialog.switcher li").first()).toContainText("Fern care");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(base + "/garden/Fern%20care");
  await expect(page.locator("main")).toContainText("water weekly");
});

test("an alias finds its note, and a title match ranks first", async ({ page }) => {
  await page.goto(base + "/a");
  await find(page, "ferns");
  await expect(page.locator("dialog.switcher li").first()).toContainText("Fern care");
  await page.locator("dialog.switcher input").fill("r");
  await expect(page.locator("dialog.switcher li").first()).toContainText("Rose");
});

test("a shortcut opens the switcher and Escape closes it", async ({ page, isMobile }) => {
  test.skip(isMobile, "a phone has no keyboard shortcut");
  await page.goto(base + "/a");
  const dialog = page.locator("dialog.switcher");
  await page.keyboard.press("Control+k");
  await expect(dialog).toBeVisible();
  await expect(dialog.locator("input")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await page.keyboard.press("/");
  await expect(dialog).toBeVisible();
});

test("the switcher works with the server stopped", async ({ page }) => {
  await synced(page, "/a", "/.tachylite/notes.json");
  await expect
    .poll(() => page.evaluate(async () => (await caches.match("/garden/Rose")) !== undefined))
    .toBe(true);
  await stop(server!);
  await page.reload();
  await find(page, "rose");
  await page.locator("dialog.switcher li a").first().click();
  await expect(page.locator("main")).toContainText("prune in spring");
});

test("the home page opens offline", async ({ page }) => {
  await synced(page, "/a", "/");
  await stop(server!);
  await page.goto(base + "/");
  await expect(page.locator("header a.site")).toHaveText("Tachylite");
  await expect(page.locator("main")).toContainText("Fern care");
});

test("the header's links and a folder index work", async ({ page }) => {
  await page.goto(base + "/garden/Rose");
  await page.locator("nav.crumbs a", { hasText: "garden" }).click();
  await expect(page).toHaveURL(base + "/.tachylite/folders/garden");
  await expect(page.locator("main ul.notes li")).toHaveText(["Fern care", "Rose"]);
  await page.locator("header a", { hasText: "Recent" }).click();
  await expect(page).toHaveURL(base + "/.tachylite/recent");
  await expect(page.locator("main ul.recent li")).toHaveCount(3);
  await page.locator("header a.site").click();
  await expect(page).toHaveURL(base + "/");
  await expect(page.locator("main ul.folders")).toContainText("garden");
});

test("a wide table does not widen the page", async ({ page }) => {
  const row = Array.from({ length: 30 }, (_, i) => `column${i}`).join(" | ");
  note("wide", `# Wide\n\n| ${row} |\n|${" --- |".repeat(30)}\n| ${row} |\n`);
  await expect.poll(async () => (await page.request.get(base + "/wide")).status()).toBe(200);
  await page.goto(base + "/wide");
  const [scroll, client] = await page.evaluate(() => [
    document.documentElement.scrollWidth,
    document.documentElement.clientWidth,
  ]);
  expect(scroll).toBeLessThanOrEqual(client);
});

test("a folded callout opens on a tap", async ({ page }) => {
  note("fold", "# Fold\n\n> [!tip]- Folded\n> hidden body\n");
  await expect.poll(async () => (await page.request.get(base + "/fold")).status()).toBe(200);
  await page.goto(base + "/fold");
  const body = page.locator(".callout-content");
  await expect(body).toBeHidden();
  await page.locator(".callout-title").click();
  await expect(body).toBeVisible();
});
