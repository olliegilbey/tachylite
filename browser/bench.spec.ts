import { cpSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { bench } from "./bench/bench";
import { report } from "./bench/report";
import { expect, run, test } from "./fixture";

// The bench is not part of the gate, so this run keeps it working. A bench asserts no time.
test("the client bench times a navigation, a first load, an in-page action, Back and a tab switch", async ({ browser }, info) => {
  test.skip(info.project.name !== "phone", "the bench brings its own profiles");
  test.setTimeout(120_000);
  const names = ["home-to-note", "note-first-load", "switcher-query", "back-to-home", "swipe-home-to-today", "tap-today-to-recent"];
  // A daily note and its settings give the fixture vault a Today tab, as the bench's own vault has.
  const vault = join(run, "bench-vault");
  cpSync(join(__dirname, "..", "tests", "fixtures", "vault"), vault, { recursive: true });
  mkdirSync(join(vault, "days"));
  writeFileSync(join(vault, "days", "2020-01-01.md"), "# A day\n\nplans for the day\n");
  writeFileSync(join(vault, "tachylite.toml"), 'daily = { folder = "days" }\n');
  const result = await bench(browser, {
    dir: join(run, "bench"),
    vault,
    binaries: [{ label: "suite" }],
    scenarios: names,
    profiles: ["phone-4x"],
    samples: 1,
    warmup: 0,
    minIdle: 0,
    // One sample has no frame grid to average out, and the suite waits on no timer.
    jitter: 0,
  });
  expect(result.cells.map((c) => [c.scenario, c.profile, c.binary])).toEqual(
    names.map((name) => [name, "phone-4x", "suite"]),
  );
  const table = report(result);
  for (const cell of result.cells) {
    // Back needs the back/forward cache. A browser without one skips it, and says why.
    if (cell.skipped) {
      expect(cell.scenario).toBe("back-to-home");
      expect(table).toContain(`Skipped back-to-home on phone-4x, \`suite\`: ${cell.skipped}`);
      continue;
    }
    expect(cell.samples.length, cell.scenario).toBe(1);
    expect(cell.samples[0].ms, cell.scenario).toBeGreaterThan(0);
    expect(table).toContain(`| ${cell.scenario} | phone-4x | suite |`);
  }
  expect(result.cells[0].samples[0].responseEnd).toBeGreaterThan(0);
});

test("the bench's table leaves out the samples taken under host load", () => {
  const table = report({
    date: "2026-01-01",
    chromium: "1.0",
    samples: 3,
    minIdle: 0.3,
    load: [1, 2],
    binaries: [{ label: "a", client: { "live.js": 10, "sw.js": 20 } }],
    cells: [
      {
        scenario: "one",
        profile: "phone-1x",
        binary: "a",
        samples: [
          { ms: 10, idle: 0.9, responseEnd: 4, dom: 8 },
          { ms: 20, idle: 0.9, responseEnd: 6, dom: 9 },
          { ms: 30, idle: 0.3, responseEnd: 8, dom: 10 },
          { ms: 900, idle: 0.29, responseEnd: 800, dom: 850 },
        ],
      },
      { scenario: "two", profile: "phone-1x", binary: "a", samples: [{ ms: 5, idle: 0.1 }] },
      { scenario: "three", profile: "phone-1x", binary: "a", samples: [], skipped: "no cache." },
    ],
  });
  expect(table).toContain("`a`: 30 B of client files, gzipped (live.js 10, sw.js 20).");
  expect(table).toContain("| one | phone-1x | a | 20.0 | 28.0 | 6.0 | 9.0 | 3 | 1 |");
  expect(table).toContain("| two | phone-1x | a | – | – | – | – | 0 | 1 |");
  expect(table).not.toContain("| three |");
  expect(table).toContain("Skipped three on phone-1x, `a`: no cache.");
});
