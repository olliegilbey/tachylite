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
