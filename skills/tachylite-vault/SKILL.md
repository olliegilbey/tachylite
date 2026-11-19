---
name: tachylite-vault
description: How Tachylite reads a vault. Use when writing or editing a note, a task, a daily note or an .html page in a folder that Tachylite serves.
---

# Writing for a Tachylite vault

Tachylite serves a vault of Obsidian-style Markdown as a website. It only reads the vault. Its reader is on a phone, often offline. You write plain files; this says how Tachylite reads them.

## Pick a form

Take the first form that can carry the content.

1. **A Markdown note.** The default. The quick switcher finds it by title, alias and path. It links, and it lists its backlinks. An open page updates as you save. It syncs to the phone ahead of time and reads offline.
2. **Raw HTML inside a note.** For layout Markdown lacks: a collapsible block, a figure with a caption, a keyboard key. It keeps every gain of a note. It carries no script and no styling.
3. **A standalone `.html` page.** For anything that needs a script or its own styling: a chart, a map, an interactive view. It gives up backlinks, live updates, a title and aliases: lists and the switcher show it by file name. One up to 1 MB still syncs to the phone ahead of time.

## Notes

A note's URL is its vault path without `.md`. Its file name is its title and its link name.

**Frontmatter** is the block between a first line of `---` and the next line of `---`. Every key shows in a properties block, in file order. Three keys do more:

```yaml
---
title: Garden plan
tags: [garden, plants/fern]
aliases: [plot plan]
---
```

- `title` names the page in lists, backlinks, the switcher and the browser's title bar. It adds no heading, so write your own `# Heading`. It is no link name: `[[Garden plan]]` matches a file name or an alias.
- `tags` link to their tag pages.
- `aliases` are extra names that links and the switcher match.
- A value that is exactly one link, `related: "[[other note]]"`, renders as a link and counts as a backlink.
- No key hides a note or styles it. `publish: false` and `cssclasses` show as plain properties.

**Links.** `[[note]]`, `[[note|label]]`, `[[note#Heading]]` and `[[#Heading]]`. Matching ignores case, and `.md` is optional. Any other file needs its extension: `[[route.html]]`, `[[plan.pdf]]`.

- A target that starts `./` or `../` resolves from the linking note.
- A target with a `/` resolves from the vault root, then from the linking note's folder.
- A bare name matches a file name anywhere. The nearest folder wins. A tie is a broken link, so write the path when two notes share a name.
- A bare name that matches no file matches an alias.
- `[label](folder/note.md)` resolves the same way. Write a space as `%20`.
- A link that resolves to nothing shows as a broken link.

**Embeds.** Put each on its own line.

- `![[photo.png]]` shows the image. `![[photo.png|A red door]]` sets its alt text. `![alt](folder/photo.png)` works too.
- `![[note]]` shows that note's body under a link to it. It nests 3 levels deep. A note shows once per page; a second embed of it is a link.
- `![[chart.html]]` frames that page.
- `![[note#Heading]]` and `![[plan.pdf]]` render as plain links.

**Other syntax that renders:**

- CommonMark and GitHub Markdown: tables, `~~strikethrough~~`, bare URLs as links, fenced code. A wide table scrolls sideways.
- Footnotes: `text[^1]`, then `[^1]: The footnote.` on its own line.
- Callouts: `> [!note] Title`, with any type name. `> [!tip]- Title` starts folded, and `> [!tip]+ Title` starts open.
- `==highlight==`.
- Tags: `#garden` and `#plants/fern`, after a space or at a line start. A tag holds letters, digits, `_`, `-` and `/`, and at least one non-digit.
- Comments: `%%hidden%%`, inline or across lines, are never published. Close each one: an open comment hides the rest of the note.
- Mermaid: a fence marked `mermaid` draws as a diagram, offline too. Mindmap and architecture diagrams are not drawn.

## Tasks

A task is a list item that starts with a checkbox. Four statuses exist:

```markdown
- [ ] todo
- [/] doing
- [x] done
- [-] cancelled
```

- Write one task per line. Any other character between the brackets makes no task; the line shows as text.
- The note is the task list. Tachylite keeps line order, so put the most important task first.
- A `#tag` or a `[[link]]` on the line works. Keep the line short and link to the detail.
- Indent a task to nest it. A task works in a callout and in an embedded note.
- Tasks are read-only in the browser. To change a status, edit the character in the note.
- Due dates and plugin fields show as plain text.

## Daily notes

A daily note is a note in the vault's daily folder whose name is a date in the vault's format. Read both from `daily` in `tachylite.toml`, else from `.obsidian/daily-notes.json`. The format defaults to `YYYY-MM-DD`, as in `journal/2026-03-02.md`. With neither, the vault has no daily notes.

- The Today tab shows the newest daily note dated on or before the reader's day. Write today's note under today's date.
- Each daily note links to the daily note before it and the one after it, by date.
- A name that does not match the format makes an ordinary note.

## Raw HTML in a note

Tachylite rebuilds raw HTML from an allowlist.

- **Tags kept:** `a abbr b bdi bdo blockquote br caption center cite code col colgroup dd del details dfn div dl dt em figcaption figure h1 h2 h3 h4 h5 h6 hr i img ins kbd li mark ol p pre q rp rt ruby s samp small span strike strong sub summary sup table tbody td tfoot th thead time tr tt u ul var wbr`.
- **Attributes kept:** `align class dir id lang title` on any tag; `href` on `a`; `src alt width height loading` on `img`; `colspan rowspan` on `td` and `th`; `span` on `col` and `colgroup`; `start` on `ol`; `open` on `details`; `datetime` on `time`.
- **Dropped with their content:** `script style iframe noscript noembed noframes textarea title xmp plaintext template svg math`.
- **Dropped:** every other tag, with its text kept. Every other attribute: `style`, event handlers such as `onclick`, `data-*`, `target`. An `href` or `src` that is not `http:`, `https:`, `mailto:`, a path from `/`, or a relative URL with no `&`. HTML comments.
- Leave a blank line after an opening tag and before its closing tag. Markdown and `[[links]]` between them then render:

```markdown
<details><summary>Soil test</summary>

The pH is **6.5**. See [[garden plan]].

</details>
```

- A raw `href` or `src` is not resolved as a vault link. Write links and images in Markdown.

## A standalone `.html` page

- **Link it** with `[[chart.html]]` or `[label](chart.html)`. It opens full screen, as written, with no Tachylite header. Back returns to the note.
- **Embed it** with `![[chart.html]]`. It shows in a frame as wide as the text and 20rem tall, about 340 px, above a link that opens it full screen.
- **Name the file well, and link it from a note.** The switcher, the recent lists and the folder's index show a page by its file name alone. A link from the note that explains it gives the reader the context.
- **Make it self-contained.** Put its CSS, its script and its data in the one file. The vault publishes no `.js`, `.css` or `.json`, so the page cannot load them from it.
- **The sandbox** runs scripts in an origin of their own. Cookies, `localStorage` and IndexedDB throw. `fetch` cannot read another vault file. `alert`, `confirm` and `prompt` do nothing. `window.open` opens nothing.
- **Offline.** A page up to 1 MB syncs to the phone with the notes and opens offline, full screen. Keep a page under 1 MB: a larger one opens offline only after the reader has opened it once. An embedded frame always needs the network, so the link under it is the offline way in. A library or map tile loaded from the web fails offline: inline what the page needs. Inline an image as a `data:` URL.
- **Fit a phone.** Add `<meta name="viewport" content="width=device-width, initial-scale=1">`. Lay it out for a screen 360 px wide, with touch targets.
- **Follow light and dark.** Add `<meta name="color-scheme" content="dark light">`. Style for dark first, and switch under `@media (prefers-color-scheme: light)`. The page cannot use Tachylite's stylesheet.
- An open page does not update when you save it. The phone's next sync fetches the new copy, so the next open shows it. A page over 1 MB shows its saved copy first and fetches the new one behind it, so a change can take two opens to show.

## What does not render

Each of these shows as plain text, or as said.

- Math: `$x^2$` and `$$` blocks. `$` is a literal character.
- Dataview and other plugins: a `dataview` fence shows as code, and `key:: value` as text.
- Block references: `^block-id` shows as text. `[[note#^block-id]]` opens the note at its top.
- Image sizes: `![[photo.png|100]]` shows at full size. Column alignment in a table: every cell aligns left.
- Inline footnotes `^[text]`, emoji codes `:smile:`, definition lists, and colours in code fences.
- A single newline: it joins the two lines. Leave a blank line between paragraphs. End a line with `\` to break it.
- `~text~` strikes text out, as `~~text~~` does. Write `H2O`, not `H~2~O`.
- An image from an `http:` address. Use `https:`, or put the file in the vault.
- An image or PDF the reader has never seen is not on the phone offline.

## What stays out

- Only `.md`, `.html`, `.pdf` and images (`png jpg jpeg gif svg webp avif`) are published. A link to any other file is a broken link.
- Dot names, `target/`, `node_modules/` and each path that `exclude` in `tachylite.toml` matches are never published. A link to one is a broken link. A note over 1 MB is skipped.
