import { expect, test } from "./fixture";

// Sums what the browser fetched, so a new script or stylesheet in the shell counts too.
// Images are assets, outside the page-weight budget.
test("a typical note page weighs at most 30 KB over the wire", async ({ page }) => {
  await page.goto("/notes/links");
  const sizes = await page.evaluate(() =>
    [...performance.getEntriesByType("navigation"), ...performance.getEntriesByType("resource")]
      .filter((e) => (e as PerformanceResourceTiming).initiatorType !== "img")
      .map((e) => [e.name, (e as PerformanceResourceTiming).transferSize] as const),
  );
  for (const [name, bytes] of sizes) expect(bytes, `${name} came from a cache`).toBeGreaterThan(0);
  expect(sizes.map(([name]) => new URL(name).pathname)).toEqual(
    expect.arrayContaining(["/notes/links", "/.tachylite/style.css", "/.tachylite/live.js"]),
  );
  const total = sizes.reduce((sum, [, bytes]) => sum + bytes, 0);
  console.log(`typical page over the wire: ${total} B (limit ${30 * 1024} B)`);
  expect(total).toBeLessThanOrEqual(30 * 1024);
});
