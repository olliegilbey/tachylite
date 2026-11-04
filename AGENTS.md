# AGENTS.md

Tachylite serves a folder of Obsidian-style Markdown as a fast, offline-capable website. It is a read-only viewer, and a phone on a bad connection is its main client.

You are working here with a pair: build paths are agreed first, so the maintainer can follow the reasoning and defend it later. You bring software fundamentals, Rust, and the web platform, and your knowledge from having read _Rust for Rustaceans_ - Gjengset, _High Performance Browser Networking_ - Grigorik, and _The Design of Everyday Things_ - Norman. Apply these principles freely. Tachylite is a new project and you are seeing it with fresh eyes: read the design and the code before you build, and build to its conventions. Where the work leans on a crate, a browser API, or Obsidian's behaviour, WebSearch the current documentation before relying on it, or brief a subagent to research and come back with findings.

The design and the reason behind each choice: `docs/superpowers/specs/2026-11-03-tachylite-design.md`. Read it before changing anything structural.

## What makes Tachylite special

These hold for every change.

1. **Speed is the first feature.** A saved note reaches an open page on the phone in about 100 ms. A cached page opens at once. Each speed target is a budget. CI enforces the budgets it can measure, and `make budget` measures the timed ones on the reference host.
2. **Offline after the first sync.** Every note and full search work on the device with no network.
3. **The vault is the owner's, and Tachylite only reads it.** Notes stay plain Markdown, YAML frontmatter and `[[wikilinks]]`. Agents write the vault in a format they already know, and Tachylite changes how it is read, never how it is written.
4. **Light enough for a Raspberry Pi.** One small static binary, no runtime dependencies, idle at zero CPU. The Pi is the bar, checked by hand each release.

## A note from Ollie

"We need barebones, lightning. A raspberry pi, or maybe even an esp32 should be able to run this thing."

The smallest thing that works, made fast, beats the clever thing. Fight scope creep. When a feature costs page weight, a dependency, or a millisecond on a budget, it pays for itself or it waits.

## Glossary

- **vault**: the folder of Markdown that Tachylite reads.
- **note**: one `.md` file in the vault.
- **page**: any HTML at a URL that the manifest lists. Clients cache every page for offline use.
- **note page**: the page rendered from one note.
- **html page**: an `.html` file from the vault, served with its content unchanged and a sandbox header.
- **index page**: a page generated from many notes: a tag page, a folder index, recent notes, the board. Index pages live under the reserved prefix `/.tachylite/`.
- **asset**: an image or PDF. It is not in the manifest. A client fetches it on first open and keeps it cached.
- **output**: the directory Tachylite writes. It lives outside the vault.
- **manifest**: the list of every page with its content hash. Clients sync against it.
- **push**: new page HTML sent to open clients over Server-Sent Events.
- **client**: a browser showing Tachylite, usually the installed PWA on a phone.
- **fixture vault**: the small hand-written vault in the repo that snapshot tests run against.
- **budget vault**: the 2,000-note synthetic vault a seeded generator builds for the budgets. It is generated, never committed.
- **reference vault**: the maintainer's private vault, where real-world rendering is checked. It never enters the repo.
- **reference host**: the maintainer's machine, where `make budget` measures the timed budgets.
- **budget**: a performance limit with a test that enforces it.
- **task**: one checkbox line in a note, with one of four statuses: todo, doing, done, cancelled. Avoid "todo" and "ticket" as synonyms.
- **task list**: the tasks in one note, in line order.
- **board**: the `/.tachylite/tasks` view, grouped by status.

## The four ways to hurt yourself

1. **Writing to a vault.** Tachylite opens vault files read-only. Tests run against the fixture vault or a vault generated in a temp directory. Output goes to a directory outside the vault.
2. **Leaking content.** This repo is public. Code, fixtures, test names, snapshots, issues, commits, PR bodies and screenshots carry synthetic content only. Nothing from the reference vault, and no paths or names from the owner's machine. An excluded note reaches no output file in any form: page, search index, backlink, manifest or metadata.
3. **Touching the network edge.** Tachylite binds `127.0.0.1` only. Tailscale and its `serve` and `funnel` config belong to the owner; leave them alone. A port that carries a public Funnel route is public on every path, so a wrong `tailscale serve` command can publish the vault.
4. **Killing by pattern.** The host runs many dev servers and agents at once. Never `pkill -f`, `pgrep | kill`, or kill a PID found by matching a name or path. Kill only a PID you captured at spawn, or the owner of your port from `ss -H -ltnp` after you confirm `/proc/<pid>/cwd` is your checkout.

## Principles

Ordered by weight. When two collide, the earlier one wins.

- **KISS.** Build the smallest correct thing.
- **Budgets are law.** A change that breaks a budget is a broken change, however good the feature.
- **Read-only vault, explicit publish list.** Only `.md`, `.html`, images and PDF reach the output. Every other file type stays out.
- **Functional core, I/O at the edges.** Rendering, link resolution and indexing are pure functions over data. Watching, serving and file writes stay thin at the edges.
- **One code path for full and incremental builds.** A full build is an incremental build of every note. Two paths drift apart.
- **The platform before a library.** HTML, CSS, a service worker and a few hundred lines of vanilla JS. No client framework, no npm and no client build step in anything that ships.
- **Heavy repo, light binary.** The repo carries every test and check that keeps the binary light. Test tooling may use what it needs: the browser suite runs Playwright from its own directory, and it never ships.
- **Every dependency pays rent.** A new crate states its cost in binary size and compile time in the PR.

## Hit every path

The likeliest defect here is a change that works in a full build and goes stale in an incremental one. Before calling a feature done, walk this list and say which entries applied:

- **Full build.** `tachylite build` renders it.
- **Incremental rebuild.** Editing, creating, renaming and deleting the source note each update it, and every page that depends on it: backlinks, transclusions, tag pages, folder indexes, recent notes, the board.
- **Push.** An open client receives the new HTML.
- **Manifest and offline.** The page hash changes, and an offline client picks it up on reconnect.
- **Search.** The index reflects it within 5 s.
- **Exclusion.** An excluded note produces none of it.
- **Phone.** It reads and works on a narrow screen with touch.

## Great code here

The stack: Rust stable, edition 2024, one crate until a second earns its place. Client assets are plain files embedded in the binary. Write code that passes the gate set by construction:

- The gate set: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and the budget benches at CI's ceiling. `make check` runs them the way CI does; run it green before any push. CI also runs `make size` for both musl targets. The first build slice creates the Makefile and CI. Until then, run the cargo commands directly.
- `cargo machete` and `cargo deny check` run in `make check`. They fail on an unused crate, a disallowed licence or a known advisory.
- Dependencies track the newest releases. Add a crate with `cargo add`. `make upgrade` updates the stable toolchain and moves every crate to its latest version, across major versions.
- `make budget` runs the timed budgets on the reference host. Put its numbers in the PR body.
- Functions over structs where there is no real state.
- Errors are explicit. A library function returns a typed error. The binary reports it once, at the top. Nothing is swallowed. No `unwrap` outside tests.
- No `unsafe`.
- Test at the highest seam that can fail. The build seam takes a vault and checks the output directory. The live seam runs `tachylite serve` and checks HTTP and SSE. The browser seam drives headless Chromium through Playwright against a real server. Lower tests exist only where an invariant lives.
- Snapshot tests for rendering: one fixture note in, HTML out. Property tests where invariants live: link resolution, the manifest, exclusion, and live output equal to a fresh full build after any sequence of edits.
- Watcher, push and browser tests await the event they expect. A test that sleeps to pass is wrong. Clippy's `disallowed-methods` forbids sleeping, and the browser suite never uses fixed timeouts.
- Default to no comment. Write one for a constraint, an external quirk, or an invariant the code cannot show, in one line where you can.
- Client JS stays under the page-weight budget. No web fonts, no animation that repaints continuously.

## Style

- Short sentences. Active voice. One instruction per sentence. Glossary terms. Use this register in code comments, docs, issues and PRs. When messaging the maintainer, lead with the bottom line, keep it speed-readable, and give the context behind each request.
- Repo files, test names, commits and PR bodies cite nothing outside the repo except public docs and GitHub issues.
- Commits: Conventional Commits with a scope, `type(scope): message`, for example `feat(render): resolve relative wikilinks`. Squash-merge; the PR title becomes the commit.
- Branches: `type/short-kebab-name`. Work on a branch, never on `main`.
- PR body: fill in the PR template. It asks for the problem, the fix, the budgets before and after, the paths hit, new crates, and the model and harness that did the work.
- The leak check: `../tachylite-docs/leak-check.sh` scans the branch's diff, commit messages and PR body against a private denylist. A git `pre-push` hook runs it. A contributor without `../tachylite-docs` leaves that box for the maintainer.
- Agents push their own branch, open the PR, and merge it with `gh pr merge --squash` once it is ready. Ready means every checklist box in the template is ticked: `make check` green, the leak check passed, a fresh subagent reviewed the whole diff and its real findings are fixed, CI green, no review thread unresolved.

## Docs and work artifacts

- `README.md` is for users: what Tachylite does, how to install it, how to point it at a vault. It carries an example deployment: Tachylite on a devbox, behind `tailscale serve` on a tailnet-only port.
- The design doc records decisions and their reasons. When a decision changes, rewrite the affected text; do not append a second account.
- Public or private: if a line tells a stranger anything about the maintainer's vault, machines, network or life, it goes in `../tachylite-docs/`, which lives outside the repo. Everything else goes in the repo. Tachylite is built for any Obsidian-style vault, so the repo describes a generic user and setup.
- Implementation plans go in `docs/plans/`. Session handoffs go in `../tachylite-docs/status/`; read the newest one when you start. The reference deployment is in `../tachylite-docs/deployment.md`. A merged PR is the implementation record.
- Track work that needs the public repo in GitHub issues, under a milestone. Track the rest in `../tachylite-docs/tasks.md`, as Tachylite tasks.
- Most code changes need no doc change. Add a paragraph only when a maintainer would get something wrong without it.

If a rule here fights the task in front of you, say so plainly and get the maintainer's sign-off before you break it.
