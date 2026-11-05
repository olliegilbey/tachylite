// Live updates: swap in pushed pages, catch up after a lost connection.
(() => {
  const meta = document.querySelector('meta[name="tachylite-version"]');
  if (!meta || !window.EventSource) return;
  const url = "/.tachylite/events?page=" + encodeURIComponent(location.pathname) + "&v=" + meta.content;
  const events = new EventSource(url);
  events.addEventListener("page", (e) => {
    const next = new DOMParser().parseFromString(e.data, "text/html");
    const main = next.querySelector("main");
    const current = document.querySelector("main");
    if (!main || !current) return;
    current.replaceWith(main);
    document.title = next.title;
    meta.content = e.lastEventId;
  });
  // Some editors save by removing the note first, so the stream stays open: a later
  // `page` event brings the note back.
  events.addEventListener("gone", () => {
    const notice = document.createElement("p");
    notice.textContent = "This note was deleted or moved.";
    document.querySelector("main")?.replaceChildren(notice);
  });
})();
