import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  workers: 1,
  forbidOnly: !!process.env.CI,
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:7431",
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  projects: [
    { name: "phone", use: { ...devices["Pixel 7"] } },
    { name: "desktop", use: { ...devices["Desktop Chrome"] } },
  ],
  webServer: {
    command: "./serve.sh",
    url: "http://127.0.0.1:7431/welcome",
    reuseExistingServer: false,
    stderr: "pipe",
  },
});
