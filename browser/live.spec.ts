import { expect, test, type Page } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

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

const binary = join(__dirname, "..", "target", "release", "tachylite");

/** Starts a server on `dir/vault`, resolving once it prints its address. */
function serve(dir: string, port: number): Promise<ChildProcess> {
  const child = spawn(binary, ["serve", join(dir, "vault"), join(dir, "out"), "--port", `${port}`]);
  return new Promise((resolve, reject) => {
    child.stderr!.on("data", (chunk: Buffer) => {
      if (chunk.toString().includes("serving http://")) resolve(child);
    });
    child.on("exit", (code) => reject(new Error(`serve exited with ${code}`)));
  });
}

/** Stops a server by the PID captured at spawn. */
function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    child.on("exit", () => resolve());
    child.kill();
  });
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
