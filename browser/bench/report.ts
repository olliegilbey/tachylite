/** One timed run of a scenario. Times are in ms from the action. */
export type Sample = {
  /** To the milestone that ends the clock. */
  ms: number;
  /** The share of the host's CPU that sat idle while the clock ran, from 0 to 1. */
  idle: number;
  /** A navigation's `responseEnd`. */
  responseEnd?: number;
  /** When the DOM held the result: `domContentLoadedEventEnd`, the added element, or `pageshow`. */
  dom?: number;
};

export type Cell = {
  scenario: string;
  profile: string;
  binary: string;
  samples: Sample[];
  /** Why the scenario could not run here. */
  skipped?: string;
};

export type Result = {
  date: string;
  chromium: string;
  /** The quiet samples asked for in each cell. */
  samples: number;
  /** A sample with a smaller idle share is dropped. */
  minIdle: number;
  /** The host's one-minute load average at the start and at the end. */
  load: [number, number];
  /** Each binary, with the gzipped bytes of its client files. */
  binaries: { label: string; client: Record<string, number> }[];
  cells: Cell[];
};

/** The quantile `p` of `values`, between the two nearest ranks. */
export function quantile(values: number[], p: number): number {
  const sorted = [...values].sort((a, b) => a - b);
  const at = (sorted.length - 1) * p;
  const low = Math.floor(at);
  const high = Math.ceil(at);
  return sorted[low] + (sorted[high] - sorted[low]) * (at - low);
}

const ms = (values: (number | undefined)[], p: number) => {
  const known = values.filter((v) => v !== undefined);
  return known.length ? quantile(known, p).toFixed(1) : "–";
};

/** The result as Markdown: the run's conditions, then one table row for each cell. */
export function report(r: Result): string {
  const lines = [
    `Client bench, ${r.date}, Chromium ${r.chromium}. ` +
      `Each cell asked for ${r.samples} samples and dropped any taken with under ` +
      `${Math.round(r.minIdle * 100)}% of the CPU idle. ` +
      `Load average: ${r.load[0].toFixed(1)} at the start, ${r.load[1].toFixed(1)} at the end.`,
    "",
  ];
  for (const { label, client } of r.binaries) {
    const files = Object.entries(client);
    const total = files.reduce((sum, [, bytes]) => sum + bytes, 0);
    const each = files.map(([name, bytes]) => `${name} ${bytes}`).join(", ");
    lines.push(`- \`${label}\`: ${total} B of client files, gzipped (${each}).`);
  }
  lines.push(
    "",
    "| Scenario | Profile | Binary | Median | p90 | Response end | DOM ready | n | Dropped |",
    "|---|---|---|---|---|---|---|---|---|",
  );
  for (const cell of r.cells.filter((c) => !c.skipped)) {
    const quiet = cell.samples.filter((s) => s.idle >= r.minIdle);
    lines.push(
      `| ${[
        cell.scenario,
        cell.profile,
        cell.binary,
        ms(quiet.map((s) => s.ms), 0.5),
        ms(quiet.map((s) => s.ms), 0.9),
        ms(quiet.map((s) => s.responseEnd), 0.5),
        ms(quiet.map((s) => s.dom), 0.5),
        quiet.length,
        cell.samples.length - quiet.length,
      ].join(" | ")} |`,
    );
  }
  const skipped = r.cells.filter((c) => c.skipped);
  if (skipped.length) lines.push("");
  for (const c of skipped) lines.push(`Skipped ${c.scenario} on ${c.profile}, \`${c.binary}\`: ${c.skipped}`);
  return lines.join("\n");
}
