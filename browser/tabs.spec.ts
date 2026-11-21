import type { Page } from "@playwright/test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { expect, lost, run, setHidden, test } from "./fixture";
import { serve, stop, type Server } from "./server";

// Each test runs its own server on a vault with daily notes, so every vault has three tabs.
let base = "";
let dir = "";
let server: Server | null = null;

const TODAY = "/.tachylite/today";
const RECENT = "/.tachylite/recent";
const scale = Number(process.env.BUDGET_SCALE ?? "1");
// The tab switch budget, in ms at 4x CPU. The design doc says how it was set.
const BUDGET = 50;

function note(name: string, body: string) {
  const path = join(dir, "vault", `${name}.md`);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, body);
}

/** The device's local date, `offset` days from now, as `YYYY-MM-DD`. */
function iso(offset: number): string {
  const d = new Date(Date.now() + offset * 864e5);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** The newest daily note dated on or before today: the note the Today tab shows. */
const latest = `/days/${iso(-1)}`;
const wide = "```\n" + "a long line of code ".repeat(20) + "\n```\n";
const diagram = "```mermaid\ngraph TD\nA-->B\n```\n";

/** The text the service worker's cache holds for a path, or null. */
function cached(page: Page, path: string): Promise<string | null> {
  return page.evaluate(async (p) => {
    const hit = await caches.match(p);
    return hit ? hit.text() : null;
  }, path);
}

/** Waits until the worker controls the page and a sync has cached each path. */
async function synced(page: Page, ...paths: string[]) {
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  for (const path of paths) await expect.poll(() => cached(page, path), { timeout: 15_000 }).not.toBeNull();
}

/** The event streams the page has opened, as the page each follows and its state. 1 is open, 2 closed. */
function streams(page: Page): Promise<[string, number][]> {
  return page.evaluate(() =>
    ((window as any).streams as EventSource[]).map((s) => [new URL(s.url).searchParams.get("page")!, s.readyState]),
  );
}

/** Waits until the page's newest stream follows `path` and is open. */
async function live(page: Page, path: string) {
  await expect.poll(async () => (await streams(page)).at(-1)).toEqual([path, 1]);
}

/**
 * One finger down at `from`, moved to `to`, and lifted. It is real touch input, so the
 * browser's own pan applies, as on a phone.
 */
async function swipe(page: Page, from: [number, number], to: [number, number]) {
  const cdp = await page.context().newCDPSession(page);
  const steps = 8;
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: from[0], y: from[1] }] });
  for (let i = 1; i <= steps; i++) {
    const at = (k: 0 | 1) => from[k] + ((to[k] - from[k]) * i) / steps;
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: at(0), y: at(1) }] });
  }
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await cdp.detach();
}

/** A swipe across the middle of the screen: to the left opens the tab to the right. */
const left = (page: Page) => swipe(page, [320, 500], [90, 510]);
const right = (page: Page) => swipe(page, [90, 500], [320, 510]);

/** Waits out any switch a gesture began: a round trip through the worker, then a frame. */
async function settled(page: Page) {
  await page.evaluate(async () => {
    await (await fetch(location.pathname)).text();
    await new Promise(requestAnimationFrame);
  });
}

/** Waits until the page is idle: a frame has passed and no task waits. */
function rested(page: Page) {
  return page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => (window as any).scheduler.postTask(r, { priority: "background" }))),
  );
}

/** Asserts the tab on screen: its URL, its mark in the header, and a text in its `<main>`. */
async function shows(page: Page, tab: "Home" | "Today" | "Recent") {
  const [path, text] = { Home: ["/", "garden"], Today: [TODAY, "yesterday's plan"], Recent: [RECENT, "Fern care"] }[tab];
  await expect(page).toHaveURL(base + path);
  await expect(page.locator('header a[aria-current="page"]')).toHaveText(tab);
  await expect(page.locator("main")).toContainText(text);
  await expect(page.locator("main ul.folders")).toHaveCount(tab === "Home" ? 1 : 0);
}

test.beforeEach(async ({ page, allowedErrors }, info) => {
  allowedErrors.push(lost);
  dir = join(run, `tabs-${info.title.replace(/\W+/g, "-").slice(0, 80)}-${info.project.name}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(join(dir, "vault"), { recursive: true });
  note("a", "# A\n\naye text\n");
  note("garden/Fern care", "# Fern care\n\nwater weekly\n");
  note(`days/${iso(-3)}`, "# older\n");
  note(`days/${iso(-1)}`, `# latest\n\nyesterday's plan\n\n${wide}\n${diagram}`);
  note(`days/${iso(2)}`, "# planned\n");
  const pins = Array.from({ length: 12 }, (_, i) => `"a.md|Pin number ${i}"`).join(", ");
  writeFileSync(join(dir, "vault", "tachylite.toml"), `daily = { folder = "days" }\npins = [${pins}]\n`);
  server = await serve(dir);
  base = server.base;
  // Lists each event stream the page opens, without a hook in the shipped script.
  await page.addInitScript(() => {
    const Native = window.EventSource;
    window.EventSource = class extends Native {
      constructor(url: string | URL, init?: EventSourceInit) {
        super(url, init);
        ((window as any).streams ??= []).push(this);
      }
    };
  });
});

test.afterEach(async () => {
  if (server) await stop(server);
  server = null;
});

test.describe("the Today tab", () => {
  test("shows the newest daily note up to today in place, and keeps its mark", async ({ page }) => {
    await page.goto(base + "/a");
    await page.locator("header > a", { hasText: "Today" }).click();
    await shows(page, "Today");
    await expect(page.locator("main h1")).toHaveText("latest");
    await expect(page).toHaveTitle(iso(-1));
    await expect(page.locator("main [data-date]")).toHaveCount(0);
    // The tab opened from a note is an ordinary link, so Back returns to the note.
    await page.goBack();
    await expect(page).toHaveURL(base + "/a");
  });

  test("takes a push to its note, and the worker stores it under the note's URL", async ({ page }) => {
    await page.goto(base + TODAY);
    await shows(page, "Today");
    await synced(page, TODAY, latest);
    await live(page, latest);
    note(`days/${iso(-1)}`, "# latest\n\npushed plan\n");
    await expect(page.locator("main")).toContainText("pushed plan");
    // The pushed note page marks no tab, and the Today tab keeps its own.
    await expect(page.locator('header a[aria-current="page"]')).toHaveText("Today");
    await expect(page).toHaveURL(base + TODAY);
    await expect.poll(() => cached(page, latest)).toContain("pushed plan");
    const list = await cached(page, TODAY);
    expect(list).toContain(`data-date="${iso(-1)}"`);
    expect(list).not.toContain("pushed plan");
  });

  test("opens offline, from the cached list and the cached note", async ({ page }) => {
    await page.goto(base + "/a");
    await synced(page, TODAY, latest);
    await stop(server!);
    await page.goto(base + TODAY);
    await shows(page, "Today");
  });

  test("replaces its note when the network's list names a newer one", async ({ page }) => {
    await page.goto(base + "/a");
    await synced(page, TODAY, latest);
    // The worker's cached list predates this note.
    note(`days/${iso(0)}`, "# today\n\ntoday's plan\n");
    await expect.poll(async () => (await page.request.get(base + TODAY)).text()).toContain(iso(0));
    await page.goto(base + TODAY);
    await expect(page.locator("main")).toContainText("today's plan");
    await expect(page).toHaveURL(base + TODAY);
    await live(page, `/days/${iso(0)}`);
  });

  test("fetches its page once when it opens with a cached copy", async ({ page, context }) => {
    await page.goto(base + "/");
    await synced(page, TODAY, latest);
    await context.serviceWorkers()[0].evaluate("queue");
    // Counts the worker's own fetches of the page. A refresh starts before the cached copy is answered.
    const worker = context.serviceWorkers()[0];
    await worker.evaluate((today) => {
      const native = self.fetch;
      (self as any).fetched = 0;
      self.fetch = (request, init) => {
        if (String((request as Request).url ?? request).endsWith(today)) (self as any).fetched++;
        return native(request, init);
      };
    }, TODAY);
    // The page asks twice: the navigation, answered from the cache, then the network's list.
    let asked = 0;
    page.on("requestfinished", (r) => r.url().endsWith(TODAY) && asked++);
    await page.goto(base + TODAY);
    await shows(page, "Today");
    await expect.poll(() => asked).toBe(2);
    expect(await worker.evaluate(() => (self as any).fetched)).toBe(1);
  });

  test("moves to a new day's note when the browser tab is visible again", async ({ page }) => {
    await page.goto(base + TODAY);
    await shows(page, "Today");
    await setHidden(page, true);
    note(`days/${iso(0)}`, "# today\n\ntoday's plan\n");
    await expect.poll(async () => (await page.request.get(base + TODAY)).text()).toContain(iso(0));
    await setHidden(page, false);
    await expect(page.locator("main")).toContainText("today's plan");
    await expect(page).toHaveURL(base + TODAY);
    await live(page, `/days/${iso(0)}`);
  });

  test("keeps its list with no script", async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    try {
      const plain = await context.newPage();
      await plain.goto(base + TODAY);
      await expect(plain.locator("main h1")).toHaveText("Daily notes");
      await expect(plain.locator("main [data-date]")).toHaveText([iso(2), iso(-1), iso(-3)]);
      await expect(plain.locator("main [data-date]").first()).toBeVisible();
    } finally {
      await context.close();
    }
  });

  test("keeps its list when no daily note is dated up to today", async ({ page }) => {
    rmSync(join(dir, "vault", "days", `${iso(-1)}.md`));
    rmSync(join(dir, "vault", "days", `${iso(-3)}.md`));
    await expect.poll(async () => (await page.request.get(base + TODAY)).text()).not.toContain(iso(-1));
    await expect.poll(async () => (await page.request.get(base + TODAY)).text()).not.toContain(iso(-3));
    await page.goto(base + TODAY);
    await expect(page.locator("main [data-date]")).toHaveText([iso(2)]);
    await expect(page.locator("main [data-date]")).toBeVisible();
    await expect(page.locator('header a[aria-current="page"]')).toHaveText("Today");
    await live(page, TODAY);
    // Switched to in place, the list shows as well.
    await page.locator("header > a", { hasText: "Home" }).click();
    await shows(page, "Home");
    await page.locator("header > a", { hasText: "Today" }).click();
    await expect(page.locator("main [data-date]")).toBeVisible();
    await expect(page).toHaveURL(base + TODAY);
  });

  test("never shows its list while the script has yet to run", async ({ page, context }) => {
    // The script never arrives, so the page stays as the server sent it.
    await context.route(/\/\.tachylite\/live\.js$/, () => {});
    await page.goto(base + TODAY, { waitUntil: "commit" });
    await expect(page.locator("main [data-date]")).toHaveCount(3);
    await expect(page.locator("main h1")).toBeHidden();
    await expect(page.locator("main [data-date]").first()).toBeHidden();
    await expect(page.locator("header > a").first()).toBeVisible();
  });
});

test.describe("a swipe", () => {
  test.skip(({ hasTouch }) => !hasTouch, "a swipe takes a finger");

  test("opens the tab beside this one, each way, and stops at both ends", async ({ page }) => {
    await page.goto(base + "/");
    await shows(page, "Home");
    await left(page);
    await shows(page, "Today");
    await left(page);
    await shows(page, "Recent");
    await left(page);
    await settled(page);
    await shows(page, "Recent");
    await right(page);
    await shows(page, "Today");
    await right(page);
    await shows(page, "Home");
    await right(page);
    await settled(page);
    await shows(page, "Home");
  });

  test("that starts at a screen edge does nothing", async ({ page }) => {
    await page.goto(base + TODAY);
    await shows(page, "Today");
    const width = page.viewportSize()!.width;
    await swipe(page, [width - 12, 500], [width - 250, 505]);
    await settled(page);
    await shows(page, "Today");
    await swipe(page, [12, 500], [250, 505]);
    await settled(page);
    await shows(page, "Today");
  });

  test("that starts in the pins row scrolls the pins", async ({ page }) => {
    await page.goto(base + TODAY);
    await shows(page, "Today");
    const pins = page.locator("header .pins");
    const box = (await pins.boundingBox())!;
    await swipe(page, [320, box.y + box.height / 2], [90, box.y + box.height / 2]);
    await expect.poll(() => pins.evaluate((e) => e.scrollLeft)).toBeGreaterThan(0);
    await settled(page);
    await shows(page, "Today");
  });

  test("that starts in a wide code block scrolls the code", async ({ page }) => {
    await page.goto(base + TODAY);
    await shows(page, "Today");
    const code = page.locator("main pre").first();
    const box = (await code.boundingBox())!;
    await swipe(page, [320, box.y + box.height / 2], [90, box.y + box.height / 2]);
    await expect.poll(() => code.evaluate((e) => e.scrollLeft)).toBeGreaterThan(0);
    await settled(page);
    await shows(page, "Today");
  });

  test("up the page scrolls it, and a slanted or a short one does nothing", async ({ page }) => {
    note(`days/${iso(-1)}`, `# latest\n\nyesterday's plan\n\n${"a paragraph\n\n".repeat(80)}`);
    await expect.poll(async () => (await page.request.get(base + latest)).text()).toContain("a paragraph");
    await page.goto(base + TODAY);
    await shows(page, "Today");
    await swipe(page, [200, 600], [215, 200]);
    await expect.poll(() => page.evaluate(() => scrollY)).toBeGreaterThan(0);
    await settled(page);
    await shows(page, "Today");
    await page.evaluate(() => scrollTo(0, 0));
    await swipe(page, [320, 300], [200, 420]);
    await settled(page);
    await shows(page, "Today");
    await swipe(page, [220, 300], [175, 300]);
    await settled(page);
    await shows(page, "Today");
  });

  test("of a pinched-in page pans it", async ({ page }) => {
    await page.goto(base + TODAY);
    await shows(page, "Today");
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Input.synthesizePinchGesture", { x: 200, y: 400, scaleFactor: 2 });
    await expect.poll(() => page.evaluate(() => visualViewport!.scale)).toBeGreaterThan(1.5);
    const before = await page.evaluate(() => visualViewport!.offsetLeft);
    // The touch lands clear of both edges of the layout viewport, which the pinch moved.
    await swipe(page, [250, 500], [60, 510]);
    await expect.poll(() => page.evaluate(() => visualViewport!.offsetLeft)).toBeGreaterThan(before);
    await settled(page);
    await shows(page, "Today");
  });

  test("joined by a second finger does nothing", async ({ page }) => {
    await page.goto(base + "/");
    await shows(page, "Home");
    const cdp = await page.context().newCDPSession(page);
    const touch = (type: string, ...touchPoints: { x: number; y: number; id: number }[]) =>
      cdp.send("Input.dispatchTouchEvent", { type: type as "touchStart", touchPoints });
    await touch("touchStart", { x: 320, y: 500, id: 0 });
    await touch("touchStart", { x: 320, y: 500, id: 0 }, { x: 330, y: 300, id: 1 });
    await touch("touchMove", { x: 90, y: 500, id: 0 }, { x: 100, y: 300, id: 1 });
    await touch("touchEnd");
    await settled(page);
    await shows(page, "Home");
  });

  test("does nothing while the switcher is open", async ({ page }) => {
    await page.goto(base + TODAY);
    await shows(page, "Today");
    await page.locator("header button.find").click();
    await expect(page.locator("dialog.switcher")).toBeVisible();
    await left(page);
    await settled(page);
    await shows(page, "Today");
  });

  test("on a note page does nothing", async ({ page }) => {
    await page.goto(base + latest);
    await expect(page.locator("main")).toContainText("yesterday's plan");
    await left(page);
    await right(page);
    await settled(page);
    await expect(page).toHaveURL(base + latest);
  });

  // The clock runs in the page, from the finger's release to the paint of the new tab's content.
  test("switches tabs within budget", async ({ page }, info) => {
    note(`days/${iso(-1)}`, "# latest\n\nyesterday's plan\n");
    await expect.poll(async () => (await page.request.get(base + latest)).text()).not.toContain("mermaid");
    await page.addInitScript(() => {
      const gaps: number[] = ((window as any).gaps = []);
      let released = 0;
      addEventListener("touchend", () => (released = performance.now()), true);
      // Element Timing reports a paint on an element that carried the attribute before it painted.
      new MutationObserver(() => {
        for (const e of document.querySelectorAll("main:not([elementtiming]), main :not([elementtiming])"))
          e.setAttribute("elementtiming", "tab");
      }).observe(document, { childList: true, subtree: true });
      new PerformanceObserver((list) => {
        const first = list.getEntries().find((e) => (e as any).renderTime > released);
        if (released && first) gaps.push((first as any).renderTime - released);
        if (first) released = 0;
      }).observe({ type: "element" });
    });
    await page.goto(base + "/");
    await synced(page, "/", TODAY, RECENT, latest);
    await live(page, "/");
    // `queue` is the chain of jobs in `src/sw.js`. Once it settles, the first sync is over.
    await page.context().serviceWorkers()[0].evaluate("queue");
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Emulation.setCPUThrottlingRate", { rate: 4 });
    const round = [
      [left, "Today", latest],
      [left, "Recent", RECENT],
      [right, "Today", latest],
      [right, "Home", "/"],
    ] as const;
    for (let i = 0; i < 12; i++) {
      // A reader rests on a tab before the next swipe: its stream is open and the page is idle.
      await rested(page);
      const [gesture, tab, stream] = round[i % 4];
      await gesture(page);
      await page.waitForFunction((n) => (window as any).gaps.length === n, i + 1);
      await shows(page, tab);
      await live(page, stream);
    }
    const gaps: number[] = await page.evaluate(() => (window as any).gaps);
    const median = [...gaps].sort((a, b) => a - b)[gaps.length >> 1];
    console.log(`tab switch, ${info.project.name}, 4x CPU throttle: median ${median.toFixed(1)} ms of ${gaps.map((g) => g.toFixed(0))}`);
    expect(median).toBeLessThanOrEqual(BUDGET * scale);
  });
});

test("a mouse drag across a tab page does nothing", async ({ page }) => {
  await page.goto(base + TODAY);
  await shows(page, "Today");
  await page.mouse.move(320, 500);
  await page.mouse.down();
  await page.mouse.move(90, 505, { steps: 5 });
  await page.mouse.up();
  await settled(page);
  await shows(page, "Today");
});

test("a tab opened from another tab replaces the history entry, by tap or by swipe", async ({ page, hasTouch }) => {
  await page.goto(base + "/a");
  // From a note, a tab is an ordinary link, and adds an entry.
  const before = await page.evaluate(() => history.length);
  await page.locator("header > a", { hasText: "Home" }).click();
  await shows(page, "Home");
  expect(await page.evaluate(() => history.length)).toBe(before + 1);
  await page.locator("header > a", { hasText: "Recent" }).click();
  await shows(page, "Recent");
  if (hasTouch) {
    await right(page);
    await shows(page, "Today");
  }
  await page.locator("header > a", { hasText: "Home" }).click();
  await shows(page, "Home");
  expect(await page.evaluate(() => history.length)).toBe(before + 1);
  await page.goBack();
  await expect(page).toHaveURL(base + "/a");
  await expect(page.locator("main")).toContainText("aye text");
});

test("Back from a note returns to the tab the reader left, not the one first loaded", async ({ page }) => {
  await page.goto(base + "/");
  await shows(page, "Home");
  await page.locator("header > a", { hasText: "Recent" }).click();
  await shows(page, "Recent");
  await page.locator("main a", { hasText: "Fern care" }).click();
  await expect(page.locator("main")).toContainText("water weekly");
  await page.goBack();
  await shows(page, "Recent");
  await live(page, RECENT);
});

test("a tab page switched to in place is that tab's page in full", async ({ page }) => {
  await page.goto(base + "/");
  await synced(page, "/", TODAY, RECENT, latest);
  await live(page, "/");
  const { pages } = await (await page.request.get(base + "/.tachylite/manifest.json")).json();
  const version = () => page.locator('meta[name="tachylite-version"]').getAttribute("content");

  await page.locator("header > a", { hasText: "Today" }).click();
  await shows(page, "Today");
  await expect(page).toHaveTitle(iso(-1));
  expect(await version()).toBe(pages[latest]);
  await live(page, latest);
  // The stream of the tab just left is closed, so no push for it can arrive late.
  expect((await streams(page)).filter(([, state]) => state !== 2)).toEqual([[latest, 1]]);
  await expect(page.locator("main pre.mermaid svg")).toBeVisible();

  await page.locator("header > a", { hasText: "Recent" }).click();
  await shows(page, "Recent");
  await expect(page).toHaveTitle("Recent");
  expect(await version()).toBe(pages[RECENT]);
  await live(page, RECENT);

  // A save to the note of the tab just left changes nothing here but its row.
  note(`days/${iso(-1)}`, "---\ntitle: retitled\n---\n\nlate text\n");
  await expect(page.locator("main ul.recent").first()).toContainText("retitled");
  await expect(page.locator("main")).not.toContainText("late text");
  await shows(page, "Recent");
  // The push for the tab shown is stored under that tab's URL.
  await expect.poll(() => cached(page, RECENT)).toContain("retitled");
  expect(await cached(page, "/")).toContain('class="folders"');
  expect(await cached(page, TODAY)).toContain("data-date");

  // A reload shows the tab the URL names.
  await page.reload();
  await shows(page, "Recent");
  await expect(page.locator("main ul.recent").first()).toContainText("retitled");
});

test("a tab switched to starts at the top", async ({ page }) => {
  for (let i = 0; i < 40; i++) note(`n${i}`, `# Note ${i}\n`);
  await expect.poll(async () => (await page.request.get(base + RECENT)).text()).toContain("n39");
  await page.goto(base + RECENT);
  await shows(page, "Recent");
  await page.evaluate(() => scrollTo(0, 600));
  await expect.poll(() => page.evaluate(() => scrollY)).toBeGreaterThan(300);
  await page.locator("header > a", { hasText: "Home" }).click();
  await shows(page, "Home");
  await expect.poll(() => page.evaluate(() => scrollY)).toBe(0);
});

test("a stale copy of a tab is corrected once its stream connects", async ({ page }) => {
  await page.goto(base + "/");
  await synced(page, "/", RECENT);
  await live(page, "/");
  // The push reaches the home page alone, so the copy the client holds of the Recent page falls behind.
  note("fresh", "# Fresh\n\nfresh text\n");
  await expect(page.locator("main ul.recent")).toContainText("fresh");
  expect(await cached(page, RECENT)).not.toContain("fresh");
  await page.locator("header > a", { hasText: "Recent" }).click();
  await shows(page, "Recent");
  await expect(page.locator("main ul.recent").last()).toContainText("fresh");
});

// The client holds parsed copies of every other tab, read from the worker's cache.
test("a copy held ahead is read again after a sync, so a tab opens fresh offline", async ({ page, context }) => {
  await page.goto(base + TODAY);
  await shows(page, "Today");
  await synced(page, "/", TODAY, RECENT, latest);
  await live(page, latest);
  // `queue` is the chain of jobs in `src/sw.js`. It settles when the first sync has told the page.
  await context.serviceWorkers()[0].evaluate("queue");
  await rested(page);
  await setHidden(page, true);
  note("fresh", "# Fresh\n\nfresh text\n");
  await expect.poll(async () => (await page.request.get(base + RECENT)).text()).toContain("fresh");
  await page.evaluate(() => {
    const told = new Promise((r) => navigator.serviceWorker.addEventListener("message", () => r(1), { once: true }));
    (window as any).told = told;
  });
  // Visible again, the stream opens, the worker syncs the Recent page and tells the page.
  await setHidden(page, false);
  await page.evaluate(() => (window as any).told);
  expect(await cached(page, RECENT)).toContain("fresh");
  await stop(server!);
  await page.locator("header > a", { hasText: "Recent" }).click();
  await shows(page, "Recent");
  await expect(page.locator("main ul.recent").last()).toContainText("fresh");
});

test("a switch to another tab reads only the tab it left: the others' copies are held", async ({ page, context }) => {
  await page.addInitScript(() => {
    const reads: string[] = ((window as any).reads = []);
    const match = caches.match.bind(caches);
    caches.match = (request: RequestInfo | URL, options?: MultiCacheQueryOptions) => {
      reads.push(String(request));
      return match(request, options);
    };
  });
  await page.goto(base + "/");
  await synced(page, "/", TODAY, RECENT, latest);
  await live(page, "/");
  // `queue` is the chain of jobs in `src/sw.js`. It settles when the first sync has told the page.
  await context.serviceWorkers()[0].evaluate("queue");
  await rested(page);
  expect(await page.evaluate(() => (window as any).reads)).toEqual(expect.arrayContaining([TODAY, latest, RECENT]));
  await page.evaluate(() => ((window as any).reads.length = 0));
  await page.locator("header > a", { hasText: "Today" }).click();
  await shows(page, "Today");
  // The switch reads the tab it left, in the same task that put Today on screen.
  expect(await page.evaluate(() => (window as any).reads)).toEqual(["/"]);
});

test("a vault with no daily settings swipes between its two tabs", async ({ page, hasTouch }) => {
  test.skip(!hasTouch, "a swipe takes a finger");
  writeFileSync(join(dir, "vault", "tachylite.toml"), "");
  await expect.poll(async () => (await page.request.get(base + "/")).text()).not.toContain(TODAY);
  await page.goto(base + "/");
  await expect(page.locator("header > a")).toHaveText(["Home", "Recent"]);
  await left(page);
  await shows(page, "Recent");
  await right(page);
  await shows(page, "Home");
});

test("the Today tab picks its note when it opens, not when its copy was read ahead", async ({ page }) => {
  await page.goto(base + "/");
  await synced(page, "/", TODAY, latest, `/days/${iso(2)}`);
  await live(page, "/");
  await rested(page);
  // Three days on, the note planned for the day after tomorrow is the newest up to today.
  await page.clock.setFixedTime(Date.now() + 3 * 864e5);
  await page.locator("header > a", { hasText: "Today" }).click();
  await expect(page.locator("main h1")).toHaveText("planned");
  await expect(page).toHaveURL(base + TODAY);
  await live(page, `/days/${iso(2)}`);
});

test("a deleted note's notice does not follow the reader to another tab", async ({ page, allowedErrors }) => {
  // The cached Today list still names the note, so the copy read ahead of a switch finds it gone.
  allowedErrors.push(/status of 404/);
  await page.clock.install();
  await page.goto(base + TODAY);
  await shows(page, "Today");
  await live(page, latest);
  await page.evaluate(() => {
    const stream = (window as any).streams.at(-1) as EventSource;
    stream.addEventListener("gone", () => ((window as any).gone = true));
  });
  rmSync(join(dir, "vault", "days", `${iso(-1)}.md`));
  await page.waitForFunction(() => (window as any).gone === true);
  await page.locator("header > a", { hasText: "Home" }).click();
  await shows(page, "Home");
  await page.clock.runFor(1500);
  await shows(page, "Home");
  await expect(page.locator("main")).not.toContainText("deleted or moved");
});
