import { test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { bench } from "./bench";
import { report } from "./report";

// `make bench` runs this file. `README.md` beside it lists the variables.
const repo = join(__dirname, "..", "..");
const scratch = join(__dirname, "..", ".tmp");
const results = join(__dirname, "results");

const list = (name: string) => process.env[name]?.split(",").filter(Boolean);

function number(name: string, fallback: number, least: number): number {
  const value = Number(process.env[name] || fallback);
  if (!(value >= least)) throw new Error(`${name} takes a number of at least ${least}: ${process.env[name]}`);
  return value;
}

/** This checkout's commit, to label its own build. */
function commit(): string {
  try {
    const git = (...args: string[]) => execFileSync("git", ["-C", repo, ...args], { encoding: "utf8" }).trim();
    return git("rev-parse", "--short", "HEAD") + (git("status", "--porcelain", "--untracked-files=no") ? "-dirty" : "");
  } catch {
    return "local";
  }
}

/** `TACHYLITE_BIN`: one path, or `label=path` pairs. A path is absolute, or starts at the repo root. */
function binaries(): { label: string; path?: string }[] {
  const named = list("TACHYLITE_BIN");
  if (!named?.length) return [{ label: commit() }];
  const all = named.map((entry) => {
    const [label, path] = entry.includes("=") ? entry.split(/=(.*)/) : ["bin", entry];
    if (!/^[\w.-]+$/.test(label)) throw new Error(`a label takes letters, digits, ".", "_" and "-": ${label}`);
    return { label, path: resolve(repo, path) };
  });
  if (new Set(all.map((b) => b.label)).size < all.length) throw new Error("give each binary its own label: label=path");
  return all;
}

test("client bench", async ({ browser }) => {
  mkdirSync(scratch, { recursive: true });
  const dir = mkdtempSync(join(scratch, "bench-"));
  try {
    const vault = join(dir, "vault");
    execFileSync(join(repo, "target", "release", "examples", "budget_vault"), [vault], { stdio: ["ignore", "ignore", "pipe"] });
    // The budget vault has daily notes and no settings for them. With these it has a Today tab.
    writeFileSync(join(vault, "tachylite.toml"), 'daily = { folder = "daily" }\n');
    const result = await bench(browser, {
      dir: join(dir, "serve"),
      vault,
      binaries: binaries(),
      scenarios: list("BENCH_SCENARIOS"),
      profiles: list("BENCH_PROFILES"),
      samples: Math.round(number("BENCH_SAMPLES", 30, 1)),
      warmup: 2,
      minIdle: number("BENCH_MIN_IDLE", 0.3, 0),
      jitter: 16,
      log: (line) => console.error(line),
    });
    mkdirSync(results, { recursive: true });
    const file = `${new Date().toISOString().replace(/[:.]/g, "-")}.json`;
    writeFileSync(join(results, file), JSON.stringify(result, null, 1));
    console.log(`\n${report(result)}\n\nRaw samples: browser/bench/results/${file}`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
