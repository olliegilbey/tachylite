# Slice 5: rich content and security

**Issue:** #7. **Spec:** `docs/superpowers/specs/2026-11-03-tachylite-design.md`, "Rendering" (embeds) and "Rich content and security".

**Goal:** a note can hold inline HTML, images, embedded notes and embedded `.html` pages, and no script it carries runs on the main origin.

## Global constraints

- Inline HTML passes through `ammonia`. It drops `script`, `object`, `embed`, event handlers and `javascript:` URLs.
- An `.html` file renders in `<iframe sandbox="allow-scripts">`, with no `allow-same-origin`, and is served with `Content-Security-Policy: sandbox allow-scripts`.
- The main origin's CSP blocks outbound requests and inline script.
- An excluded note reaches no output, embedded or not.
- One code path: a full build and an incremental one render embeds the same way.

## Decisions

- **Sanitize the whole body, and only when the note has raw HTML.** comrak splits inline HTML into single tags (`<span>`, then text, then `</span>`), so it cannot be sanitized per node. Without raw HTML, comrak escapes every author byte, and Tachylite's own markup is safe by construction. So notes without raw HTML pay nothing.
- **The allowlist keeps what Tachylite writes:** `class`, `id`, `data-*` and `aria-*` on any tag, plus `section`, the task checkbox `input` (`type`, `checked`, `disabled`) and `iframe`. No `style` attribute.
- **Iframes stay, always sandboxed.** ammonia forces `sandbox="allow-scripts"` on every `iframe`, and the CSP's `frame-src 'self'` keeps them on the origin. Tachylite's own `.html` embeds pass the sanitizer too, so a note with raw HTML keeps them. An author's iframe gets the same sandbox as an embed. The spec text changes to match.
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

1. **Sanitizer.** `render.r#unsafe` on, `sanitize(html) -> String` behind one `LazyLock` builder, heading ids in the AST. Render tests: `script`, `onclick`, `javascript:`, `object` dropped; `style` dropped; an author iframe forced to the sandbox; a raw `<h2>` takes no heading id; a fixture note with raw HTML keeps every Tachylite construct (link, broken link, tag, heading id, callout, footnote, task checkbox, Mermaid, highlight, properties).
2. **Embeds in render.** `render_body(from, markdown, index, sources)`, `Body.embeds`, `pub fn embed_targets(from, markdown, index)`. Render tests per kind, depth, cycle, self-embed, heading embed, missing target, Markdown image.
3. **Site.** Load embed sources per batch and in the `changed` pass. `Links.embeds`, `embedded_by`, the closure into `first`. Site tests: editing, creating and deleting an embedded note re-render the embedder; a nested embed; a cycle. Both property tests gain embeds. Build test: an excluded note embedded anywhere reaches no output.
4. **CSP.** Serve tests for each header. Browser tests: an embedded `.html` page runs its script and cannot read the parent; Mermaid still draws under the CSP.
5. **Docs and budgets.** The spec's security and embed text. `make budget`, and the cold build with every note forced through the sanitizer, as its worst case.

## Paths

Full build, incremental rebuild (the embedder re-renders), push, exclusion (embedded excluded notes) and phone (embed widths) apply. Manifest and search arrive in later slices.
