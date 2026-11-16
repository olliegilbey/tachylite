# Tachylite

Tachylite serves a folder of Obsidian-style Markdown as a fast, offline-capable website. It is a read-only viewer, and a phone on a bad connection is its main client.

One small static binary renders the vault, re-renders only what changes, and pushes updates to open pages. After the first sync, every note and the quick switcher work with no network. Full-text search is not built yet.

## Status

Pre-release. Tachylite is built in slices; see the [`v1` milestone](https://github.com/olliegilbey/tachylite/milestone/1) and the [design doc](docs/superpowers/specs/2026-11-03-tachylite-design.md), which is the spec.

## Run it

```sh
tachylite serve <vault> <out> [--port <port>]
```

Tachylite renders the vault into `<out>`, watches it, and serves the site on `http://127.0.0.1:7420`. An open page updates within moments of a save. `tachylite build <vault> <out>` renders once and exits.

- It binds `127.0.0.1` only. To reach it from a phone, put a proxy in front, such as `tailscale serve` on a tailnet-only port.
- `<out>` must sit outside the vault. Tachylite writes only to a directory that is empty or holds its own earlier output.
- It never writes to the vault.
- It needs Linux 5.6 or later.

## Find your way

- The app opens on a home page: the newest notes, then the vault's top folders.
- Each folder has an index page, and each note links to its folders at the top. Notes whose title starts with an ISO date, such as daily notes, list newest first.
- Recent lists the 50 newest notes. Each folder's index links to its own recent list, which takes in its subfolders.
- Today opens the newest daily note dated on or before the phone's today, offline too. It reads Obsidian's daily-notes settings, or `daily` in the config below.
- Each `#tag` links to a page of the notes with that tag, or with a tag nested under it.
- Find, or Ctrl-K, Cmd-K or `/`, opens a fuzzy switcher over titles, aliases and paths. It works offline.

## Run it on a server

This example keeps Tachylite running on a Linux box, and reaches it from a phone over a tailnet. Build the static binary and put it on your path:

```sh
make release
install -m 755 target/x86_64-unknown-linux-gnu/release/tachylite ~/.local/bin/
```

On an ARM board such as a Raspberry Pi, run `make release TARGET=aarch64-unknown-linux-gnu`, and install from `target/aarch64-unknown-linux-gnu/release/`.

Run it as a systemd user service. Save this as `~/.config/systemd/user/tachylite.service`, with your own vault and output paths:

```ini
[Unit]
Description=Tachylite

[Service]
ExecStart=%h/.local/bin/tachylite serve %h/vault %h/.local/share/tachylite --port 7420
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now tachylite
loginctl enable-linger "$USER"   # keeps it running when you log out
journalctl --user -u tachylite -f
```

Then put `tailscale serve` in front, on a port your tailnet does not already use:

```sh
tailscale serve --bg --https=8443 http://127.0.0.1:7420
```

- `serve` stays inside your tailnet. `funnel` publishes to the internet; do not use it for a vault.
- A port that already carries a Funnel route is public on every path. Check `tailscale funnel status` before you pick a port.
- To refuse requests for any other name, list the proxy's name in `tachylite.toml`: `hosts = ["<machine>.<tailnet>.ts.net"]`. Tachylite then answers 421 to a `Host` that is not `127.0.0.1`, `localhost` or a listed name. This stops a hostile web page on the same machine from reading the vault through a rebound DNS name. Without `hosts`, Tachylite answers any name. A change to `hosts` takes effect at the next start.
- Open `https://<machine>.<tailnet>.ts.net:8443/` on the phone and install it from the browser menu. After the first sync it works offline.

## What it renders

Notes render close to Obsidian's reading view:

- CommonMark and GitHub tables, task lists, strikethrough, footnotes and autolinks.
- Frontmatter as a properties block. `title` names the page, and `aliases` work as link targets.
- `[[wikilinks]]` and Markdown links to notes, resolved the way Obsidian resolves them. Every note lists its backlinks.
- Callouts, `==highlight==` and `#tags`.
- Mermaid diagrams, drawn in the browser. The script loads only on a page that has one.
- `%%comments%%` are never published. When a comment is left open, Tachylite hides everything after it rather than risk showing it, and a notice says so.

## Keep notes private

Put a `tachylite.toml` in the vault root to keep notes out of the site:

```toml
exclude = ["private", "journal/*.md", "**/draft-*.md"]
```

- A pattern matches a path from the vault root. `*` stays inside one folder, and `**` crosses folders. Case is ignored, so `journal/*.md` also hides `journal/Day.MD`. A leading `/` or `./` and a trailing `/` are ignored, so `/private/` works too.
- A pattern that matches a folder hides everything in it, so `private` and `private/**` do the same.
- Dot names, `target/` and `node_modules/` are always hidden.
- An excluded note is gone from every output. A link to it shows as a broken link, the same as a link to a note that does not exist.
- `exclude` goes above any `[table]` header. An unknown key, or `exclude` inside a table, is an error, so a typo stops the build rather than publish the notes.
- A pattern that starts or ends with a space is an error. Each pattern that matches no published path is logged at start and on each save, since it is likely a typo.
- A saved edit to `tachylite.toml` takes effect at once. If the edit does not parse, Tachylite logs the error and keeps the previous config. Deleting the file keeps its exclusions until the next start.
- Save `tachylite.toml` by rename: write a temporary file beside it, then rename it over the config. A rename is atomic, so Tachylite never reads half a save. A script that writes the config in place, as `echo ... > tachylite.toml` does, can be read half written in rare cases, and a half-written config can publish what it excludes.

## Shape the header

These keys in `tachylite.toml` set the header on every page:

```toml
title = "Field notes"
pins = ["Projects/Roadmap.md", "meetings", "maps/route.html|Route"]
daily = { folder = "journal", format = "YYYY-MM-DD" }
```

- `title` names the site. It defaults to Tachylite.
- `pins` adds links under the header. Each is a vault path. A path ending in `.md`, `.html` or another published type is a file; any other is a folder. Add `|label` to name it; the label defaults to the file or folder name. A pin to an excluded path is dropped, and a pin that matches nothing is logged.
- `daily` sets where daily notes live and how they are named. It replaces Obsidian's own daily-notes settings whole: with no `folder`, daily notes sit in the vault root. The format reads `YYYY`, `MM` and `DD`, with any separators and `/` for subfolders, as in `YYYY/MM/YYYY-MM-DD`. A format with another token, such as `ddd`, gives no Today page, and the log says why.
- A change to any of them re-renders every page once, and every installed phone syncs every page once.

## Who it is for

Anyone with an Obsidian-style vault: Markdown, YAML frontmatter and `[[wikilinks]]`. It is tuned on one large vault that AI agents write. It supports standard Obsidian features, not plugins. PRs welcome.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option.
