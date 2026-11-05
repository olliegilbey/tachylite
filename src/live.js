// Live updates: swap in pushed pages, catch up after a lost connection.
(() => {
  const meta = document.querySelector('meta[name="tachylite-version"]');
  if (!meta || !window.EventSource) return;
  const url = "/.tachylite/events?page=" + encodeURIComponent(location.pathname) + "&v=" + meta.content;
  const events = new EventSource(url);
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
