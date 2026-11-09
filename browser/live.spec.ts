import { expect, test, type Page } from "@playwright/test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { serve, stop } from "./server";

const vault = join(__dirname, ".tmp", "vault");
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

let errors: string[] = [];
test.beforeEach(async ({ page }) => {
  // Marks the page live once its event stream opens, without a hook in the shipped script.
  await page.addInitScript(() => {
    const Native = window.EventSource;
    window.EventSource = class extends Native {
      constructor(url: string | URL, init?: EventSourceInit) {
        super(url, init);
        this.addEventListener("open", () => ((window as any).liveOpen = true));
        this.addEventListener("error", () => ((window as any).liveOpen = false));
      }
    };
  });
  errors = [];
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(e.message));
});

test("push swaps main", async ({ page }, info) => {
  const name = `push-${info.project.name}`;
  note(name, "# First\n\nfirst text\n");
  await open(page, name, "first text");
  note(name, "# Second\n\nsecond text\n");
  await expect(page.locator("main")).toContainText("second text");
  expect(errors).toEqual([]);
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
  expect(errors).toEqual([]);
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
  expect(errors).toEqual([]);
});

// Offline emulation keeps an open event stream alive, so a lost connection is a server
// that stops and starts again. The client reconnects and sends its Last-Event-ID.
test("catches up after a lost connection", async ({ page }, info) => {
  const dir = join(__dirname, ".tmp", `restart-${info.project.name}`);
  mkdirSync(join(dir, "vault"), { recursive: true });
  const port = 7432;
  const file = join(dir, "vault", "away.md");
  writeFileSync(file, "# Away\n\nonline text\n");
  let server = await serve(dir, port);
  try {
    await page.goto(`http://127.0.0.1:${port}/away`);
    await expect(page.locator("main")).toContainText("online text");
    await page.waitForFunction(() => (window as any).liveOpen === true);
    await stop(server);
    await page.waitForFunction(() => (window as any).liveOpen === false);
    writeFileSync(file, "# Away\n\nmissed text\n");
    server = await serve(dir, port);
    await expect(page.locator("main")).toContainText("missed text", { timeout: 15_000 });
  } finally {
    await stop(server);
  }
  // The stop cuts the stream and refuses the first retries; nothing else may log.
  const cut = /net::ERR_(INCOMPLETE_CHUNKED_ENCODING|CONNECTION_REFUSED)/;
  expect(errors.filter((e) => !cut.test(e))).toEqual([]);
});

// Vim and git save by removing the note, then writing it again. The page must survive that.
test("a deleted note shows a notice and comes back", async ({ page }, info) => {
  const name = `gone-${info.project.name}`;
  note(name, "# Gone\n\nsoon gone\n");
  await open(page, name, "soon gone");
  await page.evaluate(() => ((window as any).stayed = true));
  rmSync(join(vault, `${name}.md`));
  await expect(page.locator("main")).toContainText("deleted");
  note(name, "# Gone\n\nback again\n");
  await expect(page.locator("main")).toContainText("back again");
  expect(await page.evaluate(() => (window as any).stayed)).toBe(true);
  expect(errors).toEqual([]);
});

test("a save that removes the note first never flashes the notice", async ({ page }, info) => {
  const name = `resave-${info.project.name}`;
  note(name, "# Resave\n\nround start\n");
  await open(page, name, "round start");
  await page.evaluate(() => {
    new MutationObserver(() => {
      if (document.querySelector("main")?.textContent?.includes("deleted")) {
        (window as any).flashed = true;
      }
    }).observe(document.body, { childList: true, subtree: true, characterData: true });
  });
  for (let round = 0; round < 10; round++) {
    rmSync(join(vault, `${name}.md`));
    note(name, `# Resave\n\nround ${round}\n`);
    await expect(page.locator("main")).toContainText(`round ${round}`);
  }
  expect(await page.evaluate(() => (window as any).flashed ?? false)).toBe(false);
  expect(errors).toEqual([]);
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
  expect(errors).toEqual([]);
});

test("a page with a Mermaid block draws on first load", async ({ page }, info) => {
  const name = `mermaid-load-${info.project.name}`;
  note(name, "# Loaded\n\n```mermaid\ngraph LR\nX-->Y\n```\n");
  await open(page, name, "Loaded");
  await expect(page.locator("main pre.mermaid svg")).toBeVisible();
  expect(errors).toEqual([]);
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
  expect(errors).toEqual([]);
});

test("a script in a note never runs", async ({ page }, info) => {
  const name = `script-${info.project.name}`;
  note(name, `# Scripted\n\n<img src="x.png" onerror="window.ran = 1">\n\n<script>window.ran = 2</script>\n`);
  await open(page, name, "Scripted");
  expect(await page.evaluate(() => (window as any).ran ?? 0)).toBe(0);
});

test("an svg opened as a page cannot run script on the origin", async ({ page }, info) => {
  const name = `svg-${info.project.name}`;
  writeFileSync(join(vault, `${name}-payload.html`), `document.title = "PWNED";`);
  writeFileSync(
    join(vault, `${name}.svg`),
    `<svg xmlns="http://www.w3.org/2000/svg"><script href="/${name}-payload.html"/></svg>`,
  );
  await expect
    .poll(async () => (await page.request.get(`/${name}.svg`)).status())
    .toBe(200);
  await page.goto(`/${name}.svg`, { waitUntil: "load" });
  expect(await page.title()).not.toBe("PWNED");
});
