import { expect, type Browser, type BrowserContext, type Page, type Request, type Route } from "@playwright/test";
import { cpSync, readFileSync, writeFileSync } from "node:fs";
import { get } from "node:http";
import { cpus, loadavg } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { serve, stop, type Server } from "../server";
import type { Cell, Result, Sample } from "./report";
import { profiles, scenarios, type Profile, type Saved, type Scenario, type Until, type Vars, type Warm } from "./scenarios";

export type Options = {
  /** A scratch directory. Each binary gets a copy of the vault and an output directory in it. */
  dir: string;
  /** The vault to serve. */
  vault: string;
  /** The binaries to compare, sampled in turn. No `path` means this checkout's release build. */
  binaries: { label: string; path?: string }[];
  /** Names to run. Left out, every scenario and every profile runs. */
  scenarios?: string[];
  profiles?: string[];
  /** The quiet samples to take in each cell. */
  samples: number;
  /** Samples taken and thrown away first. */
  warmup: number;
  /** A sample taken with a smaller share of the host's CPU idle is dropped, and taken again. */
  minIdle: number;
  /** The longest random wait before a sample's clock starts, in ms. Zero starts it at once. */
  jitter: number;
  log?: (line: string) => void;
};

const MANIFEST = "/.tachylite/manifest.json";
const CLIENT = ["live.js", "sw.js", "style.css"];
/** Where the clock's start crosses a navigation. */
const KEY = "tachylite-bench";
/** Set while a tab switch is timed, so a document the switch opens watches its own `<main>`. */
const SWITCHING = "tachylite-bench-tab";

type Session = {
  label: string;
  base: string;
  /** The vault this binary serves, which a `save` scenario writes. */
  vault: string;
  context: BrowserContext;
  page: Page;
  /** Resolves once no fetch of the worker's is in flight. */
  quiet: () => Promise<void>;
  /** Uncaught exceptions in the page. One fails the run. */
  errors: string[];
};

type Skip = { skip: string };

/** Runs every picked scenario under every picked profile, against each binary in turn. */
export async function bench(browser: Browser, o: Options): Promise<Result> {
  const picked = pick(scenarios, o.scenarios, "scenario");
  const devices = pick(profiles, o.profiles, "profile");
  const servers: { label: string; server: Server }[] = [];
  const loadBefore = loadavg()[0];
  try {
    for (const { label, path } of o.binaries) {
      const dir = join(o.dir, label);
      // The same modification times in each copy, so each binary lists the same recent notes.
      cpSync(o.vault, join(dir, "vault"), { recursive: true, preserveTimestamps: true });
      servers.push({ label, server: await serve(dir, 0, path) });
    }
    const vars = await variables(servers[0].server.base);
    const cells: Cell[] = [];
    for (const profile of devices) {
      const sessions: Session[] = [];
      try {
        for (const { label, server } of servers) {
          const first = open(browser, profile, label, server.base, join(o.dir, label, "vault"), sessions);
          await within(first, `the first sync on ${profile.name}, ${label}`, 180);
        }
        for (const scenario of picked) {
          const row = sessions.map((session) => ({
            session,
            cell: { scenario: scenario.name, profile: profile.name, binary: session.label, samples: [] } as Cell,
          }));
          const quiet = (c: Cell) => c.samples.filter((s) => s.idle >= o.minIdle).length;
          const done = (c: Cell) => !!c.skipped || quiet(c) >= o.samples;
          // One sample from each binary a turn, so a change in host load reaches them alike.
          for (let turn = -o.warmup; turn < o.samples * 4 && !row.every((r) => done(r.cell)); turn++) {
            for (const { session, cell } of row) {
              if (done(cell)) continue;
              const what = `${cell.scenario} on ${cell.profile}, ${cell.binary}`;
              const got = await within(sample(browser, profile, session, scenario, vars, o.jitter), what);
              if (session.errors.length) throw new Error(`${what}: the page threw: ${session.errors.join("; ")}`);
              if ("skip" in got) cell.skipped = got.skip;
              else if (turn >= 0) cell.samples.push(got);
            }
          }
          for (const { cell } of row) {
            cells.push(cell);
            o.log?.(`${cell.scenario}, ${cell.profile}, ${cell.binary}: ${cell.skipped ?? `${quiet(cell)} of ${cell.samples.length} samples kept`}`);
          }
        }
      } finally {
        for (const s of sessions) await s.context.close();
      }
    }
    const binaries = [];
    for (const { label, server } of servers) {
      const client: Record<string, number> = {};
      for (const file of CLIENT) client[file] = await gzipped(`${server.base}/.tachylite/${file}`);
      binaries.push({ label, client });
    }
    return {
      date: new Date().toISOString().slice(0, 10),
      chromium: browser.version(),
      samples: o.samples,
      minIdle: o.minIdle,
      load: [loadBefore, loadavg()[0]],
      binaries,
      cells,
    };
  } finally {
    for (const { server } of servers) await stop(server);
  }
}

function pick<T extends { name: string }>(all: T[], names: string[] | undefined, kind: string): T[] {
  if (!names?.length) return all;
  return names.map((name) => {
    const found = all.find((a) => a.name === name);
    if (!found) throw new Error(`no ${kind} named ${name}: there is ${all.map((a) => a.name).join(", ")}`);
    return found;
  });
}

async function variables(base: string): Promise<Vars> {
  // The switcher list: each note or html page as its title, its URL, then its path and aliases.
  // An html page is sandboxed, so the bench cannot time one: it takes a note.
  const all: [string, string][] = await (await fetch(`${base}/.tachylite/notes.json`)).json();
  const notes = all.filter(([, url]) => !url.endsWith(".html"));
  const [title, note, file] = notes[Math.floor(notes.length / 2)];
  return { note, query: title.slice(0, 3).toLowerCase(), file };
}

function fill(text: string, vars: Vars): string {
  return text.replace(/\{(\w+)\}/g, (_, name: keyof Vars) => {
    if (!(name in vars)) throw new Error(`no variable named ${name} for ${text}`);
    return vars[name];
  });
}

/** The bytes of a client file as the server sends it to a client that takes gzip. */
function gzipped(url: string): Promise<number> {
  return new Promise((resolve, reject) => {
    get(url, { headers: { "accept-encoding": "gzip" } }, (response) => {
      let bytes = 0;
      response.on("data", (chunk: Buffer) => (bytes += chunk.length)).on("end", () => resolve(bytes));
    }).on("error", reject);
  });
}

/** A promise that may lose a race. If it rejects after the run has failed, it adds no second error. */
function ahead<T>(wait: Promise<T>): Promise<T> {
  wait.catch(() => {});
  return wait;
}

/** A step that hangs names itself and fails the run, rather than holding it for ever. */
function within<T>(step: Promise<T>, what: string, seconds = 60): Promise<T> {
  let timer: NodeJS.Timeout;
  const late = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what}: no result within ${seconds} s`)), seconds * 1000);
  });
  return Promise.race([ahead(step), late]).finally(() => clearTimeout(timer));
}

/** The host's CPU time so far, summed over its cores. */
function cpu() {
  let idle = 0;
  let total = 0;
  for (const { times } of cpus()) {
    idle += times.idle;
    total += times.user + times.nice + times.sys + times.idle + times.irq;
  }
  return { idle, total };
}

/** The share of CPU time since `from` that was idle. The bench's own work counts as busy. */
function idleSince(from: { idle: number; total: number }): number {
  const now = cpu();
  if (now.total === from.total) return 1;
  return Math.round(((now.idle - from.idle) / (now.total - from.total)) * 1000) / 1000;
}

/** A page the worker controls, with every listed page in its cache, at the profile's CPU speed. */
async function open(browser: Browser, profile: Profile, label: string, base: string, vault: string, sessions: Session[]) {
  const context = await browser.newContext({ ...profile.device, baseURL: base });
  const flying = new Set<Request>();
  let landed: (() => void) | null = null;
  const end = (r: Request) => {
    flying.delete(r);
    if (!flying.size) landed?.();
  };
  context.on("request", (r) => r.serviceWorker() && flying.add(r));
  context.on("requestfinished", end);
  context.on("requestfailed", end);
  const quiet = () => (flying.size ? new Promise<void>((resolve) => (landed = resolve)) : Promise.resolve());

  const cores = profile.cores;
  if (cores) await context.addInitScript(`Object.defineProperty(navigator, "hardwareConcurrency", { value: ${cores} })`);
  // A sandboxed frame has no storage to read.
  await context.addInitScript({
    content: `try { if (sessionStorage.getItem(${JSON.stringify(SWITCHING)})) (${watchMain})(); } catch {}`,
  });
  const page = await context.newPage();
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  // Listed before the first wait, so a session that never syncs is still closed.
  sessions.push({ label, base, vault, context, page, quiet, errors });
  await page.goto("/");
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  const { pages } = await (await page.request.get(MANIFEST)).json();
  // `waitForFunction` takes a promise for true, so the page answers and the bench polls.
  const stored = () =>
    page.evaluate(
      async ([manifest, want]) => {
        const have = (await (await caches.match(manifest))?.json()) ?? {};
        const urls = Object.keys(want);
        return urls.length === Object.keys(have).length && urls.every((url) => have[url] === want[url]);
      },
      [MANIFEST, pages] as const,
    );
  // The caller sets the limit.
  await expect.poll(stored, { timeout: 0 }).toBe(true);
  const cdp = await context.newCDPSession(page);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: profile.cpu });
}

/** The worker's next fetch of the manifest: a page asks for a sync each time its event stream opens. */
function nextSync(s: Session): Promise<unknown> {
  const manifest = (r: Request) => !!r.serviceWorker() && new URL(r.url()).pathname === MANIFEST;
  return ahead(s.context.waitForEvent("requestfinished", manifest));
}

/** Waits until a newly shown page is at rest: its sync done, the worker idle, the page idle. */
async function rest(s: Session, synced: Promise<unknown>) {
  await synced;
  // `queue` is the chain of jobs in `src/sw.js`. It settles when the sync has stored its last page.
  await s.context.serviceWorkers()[0].evaluate("queue");
  await s.quiet();
  // A page back from the back/forward cache gets no idle callback, so a background task marks idle.
  await s.page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => (window as any).scheduler.postTask(r, { priority: "background" }))),
  );
}

async function load(s: Session, path: string) {
  const synced = nextSync(s);
  await s.page.goto(path);
  await rest(s, synced);
}

/** A call of `fn` as source, for the page to run. `page.evaluate` passes data only, and an action is code. */
const call = (fn: Function, action: string, ...data: unknown[]) =>
  `(${fn})(${[action, ...data.map((d) => JSON.stringify(d))].join(", ")})`;

/** In the page: starts the clock, then acts. */
async function act(run: (vars: Vars) => void, vars: Vars, key: string, jitter: number) {
  // A real tap lands anywhere in a frame, so the clock starts at a random point in one.
  if (jitter) await new Promise((r) => setTimeout(r, Math.random() * jitter));
  sessionStorage.setItem(key, String(performance.timeOrigin + performance.now()));
  run(vars);
}

/** In the page: one finger across the middle of `<main>`, as the touch events a swipe sends. */
function swipe(type: "touchstart" | "touchend", by: number) {
  const main = document.querySelector("main")!;
  const touch = new Touch({ identifier: 1, target: main, clientX: innerWidth / 2 + by, clientY: innerHeight / 2 });
  const touches = type === "touchend" ? [] : [touch];
  main.dispatchEvent(new TouchEvent(type, { bubbles: true, touches, changedTouches: [touch] }));
}

/**
 * In the page: `window.benchTab` resolves at the first paint of text or an image inside a
 * `<main>`, or part of one, that enters the document from now on. A tab's content can come
 * with a new document or be put in place, and this times both the same way.
 */
function watchMain() {
  const old = new Set(document.querySelectorAll("main, main *"));
  let dom = 0;
  // Element Timing reports a paint on an element that carried the attribute before it painted.
  const mark = () => {
    for (const e of document.querySelectorAll("main, main *")) {
      if (old.has(e)) continue;
      old.add(e);
      dom ||= performance.timeOrigin + performance.now();
      e.setAttribute("elementtiming", "bench");
    }
  };
  new MutationObserver(mark).observe(document, { childList: true, subtree: true });
  (window as any).benchTab = new Promise((resolve) => {
    new PerformanceObserver((list, observer) => {
      const first = list.getEntries().find((e) => (e as any).identifier === "bench");
      if (!first) return;
      observer.disconnect();
      resolve({ painted: performance.timeOrigin + (first as any).renderTime, dom });
    }).observe({ type: "element" });
  });
}

/** In the page: watches `<main>`, lands the finger if `before` does, starts the clock, then acts. */
async function actOnTab(run: () => void, before: (() => void) | null, watch: () => void, keys: string[], jitter: number) {
  sessionStorage.setItem(keys[1], "1");
  watch();
  if (jitter) await new Promise((r) => setTimeout(r, Math.random() * jitter));
  before?.();
  sessionStorage.setItem(keys[0], String(performance.timeOrigin + performance.now()));
  run();
}

/** In the page that shows the tab: its milestones, in ms from the stored start. */
async function tabArrived(keys: string[]) {
  const at: { painted: number; dom: number } = await (window as any).benchTab;
  const start = Number(sessionStorage.getItem(keys[0]));
  sessionStorage.removeItem(keys[1]);
  const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming;
  // A document that began after the clock did came by navigation.
  const navigated = performance.timeOrigin > start;
  return {
    ms: at.painted - start,
    dom: at.dom - start,
    navigated,
    responseEnd: navigated ? performance.timeOrigin + nav.responseEnd - start : undefined,
    deliveryType: (nav as any).deliveryType as string,
    transferSize: nav.transferSize,
    workerStart: nav.workerStart,
  };
}

/** In the page: starts the clock, acts, and times the first paint inside a new match of `selector`. */
async function paint(run: (vars: Vars) => void, vars: Vars, selector: string, jitter: number) {
  const old = new Set(document.querySelectorAll(selector));
  let dom = 0;
  // Element Timing reports a paint on an element that carried the attribute before it painted.
  // It reports text on the block that holds it, which can be an ancestor of an inline match.
  const mark = () => {
    for (const match of document.querySelectorAll(selector)) {
      if (old.has(match)) continue;
      dom ||= performance.now();
      for (const e of match.querySelectorAll("*")) e.setAttribute("elementtiming", "bench");
      for (let e: Element | null = match; e; e = e.parentElement) e.setAttribute("elementtiming", "bench");
    }
  };
  const added = new MutationObserver(mark);
  added.observe(document, { childList: true, subtree: true });
  const painted = new Promise<number>((resolve) => {
    new PerformanceObserver((list, observer) => {
      const first = list.getEntries().find((e) => (e as any).identifier === "bench");
      if (!first) return;
      observer.disconnect();
      resolve((first as any).renderTime);
    }).observe({ type: "element" });
  });
  if (jitter) await new Promise((r) => setTimeout(r, Math.random() * jitter));
  const from = performance.now();
  run(vars);
  mark();
  const at = await painted;
  added.disconnect();
  return { ms: at - from, dom: dom - from };
}

/** In the page: types a query into the switcher's box as one input event, after a tap on Find if `open`. */
function typeText(query: string, open = false) {
  if (open) document.querySelector<HTMLElement>("button.find")!.click();
  const input = document.querySelector<HTMLInputElement>("dialog.switcher input")!;
  input.value = query;
  input.dispatchEvent(new Event("input"));
}

/** In the page: opens the switcher, and waits until its search workers hold every note's text and its box is empty. */
async function openSwitcher(type: (query: string, open?: boolean) => void) {
  const answer = (query: string, open = false) =>
    new Promise<void>((resolve) => {
      new MutationObserver((_, observer) => {
        if (document.querySelector("dialog.switcher ul")?.getAttribute("aria-busy") !== "false") return;
        observer.disconnect();
        resolve();
      }).observe(document, { attributes: true, subtree: true });
      type(query, open);
    });
  // No name holds this, so the workers read their text and scan all of it.
  await answer("qzqzq", true);
  await answer("");
}

/** In the page: starts the clock, acts, and times the first paint after `selector` says it is no longer busy. */
async function answered(run: (vars: Vars) => void, vars: Vars, selector: string, jitter: number) {
  let dom = 0;
  const painted = new Promise<number>((resolve) => {
    const paints = new PerformanceObserver((entries, observer) => {
      const first = entries.getEntries().find((e) => (e as any).identifier === "bench");
      if (!first) return;
      observer.disconnect();
      resolve((first as any).renderTime);
    });
    // The whole document, since the first open of the switcher makes the list.
    new MutationObserver((_, observer) => {
      const list = document.querySelector(selector);
      if (list?.getAttribute("aria-busy") !== "false") return;
      observer.disconnect();
      dom = performance.now();
      const rows = list.querySelectorAll("*");
      // Element Timing reports a paint on an element that carried the attribute before it painted.
      for (const e of rows) e.setAttribute("elementtiming", "bench");
      if (rows.length) paints.observe({ type: "element" });
      else requestAnimationFrame(() => requestAnimationFrame(() => resolve(performance.now())));
    }).observe(document, { attributes: true, subtree: true });
  });
  if (jitter) await new Promise((r) => setTimeout(r, Math.random() * jitter));
  const from = performance.now();
  run(vars);
  const at = await painted;
  return { ms: at - from, dom: dom - from };
}

/** In the page a navigation opened: its milestones, in ms from the stored start or from its own. */
async function arrived(key: string | null) {
  const fcp = await new Promise<number>((resolve) => {
    new PerformanceObserver((list, observer) => {
      const first = list.getEntriesByName("first-contentful-paint")[0];
      if (!first) return;
      observer.disconnect();
      resolve(first.startTime);
    }).observe({ type: "paint", buffered: true });
  });
  const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming;
  const from = key ? Number(sessionStorage.getItem(key)) - performance.timeOrigin : 0;
  return {
    ms: fcp - from,
    responseEnd: nav.responseEnd - from,
    dom: nav.domContentLoadedEventEnd - from,
    deliveryType: (nav as any).deliveryType as string,
    transferSize: nav.transferSize,
    workerStart: nav.workerStart,
  };
}

/** In the page: notes when the back/forward cache puts this page back on screen. */
function awaitRestore() {
  const w = window as any;
  w.benchKept = true;
  addEventListener("pageshow", (e) => {
    if (!e.persisted) return;
    const shown = performance.timeOrigin + performance.now();
    // A restored page reports no paint. The second frame after `pageshow` stands in for it.
    requestAnimationFrame(() =>
      requestAnimationFrame(() => (w.benchRestored = { shown, painted: performance.timeOrigin + performance.now() })),
    );
  });
}

const tenth = (ms: number) => Math.round(ms * 10) / 10;

async function sample(browser: Browser, profile: Profile, s: Session, scenario: Scenario, vars: Vars, jitter: number): Promise<Sample | Skip> {
  if ("first" in scenario) return first(browser, profile, s, fill(scenario.first, vars));
  if ("save" in scenario) return saved(s, scenario, vars);
  return warm(s, scenario, vars, jitter);
}

let saves = 0;

/** The version the server's manifest lists for a page. */
async function listedVersion(s: Session, url: string): Promise<string> {
  return (await (await s.page.request.get(MANIFEST)).json()).pages[url];
}

/**
 * A link for the worker's fetches: each response waits for the ones before it to cross at
 * `kbps`, and half a round trip either side. A response crosses as many bytes as gzip makes
 * of it when the server gzipped it. Every fetch gets a whole response, so no build gains
 * from the browser's HTTP cache.
 */
function slowLink(kbps: number, rtt: number) {
  let free = 0;
  return async (route: Route) => {
    if (!route.request().serviceWorker()) return route.continue();
    const asked = { ...route.request().headers() };
    delete asked["if-none-match"];
    delete asked["if-modified-since"];
    const response = await route.fetch({ headers: asked });
    const body = await response.body();
    const headers = { ...response.headers() };
    const wire = headers["content-encoding"] ? gzipSync(body).length : body.length;
    delete headers["content-encoding"];
    delete headers["content-length"];
    const now = performance.now();
    free = Math.max(free, now + rtt / 2) + (wire * 8) / kbps;
    await new Promise((r) => setTimeout(r, free + rtt / 2 - now));
    await route.fulfill({ status: response.status(), headers, body });
  };
}

async function saved(s: Session, scenario: Saved, vars: Vars): Promise<Sample> {
  await load(s, "/");
  const before = await listedVersion(s, vars.note);
  const file = join(s.vault, `${vars.file}.md`);
  const text = readFileSync(file, "utf8").replace(/\n\nbench save \d+\n$/, "");
  writeFileSync(file, `${text}\n\nbench save ${++saves}\n`);
  await expect.poll(() => listedVersion(s, vars.note), { timeout: 10_000 }).not.toBe(before);
  const version = await listedVersion(s, vars.note);
  const link = slowLink(scenario.save.kbps, scenario.save.rtt);
  await s.context.route("**/*", link);
  try {
    const [worker] = s.context.serviceWorkers();
    const holds = () =>
      worker.evaluate(
        ([url, v]) => caches.match(url).then(async (r) => !!(await r?.text())?.includes(`content="${v}"`)),
        [vars.note, version],
      );
    const from = cpu();
    const start = performance.now();
    await s.page.evaluate(() => navigator.serviceWorker.controller!.postMessage("sync"));
    while (!(await holds()));
    const ms = performance.now() - start;
    const idle = idleSince(from);
    await worker.evaluate("queue");
    await s.quiet();
    return { ms: tenth(ms), idle };
  } finally {
    await s.context.unroute("**/*", link);
  }
}

async function first(browser: Browser, profile: Profile, s: Session, path: string): Promise<Sample> {
  const context = await browser.newContext({ ...profile.device, baseURL: s.base, serviceWorkers: "block" });
  try {
    const page = await context.newPage();
    page.on("pageerror", (e) => s.errors.push(e.message));
    const cdp = await context.newCDPSession(page);
    await cdp.send("Emulation.setCPUThrottlingRate", { rate: profile.cpu });
    const from = cpu();
    await page.goto(path);
    const at = await page.evaluate(arrived, null);
    const idle = idleSince(from);
    if (at.workerStart !== 0 || at.transferSize === 0) {
      throw new Error(`${path} did not come from the network: ${JSON.stringify(at)}`);
    }
    return { ms: tenth(at.ms), idle, responseEnd: tenth(at.responseEnd), dom: tenth(at.dom) };
  } finally {
    await context.close();
  }
}

async function warm(s: Session, scenario: Warm, vars: Vars, jitter: number): Promise<Sample | Skip> {
  const { page } = s;
  const action = scenario.action;
  const until: Until | undefined = scenario.until ?? ("go" in action ? { navigation: action.go } : undefined);
  if (!until) throw new Error(`${scenario.name}: only a \`go\` action can leave \`until\` out`);
  const by = "swipe" in action ? (action.swipe === "left" ? -100 : 100) : 0;
  const run =
    "go" in action
      ? `() => location.assign(${JSON.stringify(fill(action.go, vars))})`
      : "tap" in action
        ? `() => document.querySelector(${JSON.stringify(`header > a[href="${fill(action.tap, vars)}"]`)}).click()`
        : "swipe" in action
          ? `() => (${swipe})("touchend", ${by})`
          : "type" in action
            ? `() => (${typeText})(${JSON.stringify(action.type)}, ${!!action.open})`
            : `${action.run}`;
  if (scenario.before) {
    await load(s, fill(scenario.before, vars));
    if ("restore" in until) await page.evaluate(awaitRestore);
  }
  await load(s, fill(scenario.start, vars));
  if ("type" in action) {
    // A build from before search never says its rows are complete. It skips, as Back does without its cache.
    if (!(await page.request.get("/.tachylite/search.js")).ok()) return { skip: "this build has no full-text search." };
    if (!action.open) await page.evaluate(`(${openSwitcher})(${typeText})`);
  }

  if ("answered" in until) {
    const from = cpu();
    const at: { ms: number; dom: number } = await page.evaluate(call(answered, run, vars, until.answered, jitter));
    return { ms: tenth(at.ms), idle: idleSince(from), dom: tenth(at.dom) };
  }

  if ("paint" in until) {
    const from = cpu();
    const at: { ms: number; dom: number } = await page.evaluate(call(paint, run, vars, fill(until.paint, vars), jitter));
    return { ms: tenth(at.ms), idle: idleSince(from), dom: tenth(at.dom) };
  }

  const to = fill("navigation" in until ? until.navigation : "tab" in until ? until.tab : (until as { restore: string }).restore, vars);
  const synced = nextSync(s);
  const from = cpu();

  if ("tab" in until) {
    const keys = [KEY, SWITCHING];
    const before = by ? `() => (${swipe})("touchstart", ${-by})` : "null";
    // A switch in place changes the URL too, and commits at once.
    const there = ahead(page.waitForURL((url) => url.pathname === to, { waitUntil: "commit" }));
    await page.evaluate(`(${actOnTab})(${run}, ${before}, ${watchMain}, ${JSON.stringify(keys)}, ${jitter})`).catch((e: Error) => {
      if (!e.message.includes("context was destroyed")) throw e;
    });
    await there;
    const at = await page.evaluate(tabArrived, keys);
    const idle = idleSince(from);
    if (at.navigated && (at.deliveryType !== "cache-storage" || at.transferSize !== 0 || at.workerStart === 0)) {
      throw new Error(`${scenario.name}: the worker's cache did not answer for ${to}: ${JSON.stringify(at)}`);
    }
    await rest(s, synced);
    return { ms: tenth(at.ms), idle, responseEnd: at.responseEnd === undefined ? undefined : tenth(at.responseEnd), dom: tenth(at.dom) };
  }

  const there = ahead(page.waitForURL((url) => url.pathname === to, { waitUntil: "restore" in until ? "commit" : "load" }));
  // The navigation can take the page away before the call returns.
  await page.evaluate(call(act, run, vars, KEY, jitter)).catch((e: Error) => {
    if (!e.message.includes("context was destroyed")) throw e;
  });
  await there;

  if ("restore" in until) {
    if (!(await page.evaluate(() => (window as any).benchKept === true))) {
      await rest(s, synced);
      return { skip: 'the browser did not restore the page from its back/forward cache. See "The Back scenario" in browser/bench/README.md.' };
    }
    const handle = await page.waitForFunction(() => (window as any).benchRestored);
    const at: { shown: number; painted: number } = await handle.jsonValue();
    const start = Number(await page.evaluate((key) => sessionStorage.getItem(key), KEY));
    const idle = idleSince(from);
    await rest(s, synced);
    return { ms: tenth(at.painted - start), idle, dom: tenth(at.shown - start) };
  }

  const at = await page.evaluate(arrived, KEY);
  const idle = idleSince(from);
  if (at.deliveryType !== "cache-storage" || at.transferSize !== 0 || at.workerStart === 0) {
    throw new Error(`${scenario.name}: the worker's cache did not answer for ${to}: ${JSON.stringify(at)}`);
  }
  await rest(s, synced);
  return { ms: tenth(at.ms), idle, responseEnd: tenth(at.responseEnd), dom: tenth(at.dom) };
}
