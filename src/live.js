// Live updates: swap in pushed pages, catch up after a lost connection.
(() => {
  // Mermaid loads only once a page shows a diagram, and draws again after each swap.
  let mermaid = null;
  const diagrams = () => {
    const nodes = document.querySelectorAll("main pre.mermaid");
    if (!nodes.length) return;
    mermaid ??= new Promise((resolve, reject) => {
      const script = document.createElement("script");
      script.src = "/.tachylite/mermaid.js";
      script.onload = () => resolve(window.mermaid);
      script.onerror = reject;
      document.head.append(script);
    });
    // A script that failed to load is fetched again. A failed draw leaves its source as text.
    mermaid
      .then(
        (m) => {
          const dark = matchMedia("(prefers-color-scheme: dark)").matches;
          m.initialize({ startOnLoad: false, theme: dark ? "dark" : "default" });
          return m.run({ nodes, suppressErrors: true });
        },
        () => (mermaid = null),
      )
      .catch(() => {});
  };
  diagrams();

  // The quick switcher: a fuzzy match over every note's title, aliases and path.
  // How well `query` matches `text` as a subsequence: fewer runs first, then an earlier
  // start. Infinity when it does not.
  const score = (query, text) => {
    text = text.toLowerCase();
    const at = text.indexOf(query);
    if (at >= 0) return at;
    let runs = 0;
    let last = -2;
    let start = -1;
    for (let i = 0, j = 0; j < query.length; i++, j++) {
      i = text.indexOf(query[j], i);
      if (i < 0) return Infinity;
      if (start < 0) start = i;
      if (i !== last + 1) runs++;
      last = i;
    }
    return runs * 1000 + start;
  };
  // A title match outranks any alias or path match.
  const rank = (query, [title, , ...rest]) =>
    Math.min(score(query, title), 1e6 + Math.min(...rest.map((t) => score(query, t))));
  let dialog = null;
  let notes = null;
  let events = null;
  // Full-text search: workers hold the text of every note and scan it off this thread, so a
  // keystroke never waits for a scan. They start when the switcher first opens.
  let pool = null;
  // The newest query's number, the answers it still waits for, its hits so far, and its rows.
  let asked = 0;
  let left = 0;
  let hits = [];
  let draw = () => {};
  const answer = (e) => {
    if (e.data.id !== asked) return;
    hits.push(...e.data.hits);
    // Newest first, as each worker's own hits are.
    if (!--left) draw(hits.sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)));
  };
  const row = (url, title, path, snippet) => {
    const a = document.createElement("a");
    a.href = url;
    a.textContent = title;
    const meta = document.createElement("span");
    meta.className = "meta";
    meta.textContent = path;
    a.append(meta);
    if (snippet) {
      const text = document.createElement("span");
      text.className = "snippet";
      const mark = document.createElement("mark");
      mark.textContent = snippet[1];
      text.append(snippet[0], mark, snippet[2]);
      a.append(text);
    }
    const li = document.createElement("li");
    li.append(a);
    return li;
  };
  const show = () => {
    const query = dialog.input.value.trim().toLowerCase();
    const id = ++asked;
    notes.then((list) => {
      if (id !== asked) return;
      const names = query
        ? list
            .map((n) => [rank(query, n), n])
            .filter(([r]) => r < Infinity)
            .sort((a, b) => a[0] - b[0] || a[1][0].length - b[1][0].length)
            .slice(0, 20)
        : [];
      // Three groups, 20 rows in all: the names that hold the query whole, then the notes
      // whose text holds its words, then the names that hold only its letters in order.
      // The last group waits for the text rows, so no row moves once it is drawn.
      const whole = names.filter(([r]) => r % 1e6 < 1000);
      draw = (found) => {
        const seen = new Set();
        const rows = [];
        const add = (url, ...rest) => seen.size < 20 && !seen.has(url) && seen.add(url) && rows.push(row(url, ...rest));
        for (const [, [title, url, path]] of whole) add(url, title, path);
        for (const [url, , ...snippet] of found ?? []) {
          const note = (list.by ??= new Map(list.map((n) => [n[1], n]))).get(url);
          if (note) add(url, note[0], note[2], snippet);
        }
        if (found) for (const [, [title, url, path]] of names) add(url, title, path);
        dialog.list.replaceChildren(...rows);
        dialog.list.setAttribute("aria-busy", !found);
      };
      const search = query && whole.length < 20 && pool.length;
      draw(search ? null : []);
      if (!search) return;
      hits = [];
      left = pool.length;
      for (const w of pool) w.postMessage({ id, words: query.split(/\s+/) });
    });
  };
  const finder = () => {
    if (!dialog) {
      dialog = document.createElement("dialog");
      dialog.className = "switcher";
      dialog.input = document.createElement("input");
      // A search field would take the first Escape to clear itself.
      dialog.input.type = "text";
      dialog.input.enterKeyHint = "go";
      dialog.input.placeholder = "Find a note";
      dialog.input.setAttribute("aria-label", "Find a note");
      dialog.list = document.createElement("ul");
      dialog.list.setAttribute("aria-live", "polite");
      dialog.append(dialog.input, dialog.list);
      document.body.append(dialog);
      dialog.input.addEventListener("input", show);
      dialog.input.addEventListener("keydown", (e) => {
        const first = dialog.list.querySelector("a");
        if (e.key === "Enter" && first) location.href = first.href;
      });
      dialog.addEventListener("keydown", (e) => {
        const step = { ArrowDown: 1, ArrowUp: -1 }[e.key];
        const stops = [dialog.input, ...dialog.list.querySelectorAll("a")];
        const next = step && stops[stops.indexOf(document.activeElement) + step];
        if (next) {
          e.preventDefault();
          next.focus();
        }
      });
      // A tap on the backdrop lands on the dialog itself.
      dialog.addEventListener("click", (e) => e.target === dialog && dialog.close());
    }
    // The worker answers from its cache at once, so this costs nothing offline. While the
    // stream is open, the network's list follows, so a new note shows, and a sync brings
    // the search text that changed.
    const list = (cache) => fetch("/.tachylite/notes.json", { cache }).then((r) => r.json());
    notes = list("default").catch(() => []);
    // One search worker for each core but one, at most four. Without workers, names still
    // match. They start after the list is asked for: their reads of the cache would hold it up.
    if (!pool)
      try {
        const n = Math.max(1, Math.min(4, (navigator.hardwareConcurrency || 2) - 1));
        pool = Array.from({ length: n }, (_, i) => new Worker("/.tachylite/search.js", { name: i + "/" + n }));
        for (const w of pool) {
          w.onmessage = answer;
          // A worker that fails would leave a query waiting for ever.
          w.onerror = () => {
            pool = [];
            draw([]);
          };
        }
      } catch {
        pool = [];
      }
    if (events?.readyState === 1) {
      list("no-cache").then((fresh) => {
        notes = Promise.resolve(fresh);
        if (dialog.open) show();
      }, () => {});
      sync();
    }
    dialog.input.value = "";
    dialog.list.replaceChildren();
    dialog.showModal();
  };
  document.addEventListener("click", (e) => e.target.closest?.("button.find") && finder());
  document.addEventListener("keydown", (e) => {
    // A closed dialog's input can keep the focus after Escape; it is not a text field then.
    const field = e.target.closest?.("input, textarea, select, [contenteditable]");
    const typing = field && !dialog?.contains(field);
    const open = dialog?.open;
    if (((e.ctrlKey || e.metaKey) && e.key === "k") || (e.key === "/" && !typing && !open)) {
      e.preventDefault();
      if (!open) finder();
    }
  });

  // Offline: the worker caches every page. Each (re)connect syncs the pages that changed.
  const worker = navigator.serviceWorker;
  worker?.register("/.tachylite/sw.js", { scope: "/" }).catch(() => {});
  const sync = () => worker?.ready.then((r) => r.active?.postMessage("sync"));
  // The worker says when a sync has changed its cache: the search workers read their text
  // again, and an open switcher asks again.
  worker?.addEventListener("message", () => {
    for (const w of pool ?? []) w.postMessage(0);
    if (dialog?.open) show();
  });

  const VERSION = 'meta[name="tachylite-version"]';
  const meta = document.querySelector(VERSION);
  if (!meta || !window.EventSource) return;
  const TODAY = "/.tachylite/today";
  // The page in <main>. On the Today tab it is the daily note, under the tab's URL.
  let page = location.pathname;
  let retry = 0;
  let wait = 1500;
  let notice = 0;
  let kept = false;

  // A tab page's header marks its tab. A daily note's header marks none, so the client
  // marks the tab this URL names.
  const tabs = () => [...document.querySelectorAll("header > a")];
  const current = (a) => a.hasAttribute("aria-current");
  const mark = (header) => {
    for (const a of header.querySelectorAll(":scope > a"))
      if (a.pathname === location.pathname) a.setAttribute("aria-current", "page");
      else a.removeAttribute("aria-current");
  };
  // Puts a page on screen: its main, its header where that changed, its title and version.
  const adopt = (next, version = next.querySelector(VERSION)?.content) => {
    const main = next.querySelector("main");
    const shown = document.querySelector("main");
    if (!main || !shown) return;
    // The Today list is on screen only when it has no note to show.
    main.querySelector("[data-pick]")?.removeAttribute("data-pick");
    shown.replaceWith(main);
    // A config change moves the header: the tabs and the pins.
    const header = next.querySelector("header");
    const old = document.querySelector("header");
    if (header && old) {
      mark(header);
      mark(old);
      if (header.outerHTML !== old.outerHTML) old.replaceWith(header);
    }
    document.title = next.title;
    meta.content = version;
    diagrams();
  };

  // A network error retries on its own, but an HTTP error, as from a proxy while the
  // server is down, closes the stream for good: retry from 3 s, doubling to a minute. A
  // hidden browser tab holds no stream, since a browser allows six HTTP/1.1 connections per origin.
  const connect = () => {
    clearTimeout(retry);
    events?.close();
    if (document.hidden) return;
    // The stream and the worker's cache go by the page shown, which the Today tab's URL does not name.
    const url = page;
    events = new EventSource("/.tachylite/events?page=" + encodeURIComponent(url) + "&v=" + meta.content);
    events.onopen = () => {
      wait = 1500;
      sync();
      // So the offline copy survives storage pressure. Only Firefox prompts, never once granted.
      kept ||= !!navigator.storage
        ?.persisted?.()
        .then((p) => p || navigator.storage.persist())
        .catch(() => {});
    };
    events.onerror = (e) =>
      e.target.readyState === 2 && (retry = setTimeout(connect, (wait = Math.min(wait * 2, 6e4))));
    events.addEventListener("page", (e) => {
      clearTimeout(notice);
      const next = new DOMParser().parseFromString(e.data, "text/html");
      // A callout stays as the reader left it, matched by its title and its place among
      // callouts of that title, so one added above it takes nothing from it.
      const fold = (root, n = {}) =>
        [...root.querySelectorAll("main details")].map((d) => {
          const t = d.querySelector("summary")?.textContent;
          return [t + "\n" + (n[t] = -~n[t]), d];
        });
      const held = new Map(fold(document).map(([k, d]) => [k, d.open]));
      for (const [k, d] of fold(next)) if (held.has(k)) d.open = held.get(k);
      adopt(next, e.lastEventId);
      worker?.controller?.postMessage({ url, version: e.lastEventId, html: e.data });
    });
    // Vim and git save by removing the note first. Their `page` follows within
    // milliseconds and cancels the notice, so the page neither flashes nor loses its scroll.
    // The id `gone` matches no version, so a reconnect gets the page if it is back.
    events.addEventListener("gone", (e) => {
      clearTimeout(notice);
      meta.content = e.lastEventId;
      notice = setTimeout(() => {
        const p = document.createElement("p");
        p.textContent = "This page was deleted or moved.";
        document.querySelector("main")?.replaceChildren(p);
      }, 1000);
    });
  };

  // A page as a parsed document, read from the worker's cache directly, so no worker need
  // be awake. Then the network.
  const load = (url, cache) =>
    Promise.resolve(!cache && self.caches?.match(url))
      .catch(() => {})
      .then((hit) => hit || fetch(url, { cache }))
      .then((r) => (r.ok ? r.text() : Promise.reject()))
      .then((html) => new DOMParser().parseFromString(html, "text/html"));
  // The pages a switch would show, loaded ahead, so it waits for no read and no parse.
  let ahead = {};
  const hold = (url) => (ahead[url] ??= load(url));
  const get = (url, cache) => (!cache && ahead[url]) || load(url, cache);
  // The Today page lists daily notes newest first, each with its date. Only this device
  // knows its own today, so it picks the newest dated on or before it.
  const pick = (doc) => {
    const d = new Date();
    const p = (n) => String(n).padStart(2, "0");
    const today = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
    const a = [...doc.querySelectorAll("main a[data-date]")].find((a) => a.dataset.date <= today);
    return a ? [a.pathname, a.dataset.date < today] : [TODAY, true];
  };
  // Holds every other tab, and the note Today would show.
  const prepare = () => {
    ahead = {};
    const all = tabs();
    const at = all.findIndex(current);
    for (const a of at < 0 ? [] : all)
      if (!current(a))
        hold(a.pathname)
          .then((doc) => a.pathname === TODAY && hold(pick(doc)[0]))
          .catch(() => {});
  };
  // The worker says when a sync has changed its cache, so no copy held is older than it.
  worker?.addEventListener("message", prepare);
  // Brings on screen what the tab at `url` shows: its own page, or for Today its pick in
  // place of the list. `doc` is the tab's page, when the caller holds it. The last call wins.
  let turn = 0;
  const open = async (url, cache, doc) => {
    const mine = ++turn;
    doc ??= await get(url, cache);
    const [to, older] = (url === TODAY && pick(doc)) || [url];
    if (to !== page) {
      if (to !== url) doc = await get(to);
      if (mine !== turn) return;
      clearTimeout(notice);
      page = to;
      if (location.pathname !== url) {
        history.replaceState(null, "", url);
        scrollTo(0, 0);
      }
      adopt(doc);
      connect();
      prepare();
    }
    // A cached list can predate a note made since the last sync, so the network's list
    // follows, and its pick replaces an older one.
    if (older && !cache && mine === turn) open(url, "no-cache").catch(() => {});
  };
  // A switch that cannot read its page navigates to it, unless a later switch took over.
  const go = (url) => {
    const mine = turn + 1;
    open(url).catch(() => mine === turn && location.replace(url));
  };
  // From one tab page to another, a tap replaces the history entry as a swipe does, so Back
  // never walks through tabs. A modified click keeps the browser's own meaning.
  document.addEventListener("click", (e) => {
    const a = e.target.closest?.("header > a:not([aria-current])");
    if (!a || !tabs().some(current) || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    go(a.pathname);
  });

  // A swipe on a tab page opens the tab beside it, in the header's order, with no wrap. It
  // fires on release, for a sideways move of one finger. The browser keeps every touch: it
  // still scrolls and zooms, and the listeners only watch.
  let down = null;
  addEventListener(
    "touchstart",
    (e) => {
      const t = e.touches[0];
      // An edge swipe is the system's Back. Pinched in, a sideways move pans the page.
      let free = e.touches.length === 1 && t.clientX > 32 && t.clientX < innerWidth - 32;
      free &&= !dialog?.open && !(window.visualViewport?.scale > 1) && tabs().some(current);
      // A sideways scroller keeps its touches: the pins row, a wide code block or table.
      for (let n = e.target; free && n instanceof Element; n = n.parentElement)
        free = n.scrollWidth <= n.clientWidth || getComputedStyle(n).overflowX === "visible";
      // The numbers, not the touch: old Safari reuses the object for the next event.
      down = free ? [t.clientX, t.clientY] : null;
    },
    { passive: true },
  );
  addEventListener("touchcancel", () => (down = null));
  addEventListener("touchend", (e) => {
    const from = down;
    down = null;
    if (!from) return;
    const dx = e.changedTouches[0].clientX - from[0];
    const dy = e.changedTouches[0].clientY - from[1];
    // A long press that selected text is no swipe.
    if (Math.abs(dx) < 60 || Math.abs(dx) < 2 * Math.abs(dy) || !getSelection().isCollapsed) return;
    const all = tabs();
    const to = all[all.findIndex(current) + (dx < 0 ? 1 : -1)];
    if (to) go(to.pathname);
  });

  document.addEventListener("visibilitychange", () => {
    connect();
    // A new day moves the Today tab to its note.
    if (!document.hidden && location.pathname === TODAY) open(TODAY).catch(() => {});
  });
  // With no note to show, or none that loads, the list shows.
  if (page === TODAY)
    open(TODAY, undefined, document)
      .catch(() => {})
      .then(() => {
        document.querySelector("[data-pick]")?.removeAttribute("data-pick");
        events || (connect(), prepare());
      });
  else {
    connect();
    prepare();
  }
})();
