# The client bench

The bench times what a reader does in a client: a tap on a link, a swipe between tabs, a first load, a query in the quick switcher. Use it to choose between two ways to build a client feature, by measurement.

It serves the budget vault from a release build, with daily settings added, so the vault has a Today tab. It drives headless Chromium at phone size, at full CPU speed and at a quarter of it. It explores, and it gates nothing: a budget test in the browser suite does the gating.

## Run it

```sh
make bench
```

This builds the release binary, writes the budget vault, and starts a server on a free port. It runs every scenario under every profile, then stops the server. Each cell takes two samples to warm up, and throws them away. It prints a Markdown table, ready for a PR body. It writes every sample as JSON to `browser/bench/results/`, which git ignores.

Variables narrow a run or widen it:

| Variable | Default | Meaning |
|---|---|---|
| `BENCH_SCENARIOS` | every scenario | Scenario names, with commas between. |
| `BENCH_PROFILES` | every profile | Profile names, with commas between: `phone-1x`, `phone-4x`, `phone-4x-2core`. |
| `BENCH_SAMPLES` | 30 | The samples to keep in each cell. |
| `BENCH_MIN_IDLE` | 0.3 | A sample taken with a smaller share of the host's CPU idle is dropped. |
| `TACHYLITE_BIN` | this checkout's build | The binary to serve, or several. See "Compare two binaries". |
| `BENCH_CHANNEL` | the headless shell | `chromium` runs full Chromium. See "The Back scenario". |

One scenario alone runs in seconds:

```sh
make bench BENCH_SCENARIOS=home-to-recent BENCH_PROFILES=phone-4x BENCH_SAMPLES=10
```

## Read the table

A row is one cell: a scenario, under a profile, against a binary. Every time is in ms, from the action.

- **Median** and **p90**: the time to the milestone that ends the scenario's clock.
  - A navigation ends at the new page's first contentful paint.
  - An in-page action ends at the first paint inside the element it adds.
  - A tab switch ends at the first paint of the new tab's content.
  - A search ends at the first paint of the rows that answer the query, or at the second frame after an answer with no row.
  - Back ends at the second frame after the browser restores the page.
- **Response end**: the median `responseEnd` of a navigation.
- **DOM ready**: the median time at which the DOM held the result. For a navigation it is `domContentLoadedEventEnd`. For an in-page action it is the moment the element arrived. For a tab switch it is the moment the tab's content arrived. For a search it is the moment the answer's rows arrived. For Back it is `pageshow`.
- **n**: the samples kept. **Dropped**: the samples taken under host load, and left out.

The bench checks where each navigation's response came from. A warm navigation must come from the service worker's cache, and a first load from the network. A sample that breaks this fails the run.

The line above the table gives the date, the Chromium version and the host's load average. The list under it gives each binary's client files as the server sends them, gzipped.

## Host load

Other work on the host can slow a sample three to eight times. So the bench reads the host's CPU counters around each sample. It drops a sample taken with under 30% of the CPU idle, and takes another. It gives up on a cell after four tries for each sample asked.

A row with a small **n** was measured on a busy host. Run it again when the host is quiet. The bench's own work counts as load, so on a host with few cores, lower `BENCH_MIN_IDLE`.

## Compare two binaries

Build each candidate, then name them all in one run:

```sh
make bench TACHYLITE_BIN=main=/path/to/main/tachylite,swap=target/release/tachylite
```

Each entry is `label=path`. A path is absolute, or starts at the repo root. To build `main` beside a branch, use a `git worktree`.

The bench serves a copy of the vault from each binary. It takes one sample from each in turn, so a change in host load reaches every binary alike. The table carries one row for each label. Two builds of the same commit should agree to within about 2 ms: try it before you trust a smaller difference.

## Add a scenario

Add an entry to `scenarios` in `scenarios.ts`. A scenario is data:

```ts
{
  name: "switcher-query",
  start: "/",
  action: { run: ({ query }) => { /* tap Find, type the query */ } },
  until: { paint: "dialog.switcher li" },
}
```

- `name`: what `BENCH_SCENARIOS` and the table call it.
- `start`: the page the action starts on. The bench loads it before each sample and waits for it to come to rest.
- `before`: a page opened before `start`, so Back has somewhere to go.
- `action`: what the reader does.
  - `{ go: path }` is a plain navigation, as a tapped link makes.
  - `{ run: fn }` is any script: a tap, typed text. It runs alone in the page, so it sees no variable from outside itself.
  - `{ tap: path }` is a tap on the header's tab for that path.
  - `{ type: query }` types a query into the open quick switcher as one input event. Before the clock runs, the bench opens the switcher and waits until its search workers hold every note's text. With `open: true` the switcher starts closed, and the action taps Find first: a cold open.
  - `{ swipe: "left" }` or `{ swipe: "right" }` is one finger swiped across the middle of the page, as touch events. A swipe to the left opens the tab to the right. The clock starts at the release.
- `until`: the milestone that ends the clock.
  - `{ navigation: path }`: the first contentful paint of a new document at that path. After `go`, leave `until` out.
  - `{ paint: selector }`: the first paint of text or an image in an element the action added. An element that was there before the action does not count, so a change to one never ends the clock.
  - `{ restore: path }`: the page at that path, back from the back/forward cache.
  - `{ answered: selector }`: the first paint of a row in the list at that selector, once the list says `aria-busy="false"`: every search worker has answered. A row drawn before the answer does not end the clock.
  - `{ tab: path }`: the first paint of text or an image in the `<main>` of the tab page at that path. The content may come with a new document or be put in place by the client. Both end the clock the same way, so two ways to switch tabs compare fairly.

A scenario with `first: path` in place of these is a first load. It opens the path in a new browser context, with no service worker and nothing cached.

Every other scenario is warm: it measures the cached path. Before the clock runs, the service worker controls the page and a sync has stored every listed page.

A scenario names no note of its own, since the vault is generated. A path or a selector can hold `{note}`, the path of a note from the middle of the switcher list. `{query}` is the first three letters of that note's title. A `run` function gets both as its argument.

Navigate by path. A scenario that taps a link by a selector breaks when the header changes.

The `swipe-*` and `tap-*` scenarios time a switch between each pair of neighbouring tabs, each way. `tap-home-to-recent` and `tap-recent-to-home` time a tap across the Today tab, and `tap-home-to-tasks` a tap across Today and Recent to the board. They use `tap`, `swipe` and `tab`.

The `search-*` scenarios time one query each in the quick switcher: one letter, a common word, a rare word, two words, two rare words, no match, and a near miss. `search-cold-open` times a rare word from a closed switcher. They use `type` and `answered`, and they skip a binary that has no search.

A profile is a device, a CPU rate and, with `cores`, the core count the page sees. Add one to `profiles` in the same file. `phone-4x-2core` gives the page two cores, so the switcher starts one search worker. See "Limits".

## The Back scenario

`back-to-home` needs the back/forward cache. Playwright's default browser, the headless shell, has none, so the scenario skips itself and prints why. Full Chromium has one. Install it once, then name it:

```sh
cd browser && npx playwright install chromium
make bench BENCH_CHANNEL=chromium BENCH_SCENARIOS=back-to-home
```

Full Chromium and the headless shell give different numbers for the same scenario. Compare runs from one browser only.

## Limits

- **CPU throttling reaches the page only.** The service worker and the browser process run at full speed. So **Response end** is optimistic for a phone.
- **A search worker runs at full speed too.** Chromium refuses the throttle on a worker. So a `search-*` time at 4x is the page's share at a quarter speed, plus the scan at full speed. `phone-4x-2core` makes up for it: one worker scans every note alone, which takes as long as four workers on cores four times slower. That is the pool a phone with five cores or more gets. For another pool, redo the sum: the page's share, plus the scan at full speed times four, over the workers. The budget test in `browser/search.spec.ts` logs both parts.
- **The network is localhost.** A first load pays no latency and no bandwidth limit. No profile limits the network yet. CDP's `Network.emulateNetworkConditions` can, for a question that needs it.
- **The clock starts in the page's script.** The browser's own handling of a touch, before the script sees it, is not counted. A swipe is two touch events made in the page, so the browser's pan never runs.
- **A sleeping service worker is never timed.** Every warm sample starts with the worker awake. On a phone the browser stops an idle worker, and its next fetch waits for it to start.
- **Chrome's prerender cannot be timed.** Chromium refuses every prerender while a DevTools client such as Playwright drives it, in the headless shell and in full Chromium alike.
- **The clock starts at a random point in a frame.** A paint lands on a frame, 16.7 ms apart. A real tap comes at any point between two. So each sample waits a random time of up to 16 ms before it starts, and the frame grid averages out over the samples. The smoke run in the browser suite takes one sample and skips the wait, so the suite waits on no timer.
- **Paint times come in steps of about 4 ms.** Chromium rounds them. A median can sit on a step, with the 90th percentile on the same one.
- **A restored page reports no paint.** The second frame after `pageshow` stands in for it, so a Back time is good to about one frame.
- **Headless Chromium on a desktop CPU is no phone.** Trust the bench for the order of two candidates and the size of the gap, not for the time a reader will see.
- **The waits lean on two client internals.** One is the `queue` of jobs in `src/sw.js`. The other is the sync a page asks for when its event stream opens. A change to either breaks the bench, and the smoke test says so.

## Keeping it working

`browser/bench.spec.ts` runs one sample of six scenarios on a copy of the fixture vault, in the browser suite. So `make check` fails when the bench stops running. It asserts no time.
