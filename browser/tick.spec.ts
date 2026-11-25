import type { Locator, Page } from "@playwright/test";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, lost, run, setHidden, test } from "./fixture";
import { serve, stop, type Server } from "./server";

// Each test runs its own server on a vault that takes writes, so it can stop it.
let base = "";
let dir = "";
let server: Server | null = null;

const TASKS = "# N\n\n- [ ] water the fern\n- [/] repot the basil\n";
const file = () => join(dir, "vault", "n.md");
const saved = () => readFileSync(file(), "utf8");
const VERSION = 'meta[name="tachylite-version"]';

/** The list item of the task with `text`. */
const task = (page: Page, text: string) => page.locator("li[data-box]", { hasText: text });
/** Its box. */
const box = (item: Locator) => item.locator("> input");

/** Opens a page and waits until the service worker controls it and its stream is open. */
async function visit(page: Page, path: string) {
  await page.goto(base + path);
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  await page.waitForFunction(() => (window as any).liveOpen === true);
}

test.beforeEach(async ({ page, allowedErrors }, info) => {
  // Each test ends by stopping its server under an open stream.
  allowedErrors.push(lost);
  dir = join(run, `tick-${info.title.replace(/\W+/g, "-")}-${info.project.name}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "vault"), { recursive: true });
  writeFileSync(join(dir, "vault", "tachylite.toml"), "writes = true\n");
  writeFileSync(file(), TASKS);
  server = await serve(dir);
  base = server.base;
  // Marks the page live once its event stream opens, without a hook in the shipped script.
  await page.addInitScript(() => {
    const Native = window.EventSource;
    window.EventSource = class extends Native {
      constructor(url: string | URL, init?: EventSourceInit) {
        super(url, init);
        const w = window as any;
        this.addEventListener("open", () => (w.liveOpen = true));
        this.addEventListener("error", () => (w.liveOpen = false));
      }
    };
  });
});

test.afterEach(async () => {
  if (server) await stop(server);
  server = null;
});

test("a tap ticks a task on its note page, and the push keeps it ticked", async ({ page }) => {
  await visit(page, "/n");
  const water = task(page, "water the fern");
  const version = await page.locator(VERSION).getAttribute("content");
  await box(water).click();
  await expect.poll(saved).toBe(TASKS.replace("[ ] water", "[x] water"));
  // The push brings the page the server rendered from the ticked note.
  await expect(page.locator(VERSION)).not.toHaveAttribute("content", version!);
  await expect(water).toHaveAttribute("data-task", "done");
  await expect(water).not.toHaveAttribute("data-pending");
  await expect(box(water)).toBeChecked();
  expect(await page.evaluate(() => localStorage.tachyliteTicks)).toBe("[]");

  // A doing task ticks done, and a second tap unticks it to todo.
  const basil = task(page, "repot the basil");
  await box(basil).click();
  await expect.poll(saved).toBe("# N\n\n- [x] water the fern\n- [x] repot the basil\n");
  await expect(box(basil)).toBeChecked();
  await box(basil).click();
  await expect.poll(saved).toBe("# N\n\n- [x] water the fern\n- [ ] repot the basil\n");
  await expect(basil).toHaveAttribute("data-task", "todo");
  await expect(basil).not.toHaveAttribute("data-pending");
});

test("a tap beside a box ticks it, and a tap on the task's text does not", async ({ page }) => {
  await visit(page, "/n");
  const water = task(page, "water the fern");
  const at = (await box(water).boundingBox())!;
  const text = (await water.boundingBox())!;
  await page.mouse.click(text.x + text.width / 2, at.y + at.height / 2);
  await page.mouse.click(at.x + at.width + 8, at.y + at.height / 2);
  await expect(box(water)).not.toBeChecked();
  // Left of the box, and above it by less than half a line.
  await page.mouse.click(at.x - 10, at.y - 4);
  await expect(box(water)).toBeChecked();
  await expect.poll(saved).toBe(TASKS.replace("[ ] water", "[x] water"));
  // The next line's box is its own.
  await expect(box(task(page, "repot the basil"))).not.toBeChecked();
});

test("a tap on the board ticks the task, and the board drops its line", async ({ page }) => {
  await visit(page, "/.tachylite/tasks");
  const water = task(page, "water the fern");
  await box(water).click();
  await expect.poll(saved).toBe(TASKS.replace("[ ] water", "[x] water"));
  await expect(water).toHaveCount(0);
  await expect(task(page, "repot the basil")).toHaveCount(1);
});

test("a tick made offline waits, survives a reload, and lands on reconnect", async ({ page }) => {
  await visit(page, "/n");
  await expect.poll(() => page.evaluate(async () => (await caches.match("/n")) !== undefined)).toBe(true);
  const port = server!.port;
  await stop(server!);
  const water = task(page, "water the fern");
  await box(water).click();
  await expect(water).toHaveAttribute("data-task", "done");
  await expect(water).toHaveAttribute("data-pending", "");
  await expect(box(water)).toBeChecked();

  await page.reload();
  await expect(water).toHaveAttribute("data-pending", "");
  await expect(water).toHaveAttribute("data-task", "done");
  await expect(box(water)).toBeChecked();
  expect(saved()).toBe(TASKS);

  server = await serve(dir, port);
  // A page that comes back into view connects again at once.
  await setHidden(page, true);
  await setHidden(page, false);
  await expect.poll(saved).toBe(TASKS.replace("[ ] water", "[x] water"));
  await expect(water).not.toHaveAttribute("data-pending");
  await expect(water).toHaveAttribute("data-task", "done");
  await expect(box(water)).toBeChecked();
  expect(await page.evaluate(() => localStorage.tachyliteTicks)).toBe("[]");
});

test("a refused tick puts the box back", async ({ page, allowedErrors }) => {
  allowedErrors.push(/409 \(Conflict\)/);
  await visit(page, "/n");
  // A hidden page holds no stream, so it keeps the page it has while the note changes.
  await setHidden(page, true);
  const reworded = TASKS.replace("water the fern", "water the fern twice");
  writeFileSync(file(), reworded);
  await expect.poll(async () => (await page.request.get(base + "/n")).text()).toContain("twice");
  const water = task(page, "water the fern");
  const refused = page.waitForResponse((r) => r.url().includes("/.tachylite/task"));
  await box(water).click();
  expect((await refused).status()).toBe(409);
  await expect(water).toHaveAttribute("data-task", "todo");
  await expect(water).not.toHaveAttribute("data-pending");
  await expect(box(water)).not.toBeChecked();
  expect(await page.evaluate(() => localStorage.tachyliteTicks)).toBe("[]");
  expect(saved()).toBe(reworded);
});

test("a POST is never answered from the cache", async ({ page, allowedErrors }) => {
  allowedErrors.push(/405 \(Method Not Allowed\)/);
  await visit(page, "/n");
  await expect.poll(() => page.evaluate(async () => (await caches.match("/n")) !== undefined)).toBe(true);
  const post = (url: string) =>
    page.evaluate((u) => fetch(u, { method: "POST" }).then((r) => r.status, () => "no answer"), url);
  // The cache holds the page, and the server refuses a POST to it.
  expect(await post("/n")).toBe(405);
  await stop(server!);
  expect(await page.evaluate(() => fetch("/n").then((r) => r.status))).toBe(200);
  expect(await post("/n")).toBe("no answer");
  expect(await post("/.tachylite/task?note=n.md&at=3-00000000&to=done")).toBe("no answer");
});
