import { expect, test as base } from "@playwright/test";

/** The run's directory, and the shared server's address, from `global-setup.ts`. */
// Empty when a tool loads the specs without running the global setup, as `--list` does.
export const run = process.env.TACHYLITE_RUN ?? "";

/** What a browser logs when a test stops the server: a cut stream and refused retries. */
export const lost = /net::ERR_(INCOMPLETE_CHUNKED_ENCODING|CONNECTION_REFUSED)/;

export const test = base.extend<{ allowedErrors: RegExp[] }>({
  baseURL: async ({}, use) => use(process.env.TACHYLITE_BASE),
  // Every test fails on a console error or an uncaught exception, CSP violations included.
  // A test that expects some pushes a pattern for them.
  allowedErrors: [
    async ({ page }, use) => {
      const errors: string[] = [];
      page.on("console", (m) => {
        if (m.type() === "error") errors.push(m.text());
      });
      page.on("pageerror", (e) => errors.push(e.message));
      const allowed: RegExp[] = [];
      await use(allowed);
      expect(errors.filter((e) => !allowed.some((a) => a.test(e))), "console errors").toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
