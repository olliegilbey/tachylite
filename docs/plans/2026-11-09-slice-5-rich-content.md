# Slice 5: rich content and security

**Issue:** #7. **Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`, "Rendering" (embeds) and "Rich content and security".

**Goal:** a note can hold inline HTML, images, embedded notes and embedded `.html` pages, and no script it carries runs on the main origin.

## Global constraints

- Inline HTML passes through an allowlist sanitizer. It drops `script`, `iframe`, `object`, `embed`, event handlers and `javascript:` URLs.
- An `.html` file renders in `<iframe sandbox="allow-scripts">`, with no `allow-same-origin`, and is served with `Content-Security-Policy: sandbox allow-scripts`.
- The main origin's CSP blocks outbound requests and inline script.
- An excluded note reaches no output, embedded or not.
- One code path: a full build and an incremental one render embeds the same way.

## Decisions

- **A hand-written allowlist re-serializer, per raw HTML node.** `ammonia` was built in first and measured: +1.24 MB of binary, an MPL-2.0 licence exception, and 0.47 ms per note. The maintainer chose the hand-written one. `sanitize` tokenizes like a browser, emits only allowlisted tags it builds itself, and escapes every other `<` and `>`. It runs on each `HtmlInline` and `HtmlBlock` literal, so Tachylite's own markup never passes through it, and a note without raw HTML pays nothing. Author iframes are dropped; an `.html` page embeds with `![[page.html]]`.
- **Heading ids move into the AST.** Each heading becomes raw `<hN id>` tags around its children, as links already are. The post-pass `add_ids` matched `<hN>` in the output, and it would give an author's raw `<h2>` the next heading's id.
- **Embeds by target kind:**
  - an image becomes `<img src alt loading="lazy">`. The alt is the label, unless the label is an Obsidian size (`300`, `300x200`); sizing stays out of scope.
  - an `.html` page becomes `<iframe class="embed-html" sandbox="allow-scripts" src loading="lazy">`;
  - a note is transcluded: `<div class="embed"><a class="embed-link" href>Title</a>body</div>`, its body without the properties block;
  - a pdf, a heading embed (`![[note#H]]`) and a block reference are plain links;
  - a missing or excluded target is a broken link, logged.
- **Markdown images resolve like links.** `![alt](img.png)` with no scheme goes through the resolver; a broken one is logged and keeps its `src`.
- **Depth and cycles.** Transclusion nests 3 levels. A deeper embed, or a note already being embedded on the current chain, renders as a plain link.
- **Embeds count as links.** They add backlinks, as in Obsidian. Links inside an embedded note stay the embedded note's: they add no backlink to the embedder and log nothing for it.
- **Embed sources load before the render.** Rendering stays pure. Per batch, the site scans each note's text for `![[...]]`, resolves each target, and loads the notes it finds, up to 3 levels deep. The scan over-matches (code, comments), which only loads a note that is not used. The render threads read the loaded map.
- **Dependents.** A body reports every note it read, at any depth. The site keeps `embedded_by`. Every note that re-renders in an apply also re-renders the notes that embed it. One step suffices, because the reads are already transitive.
- **CSP.** Every response from the vault handler gets one: `.html` files from `files/` get `sandbox allow-scripts`. Pages and assets get `default-src 'self'; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; form-action 'none'`. Mermaid injects `<style>`, hence `'unsafe-inline'` for styles only. Remote images in notes keep working.

## Tasks

1. **Sanitizer.** `render.r#unsafe` on, `sanitize(html) -> String` in `src/sanitize.rs`, heading ids in the AST. Sanitizer tests: classic script vectors, entity-hidden schemes, and a property test that the output holds only allowlisted markup. Render tests: `script`, `onclick`, `javascript:`, `object` dropped; `style` dropped; an author iframe dropped; a raw `<h2>` takes no heading id; a fixture note with raw HTML keeps every Tachylite construct (link, broken link, tag, heading id, callout, footnote, task checkbox, Mermaid, highlight, properties).
2. **Embeds in render.** `render_body(from, markdown, index, sources)`, `Body.embeds`, `pub fn embed_targets(from, markdown, index)`. Render tests per kind, depth, cycle, self-embed, heading embed, missing target, Markdown image.
3. **Site.** Load embed sources per batch and in the `changed` pass. `Links.embeds`, `embedded_by`, the closure into `first`. Site tests: editing, creating and deleting an embedded note re-render the embedder; a nested embed; a cycle. Both property tests gain embeds. Build test: an excluded note embedded anywhere reaches no output.
4. **CSP.** Serve tests for each header. Browser tests: an embedded `.html` page runs its script and cannot read the parent; Mermaid still draws under the CSP.
5. **Docs and budgets.** The spec's security and embed text. 3% of budget-vault notes carry raw HTML, so the budgets cover the sanitizer.

## Paths

Full build, incremental rebuild (the embedder re-renders), push, exclusion (embedded excluded notes) and phone (embed widths) apply. Manifest and search arrive in later slices.
