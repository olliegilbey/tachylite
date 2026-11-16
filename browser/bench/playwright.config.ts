import { defineConfig } from "@playwright/test";

// The bench is one long test with no time limit. `make bench` runs it. It is not part of the suite.
export default defineConfig({
  testDir: ".",
  testMatch: "run.ts",
  outputDir: "../test-results/bench",
  workers: 1,
  timeout: 0,
  reporter: "list",
  use: {
    // `chromium` is full Chromium, which has a back/forward cache. The default headless shell has none.
    channel: process.env.BENCH_CHANNEL,
    launchOptions: { ignoreDefaultArgs: ["--disable-back-forward-cache"] },
  },
});
