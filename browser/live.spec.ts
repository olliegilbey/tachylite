import type { Page } from "@playwright/test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, lost, run, setHidden, test } from "./fixture";
import { serve, stop } from "./server";

const vault = join(run, "vault");
const scale = Number(process.env.BUDGET_SCALE ?? "1");

// Each test writes its own note, named for the test and project, so no two tests share one.
function note(name: string, body: string) {
  writeFileSync(join(vault, `${name}.md`), body);
}

async function open(page: Page, name: string, text: string) {
  await expect
    .poll(async () => (await page.request.get(`/${name}`)).status())
    .toBe(200);
  await page.goto(`/${name}`);
  await expect(page.locator("main")).toContainText(text);
  await page.waitForFunction(() => (window as any).liveOpen === true);
}

test.beforeEach(async ({ page }) => {
  // Marks the page live once its event stream opens, without a hook in the shipped script.
  await page.addInitScript(() => {
    const Native = window.EventSource;
    window.EventSource = class extends Native {
      constructor(url: string | URL, init?: EventSourceInit) {
        super(url, init);
        const w = window as any;
        (w.streams ??= []).push(this);
        this.addEventListener("open", () => (w.liveOpen = true));
        this.addEventListener("error", () => (w.liveOpen = false));
        this.addEventListener("gone", () => (w.gones = (w.gones ?? 0) + 1));
      }
    };
  });
});

test("push swaps main", async ({ page }, info) => {
  const name = `push-${info.project.name}`;
  note(name, "# First\n\nfirst text\n");
  await open(page, name, "first text");
  note(name, "# Second\n\nsecond text\n");
  await expect(page.locator("main")).toContainText("second text");
});

test("a push keeps a callout the reader opened open", async ({ page }, info) => {
  const name = `fold-${info.project.name}`;
  note(name, "# Fold\n\n> [!tip]- Folded\n> hidden body\n\nfirst text\n");
  await open(page, name, "first text");
  await page.locator(".callout-title").click();
  note(name, "# Fold\n\n> [!tip]- Folded\n> hidden body\n\nsecond text\n");
  await expect(page.locator("main")).toContainText("second text");
  await expect(page.locator(".callout-content")).toBeVisible();
});

test("the page asks once to keep its storage, and shows an icon", async ({ page }, info) => {
  await page.addInitScript(() => {
    const w = window as any;
    w.persists = 0;
    navigator.storage.persist = () => (w.persists++, Promise.reject(new Error("refused")));
  });
  const name = `persist-${info.project.name}`;
  note(name, "# Persist\n\npersist text\n");
  await open(page, name, "persist text");
  await expect.poll(() => page.evaluate(() => (window as any).persists)).toBe(1);
  await page.evaluate(() => (window as any).streams.at(-1).dispatchEvent(new Event("open")));
  expect(await page.evaluate(() => (window as any).persists)).toBe(1);
  const icon = await page.locator('link[rel="icon"]').getAttribute("href");
  const r = await page.request.get(icon!);
  expect(r.status()).toBe(200);
  expect(r.headers()["content-type"]).toContain("image/svg+xml");
});

test("a backlink appears on an open page", async ({ page }, info) => {
  const target = `target-${info.project.name}`;
  const linker = `linker-${info.project.name}`;
  note(target, "# Target\n\ntarget text\n");
  await open(page, target, "target text");
  note(linker, `# Linker\n\nsee [[${target}]]\n`);
  await expect(page.locator("main .backlinks")).toContainText(linker);
  await page.locator("main .backlinks a").click();
  await expect(page.locator("main")).toContainText(`see ${target}`);
});

test("save to screen within budget", async ({ page }, info) => {
  const name = `budget-${info.project.name}`;
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 4 });
  await page.addInitScript(() => {
    new MutationObserver(() => {
      if (document.querySelector("main")?.textContent?.includes("after save")) {
        (window as any).swappedAt ??= performance.timeOrigin + performance.now();
      }
    }).observe(document, { childList: true, subtree: true });
  });
  note(name, "# Budget\n\nbefore save\n");
  await open(page, name, "before save");
  const savedAt = Date.now();
  note(name, "# Budget\n\nafter save\n");
  const swappedAt = await page
    .waitForFunction(() => (window as any).swappedAt)
    .then((h) => h.jsonValue() as Promise<number>);
  const gap = swappedAt - savedAt;
  console.log(`save to screen, ${info.project.name}, 4x CPU throttle: ${gap.toFixed(1)} ms`);
  expect(gap).toBeLessThanOrEqual(100 * scale);
});

// Offline emulation keeps an open event stream alive, so a lost connection is a server
// that stops and starts again. The client reconnects and sends its Last-Event-ID.
test("catches up after a lost connection", async ({ page, allowedErrors }, info) => {
  allowedErrors.push(lost);
  const dir = join(run, `restart-${info.project.name}`);
  mkdirSync(join(dir, "vault"), { recursive: true });
  const file = join(dir, "vault", "away.md");
  writeFileSync(file, "# Away\n\nonline text\n");
  let server = await serve(dir);
  try {
    await page.goto(`${server.base}/away`);
    await expect(page.locator("main")).toContainText("online text");
    await page.waitForFunction(() => (window as any).liveOpen === true);
    await stop(server);
    await page.waitForFunction(() => (window as any).liveOpen === false);
    writeFileSync(file, "# Away\n\nmissed text\n");
    server = await serve(dir, server.port);
    await expect(page.locator("main")).toContainText("missed text", { timeout: 15_000 });
  } finally {
    await stop(server);
  }
});

// A proxy answers an HTTP error while the server is down, and an EventSource closes for good
// on one. The page opens a new stream, which catches up.
test("an HTTP error on the stream does not stop live updates", async ({ page, context, allowedErrors }, info) => {
  allowedErrors.push(/502/);
  const name = `proxy-${info.project.name}`;
  note(name, "# Proxy\n\nbefore error\n");
  await expect.poll(async () => (await page.request.get(`/${name}`)).status()).toBe(200);
  let refused = 0;
  await context.route(/\/\.tachylite\/events/, (route) => {
    refused++;
    return route.fulfill({ status: 502, body: "bad gateway" });
  });
  await page.goto(`/${name}`);
  await expect.poll(() => refused).toBeGreaterThan(0);
  await page.waitForFunction(() => (window as any).streams.at(-1).readyState === 2);
  await context.unroute(/\/\.tachylite\/events/);
  note(name, "# Proxy\n\nafter error\n");
  await expect(page.locator("main")).toContainText("after error", { timeout: 15_000 });
});

// A browser allows six HTTP/1.1 connections per origin, so a hidden tab gives its stream up.
test("a hidden tab closes its stream and catches up once visible", async ({ page }, info) => {
  const name = `hidden-${info.project.name}`;
  note(name, "# Hidden\n\nbefore hide\n");
  await open(page, name, "before hide");
  await setHidden(page, true);
  expect(await page.evaluate(() => (window as any).streams.map((s: EventSource) => s.readyState))).toEqual([2]);
  note(name, "# Hidden\n\nwhile hidden\n");
  await expect.poll(async () => (await page.request.get(`/${name}`)).text()).toContain("while hidden");
  await setHidden(page, false);
  await expect(page.locator("main")).toContainText("while hidden");
});

// After `gone` the client's version is `gone`, so a note back with the same bytes is sent.
test("a note back unchanged after a lost connection replaces the notice", async ({ page, allowedErrors }, info) => {
  allowedErrors.push(lost);
  const dir = join(run, `back-${info.project.name}`);
  mkdirSync(join(dir, "vault"), { recursive: true });
  const file = join(dir, "vault", "back.md");
  writeFileSync(file, "# Back\n\nsame text\n");
  let server = await serve(dir);
  try {
    await page.goto(`${server.base}/back`);
    await page.waitForFunction(() => (window as any).liveOpen === true);
    rmSync(file);
    await expect(page.locator("main")).toContainText("This page was deleted or moved.");
    await stop(server);
    await page.waitForFunction(() => (window as any).liveOpen === false);
    writeFileSync(file, "# Back\n\nsame text\n");
    server = await serve(dir, server.port);
    await expect(page.locator("main")).toContainText("same text", { timeout: 15_000 });
  } finally {
    await stop(server);
  }
});

// Vim and git save by removing the note, then writing it again. The page must survive that.
test("a deleted note shows a notice and comes back", async ({ page }, info) => {
  const name = `gone-${info.project.name}`;
  note(name, "# Gone\n\nsoon gone\n");
  await open(page, name, "soon gone");
  await page.evaluate(() => ((window as any).stayed = true));
  rmSync(join(vault, `${name}.md`));
  await expect(page.locator("main")).toContainText("This page was deleted or moved.");
  note(name, "# Gone\n\nback again\n");
  await expect(page.locator("main")).toContainText("back again");
  expect(await page.evaluate(() => (window as any).stayed)).toBe(true);
});

// Each round waits for the `gone`, then runs the page's clock past the notice's second.
test("a save that removes the note first never flashes the notice", async ({ page }, info) => {
  const name = `resave-${info.project.name}`;
  await page.clock.install();
  note(name, "# Resave\n\nround start\n");
  await open(page, name, "round start");
  await page.evaluate(() => {
    new MutationObserver(() => {
      if (document.querySelector("main")?.textContent?.includes("deleted")) {
        (window as any).flashed = true;
      }
    }).observe(document.body, { childList: true, subtree: true, characterData: true });
  });
  for (let round = 0; round < 3; round++) {
    rmSync(join(vault, `${name}.md`));
    await expect.poll(() => page.evaluate(() => (window as any).gones)).toBe(round + 1);
    note(name, `# Resave\n\nround ${round}\n`);
    await expect(page.locator("main")).toContainText(`round ${round}`);
    await page.clock.runFor(1500);
  }
  expect(await page.evaluate(() => (window as any).flashed ?? false)).toBe(false);
});

test("a Mermaid block draws, and only its page loads the script", async ({ page }, info) => {
  const name = `mermaid-${info.project.name}`;
  const requested: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/.tachylite/mermaid.js")) requested.push(r.url());
  });
  note(name, "# Plain\n\nno diagram yet\n");
  await open(page, name, "no diagram yet");
  expect(requested).toEqual([]);
  note(name, "# Drawn\n\n```mermaid\ngraph TD\nA-->B\n```\n");
  await expect(page.locator("main pre.mermaid svg")).toBeVisible();
  expect(requested.length).toBe(1);
  // A second push redraws from the source, with the script already loaded.
  note(name, "# Drawn again\n\n```mermaid\ngraph TD\nC-->D\n```\n");
  await expect(page.locator("main h1")).toHaveText("Drawn again");
  await expect(page.locator("main pre.mermaid svg")).toBeVisible();
  expect(requested.length).toBe(1);
});

test("a page with a Mermaid block draws on first load", async ({ page }, info) => {
  const name = `mermaid-load-${info.project.name}`;
  note(name, "# Loaded\n\n```mermaid\ngraph LR\nX-->Y\n```\n");
  await open(page, name, "Loaded");
  await expect(page.locator("main pre.mermaid svg")).toBeVisible();
});

test("an embedded html page runs its script in an opaque origin", async ({ page }, info) => {
  const name = `embed-html-${info.project.name}`;
  writeFileSync(
    join(vault, `${name}.html`),
    `<!doctype html><p id="out">waiting</p><script>
      const probe = (f) => { try { f(); return "open"; } catch { return "blocked"; } };
      document.getElementById("out").textContent =
        "ran, parent " + probe(() => parent.document.title) + ", storage " + probe(() => localStorage.length);
    </script>`,
  );
  note(name, `# Embeds\n\n![[${name}.html]]\n`);
  await open(page, name, "Embeds");
  await expect(page.frameLocator("main iframe.embed-html").locator("#out")).toHaveText(
    "ran, parent blocked, storage blocked",
  );
});

test("a script in a note never runs", async ({ page, allowedErrors }, info) => {
  allowedErrors.push(/status of 404/);
  const name = `script-${info.project.name}`;
  note(name, `# Scripted\n\n<img src="x.png" onerror="window.ran = 1">\n\n<script>window.ran = 2</script>\n`);
  await open(page, name, "Scripted");
  expect(await page.evaluate(() => (window as any).ran ?? 0)).toBe(0);
});

// The script runs, so the sandbox, not the main policy, is what keeps it off the origin.
test("an svg opened as a page cannot run script on the origin", async ({ page }, info) => {
  const name = `svg-${info.project.name}`;
  writeFileSync(
    join(vault, `${name}.svg`),
    `<svg xmlns="http://www.w3.org/2000/svg"><text id="out" y="20">waiting</text><script>
      let storage = "open";
      try { localStorage.length; } catch { storage = "blocked"; }
      document.getElementById("out").textContent = "ran, storage " + storage;
    </script></svg>`,
  );
  await expect
    .poll(async () => (await page.request.get(`/${name}.svg`)).status())
    .toBe(200);
  await page.goto(`/${name}.svg`);
  await expect(page.locator("#out")).toHaveText("ran, storage blocked");
});
