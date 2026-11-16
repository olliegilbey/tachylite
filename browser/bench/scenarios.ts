import { phone } from "../playwright.config";

/** A named device and CPU speed. `cpu` is the CDP throttling rate: 4 runs the page four times slower. */
export type Profile = { name: string; device: typeof phone; cpu: number };

export const profiles: Profile[] = [
  { name: "phone-1x", device: phone, cpu: 1 },
  { name: "phone-4x", device: phone, cpu: 4 },
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
};

/** What the reader does. It runs in the page, and the clock starts as it is called. */
export type Action =
  /** A plain navigation, as a tapped link makes. */
  | { go: string }
  /** Any script: a tap, typed text, a swipe. It runs alone in the page, so it sees no outer variable. */
  | { run: (vars: Vars) => void };

/** The milestone that ends the clock. */
export type Until =
  /** The first contentful paint of a new document at this path. */
  | { navigation: string }
  /** The first paint of text or an image in an element the action added, matched by this selector. */
  | { paint: string }
  /** The page at this path, back on screen from the back/forward cache. */
  | { restore: string };

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

export type Scenario = Warm | First;

export const scenarios: Scenario[] = [
  { name: "home-to-recent", start: "/", action: { go: "/.tachylite/recent" } },
  { name: "recent-to-home", start: "/.tachylite/recent", action: { go: "/" } },
  { name: "home-to-note", start: "/", action: { go: "{note}" } },
  { name: "note-first-load", first: "{note}" },
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
];
