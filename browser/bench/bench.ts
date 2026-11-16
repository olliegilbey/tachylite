import type { Browser, BrowserContext, Page, Request } from "@playwright/test";
import { cpSync } from "node:fs";
import { get } from "node:http";
import { cpus, loadavg } from "node:os";
import { join } from "node:path";
import { serve, stop, type Server } from "../server";
import type { Cell, Result, Sample } from "./report";
import { profiles, scenarios, type Profile, type Scenario, type Until, type Vars, type Warm } from "./scenarios";

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
  log?: (line: string) => void;
};

const MANIFEST = "/.tachylite/manifest.json";
const CLIENT = ["live.js", "sw.js", "style.css"];
/** Where the clock's start crosses a navigation. */
const KEY = "tachylite-bench";

type Session = {
  label: string;
  base: string;
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
  const load = loadavg()[0];
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
        for (const { label, server } of servers) sessions.push(await open(browser, profile, label, server.base));
        for (const scenario of picked) {
          const row = sessions.map((session) => ({
            session,
            cell: { scenario: scenario.name, profile: profile.name, binary: session.label, samples: [] } as Cell,
          }));
          const quiet = (c: Cell) => c.samples.filter((s) => s.idle >= o.minIdle).length;
          const done = (c: Cell) => !!c.skipped || quiet(c) >= o.samples;
          // One sample from each binary a round, so a change in host load reaches them alike.
          for (let round = -o.warmup; round < o.samples * 4 && !row.every((r) => done(r.cell)); round++) {
            for (const { session, cell } of row) {
              if (done(cell)) continue;
              const what = `${cell.scenario} on ${cell.profile}, ${cell.binary}`;
              const got = await within(sample(browser, profile, session, scenario, vars), what);
              if (session.errors.length) throw new Error(`${what}: the page threw: ${session.errors.join("; ")}`);
              if ("skip" in got) cell.skipped = got.skip;
              else if (round >= 0) cell.samples.push(got);
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
      load: [load, loadavg()[0]],
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
  // The switcher list: each note as its title, its URL, then its path and aliases.
  const notes: [string, string][] = await (await fetch(`${base}/.tachylite/notes.json`)).json();
  const [title, note] = notes[Math.floor(notes.length / 2)];
  return { note, query: title.slice(0, 3).toLowerCase() };
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

/** A sample that hangs names its cell and fails the run, rather than holding it for ever. */
function within<T>(sample: Promise<T>, what: string): Promise<T> {
  let timer: NodeJS.Timeout;
  const late = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what}: no result within 60 s`)), 60_000);
  });
  return Promise.race([ahead(sample), late]).finally(() => clearTimeout(timer));
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
  return now.total > from.total ? (now.idle - from.idle) / (now.total - from.total) : 1;
}

/** A page the worker controls, with every listed page in its cache, at the profile's CPU speed. */
async function open(browser: Browser, profile: Profile, label: string, base: string): Promise<Session> {
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

  const page = await context.newPage();
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  const { pages } = await (await page.request.get(MANIFEST)).json();
  await page.waitForFunction(
    async ([manifest, want]) => {
      const have = (await (await caches.match(manifest))?.json()) ?? {};
      const urls = Object.keys(want);
      return urls.length === Object.keys(have).length && urls.every((url) => have[url] === want[url]);
    },
    [MANIFEST, pages] as const,
    { polling: 100, timeout: 120_000 },
  );
  const cdp = await context.newCDPSession(page);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: profile.cpu });
  return { label, base, context, page, quiet, errors };
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
async function act(run: (vars: Vars) => void, vars: Vars, key: string) {
  // A real tap lands anywhere in a frame, so the clock starts at a random point in one.
  await new Promise((r) => setTimeout(r, Math.random() * 16));
  sessionStorage.setItem(key, String(performance.timeOrigin + performance.now()));
  run(vars);
}

/** In the page: starts the clock, acts, and times the first paint inside a new match of `selector`. */
async function paint(run: (vars: Vars) => void, vars: Vars, selector: string) {
  const old = new Set(document.querySelectorAll(selector));
  let dom = 0;
  // Element Timing reports the paint of an element that carries the attribute before it paints.
  const mark = () => {
    for (const match of document.querySelectorAll(selector)) {
      if (old.has(match)) continue;
      dom ||= performance.now();
      for (const e of [match, ...match.querySelectorAll("*")]) e.setAttribute("elementtiming", "bench");
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
  await new Promise((r) => setTimeout(r, Math.random() * 16));
  const from = performance.now();
  run(vars);
  mark();
  const at = await painted;
  added.disconnect();
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
  // A prerendered page paints before it is shown, and counts from the moment it is shown.
  const shown = (nav as any).activationStart ?? 0;
  return {
    ms: Math.max(fcp, shown) - from,
    responseEnd: Math.max(nav.responseEnd - from, 0),
    dom: Math.max(nav.domContentLoadedEventEnd - from, 0),
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

const round = (ms: number) => Math.round(ms * 10) / 10;

async function sample(browser: Browser, profile: Profile, s: Session, scenario: Scenario, vars: Vars): Promise<Sample | Skip> {
  if ("first" in scenario) return first(browser, profile, s, fill(scenario.first, vars));
  return warm(s, scenario, vars);
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
    return { ms: round(at.ms), idle: round(idle), responseEnd: round(at.responseEnd), dom: round(at.dom) };
  } finally {
    await context.close();
  }
}

async function warm(s: Session, scenario: Warm, vars: Vars): Promise<Sample | Skip> {
  const { page } = s;
  const action = scenario.action;
  const until: Until = scenario.until ?? { navigation: (action as { go: string }).go };
  const run = "go" in action ? `() => location.assign(${JSON.stringify(fill(action.go, vars))})` : `${action.run}`;
  if (scenario.before) {
    await load(s, fill(scenario.before, vars));
    if ("restore" in until) await page.evaluate(awaitRestore);
  }
  await load(s, fill(scenario.start, vars));

  if ("paint" in until) {
    const from = cpu();
    const at: { ms: number; dom: number } = await page.evaluate(call(paint, run, vars, fill(until.paint, vars)));
    return { ms: round(at.ms), idle: round(idleSince(from)), dom: round(at.dom) };
  }

  const to = fill("navigation" in until ? until.navigation : until.restore, vars);
  const synced = nextSync(s);
  const from = cpu();
  const there = ahead(page.waitForURL((url) => url.pathname === to, { waitUntil: "restore" in until ? "commit" : "load" }));
  // The navigation can take the page away before the call returns.
  await page.evaluate(call(act, run, vars, KEY)).catch((e: Error) => {
    if (!/context was destroyed|navigation/i.test(e.message)) throw e;
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
    return { ms: round(at.painted - start), idle: round(idle), dom: round(at.shown - start) };
  }

  const at = await page.evaluate(arrived, KEY);
  const idle = idleSince(from);
  if (at.deliveryType !== "cache-storage" || at.transferSize !== 0 || at.workerStart === 0) {
    throw new Error(`${scenario.name}: the worker's cache did not answer for ${to}: ${JSON.stringify(at)}`);
  }
  await rest(s, synced);
  return { ms: round(at.ms), idle: round(idle), responseEnd: round(at.responseEnd), dom: round(at.dom) };
}
