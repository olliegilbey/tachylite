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
  await expect(page).toHaveTitle("Tachylite");
  await expect(page.locator('header a[aria-current="page"]')).toHaveText("Home");
  await expect(page.locator("main")).toContainText("Fern care");
});

test("the tabs and a folder index work, and a tab page marks its own tab", async ({ page }) => {
  const current = page.locator('header a[aria-current="page"]');
  await page.goto(base + "/garden/Rose");
  await expect(page.locator("header > a")).toHaveText(["Home", "Recent", "Tasks"]);
  await expect(current).toHaveCount(0);
  await page.locator("nav.crumbs a", { hasText: "garden" }).click();
  await expect(page).toHaveURL(base + "/.tachylite/folders/garden");
  await expect(page.locator("main ul.notes li")).toHaveText(["Fern care", "Rose"]);
  await expect(current).toHaveCount(0);
  await page.locator("header > a", { hasText: "Recent" }).click();
  await expect(page).toHaveURL(base + "/.tachylite/recent");
  await expect(page.locator("main ul.recent li")).toHaveCount(3);
  await expect(current).toHaveText("Recent");
  await page.locator("header > a", { hasText: "Tasks" }).click();
  await expect(page).toHaveURL(base + "/.tachylite/tasks");
  await expect(current).toHaveText("Tasks");
  await page.locator("header > a", { hasText: "Home" }).click();
  await expect(page).toHaveURL(base + "/");
  await expect(page.locator("main ul.folders")).toContainText("garden");
  await expect(current).toHaveText("Home");
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

test("a long property key or value does not widen the page", async ({ page }) => {
  note("hash", `---\nchecksum: ${"0123456789abcdef".repeat(4)}\n${"key".repeat(24)}: x\n---\n# Hash\n`);
  await expect.poll(async () => (await page.request.get(base + "/hash")).status()).toBe(200);
  await page.goto(base + "/hash");
  const [scroll, client] = await page.evaluate(() => [
    document.documentElement.scrollWidth,
    document.documentElement.clientWidth,
  ]);
  expect(scroll).toBeLessThanOrEqual(client);
});

test("a long word in a task keeps the board's other column on screen", async ({ page }) => {
  note("unbroken", `# Unbroken\n\n- [ ] ${"word".repeat(80)}\n- [/] short\n`);
  await expect
    .poll(async () => (await (await page.request.get(base + "/.tachylite/tasks")).text()).includes("wordword"))
    .toBe(true);
  await page.goto(base + "/.tachylite/tasks");
  // A wide screen sets the doing task beside the todo task: the long word wraps in its own column.
  const doing = (await page.locator('main li[data-task="doing"]').boundingBox())!;
  const width = await page.evaluate(() => document.documentElement.clientWidth);
  expect(doing.x + doing.width).toBeLessThanOrEqual(width);
});

test("a tap beside a title in an index list opens the note", async ({ page }) => {
  await page.goto(base + "/.tachylite/folders/garden");
  const row = page.locator("main ul.notes li").first();
  const box = (await row.boundingBox())!;
  await page.mouse.click(box.x + box.width - 4, box.y + 4);
  await expect(page).toHaveURL(base + "/garden/Fern%20care");
});

test("a folded callout opens on a tap and from the keyboard", async ({ page }) => {
  note("fold", "# Fold\n\n> [!tip]- Folded\n> hidden body\n\n> [!note]+ Open\n> shown body\n");
  await expect.poll(async () => (await page.request.get(base + "/fold")).status()).toBe(200);
  await page.goto(base + "/fold");
  const body = page.locator(".callout-content").first();
  await expect(body).toBeHidden();
  await expect(page.locator(".callout-content").nth(1)).toBeVisible();
  await page.locator(".callout-title").first().click();
  await expect(body).toBeVisible();
  await page.locator(".callout-title").first().click();
  await expect(body).toBeHidden();
  await page.locator(".callout-title").first().focus();
  await page.keyboard.press("Enter");
  await expect(body).toBeVisible();
});

test("Escape closes the switcher with a query typed", async ({ page, isMobile }) => {
  test.skip(isMobile, "a phone has no Escape key");
  await page.goto(base + "/a");
  await find(page, "rose");
  await expect(page.locator("dialog.switcher li")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(page.locator("dialog.switcher")).toBeHidden();
});

test("the arrow keys move through the switcher's results", async ({ page, isMobile }) => {
  test.skip(isMobile, "a phone has no arrow keys");
  await page.goto(base + "/a");
  // Two titles hold an `r`, and no other note's text does.
  await find(page, "r");
  const input = page.locator("dialog.switcher input");
  await expect(input).toHaveAttribute("enterkeyhint", "go");
  await expect(page.locator("dialog.switcher ul")).toHaveAttribute("aria-live", "polite");
  const links = page.locator("dialog.switcher li a");
  await expect(links).toHaveCount(2);
  await page.keyboard.press("ArrowDown");
  await expect(links.nth(0)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(links.nth(1)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(links.nth(1)).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await expect(input).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  const second = await links.nth(1).getAttribute("href");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(base + second);
});

/** The device's local date, `offset` days from now, as `YYYY-MM-DD`. */
function iso(offset: number): string {
  const d = new Date(Date.now() + offset * 864e5);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Adds daily notes around today and a config with a daily folder and five pins. */
async function daily(page: Page) {
  note(`days/${iso(-3)}`, "# older\n");
  note(`days/${iso(-1)}`, "# latest\n\nyesterday's plan\n");
  note(`days/${iso(2)}`, "# planned\n");
  writeFileSync(
    join(dir, "vault", "tachylite.toml"),
    'daily = { folder = "days" }\n' +
      'pins = ["garden", "a.md|Aye", "garden/Rose.md", "garden/Fern care.md|Ferns", "days|Journal"]\n',
  );
  // A note saved before its new folder's watch is held as mid-save, so days can come apart.
  for (const day of [iso(-3), iso(-1), iso(2)]) {
    await expect
      .poll(async () => (await page.request.get(base + "/.tachylite/today")).text())
      .toContain(day);
  }
}

test("a daily note links to the daily notes before and after it", async ({ page }) => {
  await daily(page);
  await page.goto(base + `/days/${iso(-1)}`);
  const days = page.locator("main nav.days a");
  await expect(days).toHaveText([iso(-3), iso(2)]);
  // The previous day sits at the left edge and the next at the right, on one line.
  const boxes = await days.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().toJSON()));
  expect(boxes[0].right).toBeLessThan(boxes[1].left);
  expect(Math.round(boxes[0].top)).toBe(Math.round(boxes[1].top));
  // Each is a touch target, as a tab is, and names its direction to a screen reader.
  for (const box of boxes) expect(box.height).toBeGreaterThanOrEqual(48);
  await expect(page.getByRole("link", { name: `Previous: ${iso(-3)}` })).toBeVisible();
  await expect(page.getByRole("link", { name: `Next: ${iso(2)}` })).toBeVisible();
  await days.nth(1).click();
  await expect(page).toHaveURL(base + `/days/${iso(2)}`);
  await expect(days).toHaveText([iso(-1)]);
  await days.click();
  await expect(page).toHaveURL(base + `/days/${iso(-1)}`);
  // A new daily note reaches the open page of the one before it.
  note(`days/${iso(0)}`, "# today\n");
  await expect(days).toHaveText([iso(-3), iso(0)]);
});

test("five pins keep the header on its lines and the page its width", async ({ page }) => {
  await daily(page);
  await page.goto(base + "/a");
  await expect(page.locator("header .pins a")).toHaveText(["garden", "Aye", "Rose", "Ferns", "Journal"]);
  const tops = await page.locator("header > a, header > button").evaluateAll((els) =>
    els.map((e) => { const r = e.getBoundingClientRect(); return Math.round(r.top + r.height / 2); }),
  );
  expect(new Set(tops).size, "the first line wrapped").toBe(1);
  // A tab is a touch target: 48 CSS px is Android's minimum.
  const tabs = await page.locator("header > a").evaluateAll((els) =>
    els.map((e) => { const r = e.getBoundingClientRect(); return Math.min(r.width, r.height); }),
  );
  expect(tabs.length).toBe(4);
  for (const side of tabs) expect(side).toBeGreaterThanOrEqual(48);
  const find = await page.locator("header button.find").boundingBox();
  expect(find!.height).toBeGreaterThanOrEqual(44);
  const pins = await page.locator("header .pins a").evaluateAll((els) =>
    els.map((e) => { const r = e.getBoundingClientRect(); return Math.round(r.top + r.height / 2); }),
  );
  expect(new Set(pins).size, "the pins wrapped").toBe(1);
  const [scroll, client] = await page.evaluate(() => [
    document.documentElement.scrollWidth,
    document.documentElement.clientWidth,
  ]);
  expect(scroll).toBeLessThanOrEqual(client);
  await page.locator("header .pins a", { hasText: "Aye" }).click();
  await expect(page).toHaveURL(base + "/a");
});
