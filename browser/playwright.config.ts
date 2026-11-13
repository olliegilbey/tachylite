import { defineConfig, devices } from "@playwright/test";

// Workers inherit this, so every process of one run writes under the same results directory.
process.env.TACHYLITE_RUN_ID ??= `${process.pid}`;

export default defineConfig({
  testDir: ".",
  outputDir: `test-results/${process.env.TACHYLITE_RUN_ID}`,
  workers: 1,
  forbidOnly: !!process.env.CI,
  reporter: "list",
  globalSetup: "./global-setup.ts",
  use: {
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  projects: [
    { name: "phone", use: { ...devices["Pixel 7"] } },
    { name: "desktop", use: { ...devices["Desktop Chrome"] } },
  ],
});
