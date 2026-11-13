# Tachylite

Tachylite serves a folder of Obsidian-style Markdown as a fast, offline-capable website. It is a read-only viewer, and a phone on a bad connection is its main client.

One small static binary renders the vault, re-renders only what changes, and pushes updates to open pages. After the first sync, every note and full search work with no network.

## Status

Pre-release. Tachylite is built in slices; see the [`v1` milestone](https://github.com/olliegilbey/tachylite/milestone/1) and the [v1 spec](https://github.com/olliegilbey/tachylite/issues/14).

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
- Recent lists the 50 newest notes.
- Find, or Ctrl-K, Cmd-K or `/`, opens a fuzzy switcher over titles, aliases and paths. It works offline.

## Run it on a server

This example keeps Tachylite running on a Linux box, and reaches it from a phone over a tailnet. Build the binary and put it on your path:

```sh
cargo build --release
install -m 755 target/release/tachylite ~/.local/bin/
```

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
- `%%comments%%` are never published. When a comment is left open, Tachylite hides everything after it rather than risk showing it.

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

## Who it is for

Anyone with an Obsidian-style vault: Markdown, YAML frontmatter and `[[wikilinks]]`. It is tuned on one large vault that AI agents write. It supports standard Obsidian features, not plugins. PRs welcome.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option.
