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

## Who it is for

Anyone with an Obsidian-style vault: Markdown, YAML frontmatter and `[[wikilinks]]`. It is tuned on one large vault that AI agents write. It supports standard Obsidian features, not plugins. PRs welcome.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option.
