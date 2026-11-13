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
      script.onload = () => {
        const dark = matchMedia("(prefers-color-scheme: dark)").matches;
        window.mermaid.initialize({ startOnLoad: false, theme: dark ? "dark" : "default" });
        resolve(window.mermaid);
      };
      script.onerror = reject;
      document.head.append(script);
    });
    mermaid.then(
      (m) => m.run({ nodes, suppressErrors: true }),
      () => (mermaid = null),
    );
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
  const show = () => {
    const query = dialog.input.value.trim().toLowerCase();
    notes.then((list) => {
      const hits = query
        ? list
            .map((n) => [rank(query, n), n])
            .filter(([r]) => r < Infinity)
            .sort((a, b) => a[0] - b[0] || a[1][0].length - b[1][0].length)
            .slice(0, 20)
        : [];
      dialog.list.replaceChildren(
        ...hits.map(([, [title, url, path]]) => {
          const a = document.createElement("a");
          a.href = url;
          a.textContent = title;
          const meta = document.createElement("span");
          meta.className = "meta";
          meta.textContent = path;
          a.append(meta);
          const li = document.createElement("li");
          li.append(a);
          return li;
        }),
      );
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
    // stream is open, the network's list follows, so a new note shows.
    const list = (cache) => fetch("/.tachylite/notes.json", { cache }).then((r) => r.json());
    notes = list("default").catch(() => []);
    if (events?.readyState === 1)
      list("no-cache").then((fresh) => {
        notes = Promise.resolve(fresh);
        if (dialog.open) show();
      }, () => {});
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

  const meta = document.querySelector('meta[name="tachylite-version"]');
  if (!meta || !window.EventSource) return;
  let retry = 0;
  let wait = 1500;
  let notice = 0;
  let kept = false;
  // A network error retries on its own, but an HTTP error, as from a proxy while the
  // server is down, closes the stream for good: retry from 3 s, doubling to a minute. A
  // hidden tab holds no stream, since a browser allows six HTTP/1.1 connections per origin.
  const connect = () => {
    clearTimeout(retry);
    events?.close();
    if (document.hidden) return;
    events = new EventSource(
      "/.tachylite/events?page=" + encodeURIComponent(location.pathname) + "&v=" + meta.content,
    );
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
      const main = next.querySelector("main");
      const current = document.querySelector("main");
      if (!main || !current) return;
      // A callout stays as the reader left it.
      const open = [...current.querySelectorAll("details")].map((d) => d.open);
      main.querySelectorAll("details").forEach((d, i) => i in open && (d.open = open[i]));
      current.replaceWith(main);
      document.title = next.title;
      meta.content = e.lastEventId;
      diagrams();
      worker?.controller?.postMessage({ url: location.pathname, version: e.lastEventId, html: e.data });
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
  document.addEventListener("visibilitychange", connect);
  connect();
})();
