import { phone } from "../playwright.config";

/**
 * A named device and CPU speed. `cpu` is the CDP throttling rate: 4 runs the page four times
 * slower. `cores` is the core count the page reports, where the host's own would mislead.
 */
export type Profile = { name: string; device: typeof phone; cpu: number; cores?: number };

export const profiles: Profile[] = [
  { name: "phone-1x", device: phone, cpu: 1 },
  { name: "phone-4x", device: phone, cpu: 4 },
  // The throttle never slows a worker. With two cores the switcher starts one search worker,
  // and one worker at full speed scans as long as four on cores four times slower.
  { name: "phone-4x-2core", device: phone, cpu: 4, cores: 2 },
];

/**
 * Facts read from the vault being served, so a scenario names no note of its own. A path
 * or a selector takes them as `{note}` and `{query}`. A `run` function takes them as its argument.
 */
export type Vars = {
  /** The path of a note from the middle of the switcher list. */
  note: string;
  /** The first letters of that note's title, as a reader would type them. */
  query: string;
  /** That note's path in the vault, without `.md`. */
  file: string;
};

/** What the reader does. It runs in the page, and the clock starts as it is called. */
export type Action =
  /** A plain navigation, as a tapped link makes. */
  | { go: string }
  /** Any script: a tap, typed text. It runs alone in the page, so it sees no outer variable. */
  | { run: (vars: Vars) => void }
  /** A tap on the header's tab for this path. */
  | { tap: string }
  /** One finger swiped across the middle of the page. To the left opens the tab to the right. The clock starts at the release. */
  | { swipe: "left" | "right" }
  /**
   * Text typed into the quick switcher's box, as one input event. The switcher is open by
   * then and its search workers hold every note's text. With `open`, it starts closed, and
   * the tap on Find is the first thing the clock times.
   */
  | { type: string; open?: boolean };

/** The milestone that ends the clock. */
export type Until =
  /** The first contentful paint of a new document at this path. */
  | { navigation: string }
  /** The first paint of text or an image in an element the action added, matched by this selector. */
  | { paint: string }
  /** The page at this path, back on screen from the back/forward cache. */
  | { restore: string }
  /** The first paint of the tab page at this path, whether a new document brings it or the client puts it in place. */
  | { tab: string }
  /**
   * The first paint after the element at this selector says `aria-busy="false"`: the
   * switcher's list, once every search worker has answered. With no row to paint, the
   * second frame after it.
   */
  | { answered: string };

/**
 * A warm scenario runs the cached path. Before each sample the service worker controls
 * the page, a sync has stored every listed page, and `start` is loaded and at rest.
 */
export type Warm = {
  name: string;
  /** The page the action starts on. */
  start: string;
  /** A page opened before `start`, so Back has somewhere to go. */
  before?: string;
} & (
  | { action: Extract<Action, { go: string }>; /** Left out, it is the navigation to that path. */ until?: Until }
  | { action: Exclude<Action, { go: string }>; until: Until }
);

/**
 * A first load opens `first` in a new browser context: no service worker, nothing cached.
 * Its clock runs from the start of the navigation to the first contentful paint.
 */
export type First = { name: string; first: string };

/**
 * A sync after one saved note, on a slow link. The bench saves `{note}` and asks the worker
 * for a sync. The clock runs from the ask until the worker's cache holds the note's new
 * version. The link carries the worker's fetches one after another, at `kbps` kbit/s, and
 * each waits `rtt` ms more.
 */
export type Saved = { name: string; save: { kbps: number; rtt: number } };

export type Scenario = Warm | First | Saved;

const HOME = "/";
const TODAY = "/.tachylite/today";
const RECENT = "/.tachylite/recent";
const TASKS = "/.tachylite/tasks";

/** A switch from one tab page to the one beside it, by swipe and by tap. */
function switches(from: string, start: string, to: string, tab: string, swipe: "left" | "right"): Scenario[] {
  return [
    { name: `swipe-${from}-to-${to}`, start, action: { swipe }, until: { tab } },
    { name: `tap-${from}-to-${to}`, start, action: { tap: tab }, until: { tab } },
  ];
}

const LIST = "dialog.switcher ul";

/**
 * One query typed into the open, empty switcher, timed until the rows that answer it paint.
 * The words are the budget vault's, from its seeded generator. On another vault they match
 * nothing, and the scenario times a scan of every note.
 */
function search(name: string, query: string): Scenario {
  return { name: `search-${name}`, start: "/", action: { type: query }, until: { answered: LIST } };
}

export const scenarios: Scenario[] = [
  { name: "home-to-recent", start: "/", action: { go: "/.tachylite/recent" } },
  { name: "recent-to-home", start: "/.tachylite/recent", action: { go: "/" } },
  { name: "home-to-note", start: "/", action: { go: "{note}" } },
  { name: "note-first-load", first: "{note}" },
  { name: "sync-after-save-400k", save: { kbps: 400, rtt: 100 } },
  {
    name: "switcher-query",
    start: "/",
    action: {
      run: ({ query }) => {
        document.querySelector<HTMLElement>("button.find")!.click();
        const input = document.querySelector<HTMLInputElement>("dialog.switcher input")!;
        input.value = query;
        input.dispatchEvent(new Event("input"));
      },
    },
    until: { paint: "dialog.switcher li" },
  },
  {
    name: "back-to-home",
    before: "/",
    start: "{note}",
    action: { run: () => history.back() },
    until: { restore: "/" },
  },
  search("one-letter", "k"),
  search("common-word", "kalo"),
  search("rare-word", "zanquinne"),
  search("two-words", "dorten mifelbar"),
  search("two-rare-words", "zanquinne tuvomar"),
  search("no-match", "xyzzyq"),
  // No note holds it, and most hold all but its last letter: the slowest scan.
  search("near-miss", "kalomiq"),
  // From a closed switcher: the workers start, read every shard, then scan.
  { name: "search-cold-open", start: "/", action: { type: "zanquinne", open: true }, until: { answered: LIST } },
  ...switches("home", HOME, "today", TODAY, "left"),
  ...switches("today", TODAY, "home", HOME, "right"),
  ...switches("today", TODAY, "recent", RECENT, "left"),
  ...switches("recent", RECENT, "today", TODAY, "right"),
  { name: "tap-home-to-recent", start: HOME, action: { tap: RECENT }, until: { tab: RECENT } },
  { name: "tap-recent-to-home", start: RECENT, action: { tap: HOME }, until: { tab: HOME } },
  ...switches("recent", RECENT, "tasks", TASKS, "left"),
  ...switches("tasks", TASKS, "recent", RECENT, "right"),
  { name: "tap-home-to-tasks", start: HOME, action: { tap: TASKS }, until: { tab: TASKS } },
];
