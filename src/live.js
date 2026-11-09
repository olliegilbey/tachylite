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

  // A folded callout opens and closes on a tap on its title.
  document.addEventListener("click", (e) => {
    const title = e.target.closest?.('.callout:not([data-callout-fold="none"]) > .callout-title');
    if (!title) return;
    const callout = title.parentElement;
    callout.dataset.calloutFold = callout.dataset.calloutFold === "collapsed" ? "expanded" : "collapsed";
  });

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
      dialog.input.type = "search";
      dialog.input.placeholder = "Find a note";
      dialog.input.setAttribute("aria-label", "Find a note");
      dialog.list = document.createElement("ul");
      dialog.append(dialog.input, dialog.list);
      document.body.append(dialog);
      dialog.input.addEventListener("input", show);
      dialog.input.addEventListener("keydown", (e) => {
        const first = dialog.list.querySelector("a");
        if (e.key === "Enter" && first) location.href = first.href;
      });
      // A tap on the backdrop lands on the dialog itself.
      dialog.addEventListener("click", (e) => e.target === dialog && dialog.close());
    }
    // The worker answers from its cache, so this costs nothing offline.
    notes = fetch("/.tachylite/notes.json").then((r) => r.json()).catch(() => []);
    dialog.input.value = "";
    dialog.list.replaceChildren();
    dialog.showModal();
  };
  document.addEventListener("click", (e) => e.target.closest?.("button.find") && finder());
  document.addEventListener("keydown", (e) => {
    const typing = e.target.closest?.("input, textarea, select, [contenteditable]");
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
  const url = "/.tachylite/events?page=" + encodeURIComponent(location.pathname) + "&v=" + meta.content;
  const events = new EventSource(url);
  events.addEventListener("open", sync);
  let notice = 0;
  events.addEventListener("page", (e) => {
    clearTimeout(notice);
    const next = new DOMParser().parseFromString(e.data, "text/html");
    const main = next.querySelector("main");
    const current = document.querySelector("main");
    if (!main || !current) return;
    current.replaceWith(main);
    document.title = next.title;
    meta.content = e.lastEventId;
    diagrams();
  });
  // Vim and git save by removing the note first. Their `page` follows within
  // milliseconds and cancels the notice, so the page neither flashes nor loses its scroll.
  events.addEventListener("gone", () => {
    clearTimeout(notice);
    notice = setTimeout(() => {
      const p = document.createElement("p");
      p.textContent = "This note was deleted or moved.";
      document.querySelector("main")?.replaceChildren(p);
    }, 1000);
  });
})();
