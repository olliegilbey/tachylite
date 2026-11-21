---
created: 2026-11-21
type: reference
tags: [showcase]
---

# Rendering limits

Tachylite shows each of these as plain text, or as said. This page keeps them in view, so nobody writes one and expects more.

## Shows as plain text

- Math: $x^2 + y^2$. A dollar sign is a plain character, so a price like $4 is safe.
- An inline field: due:: 2026-10-01
- An emoji code: :seedling:
- An inline footnote: ^[like this]
- A block reference at a line's end: ^bed-three

## Shows as code

```dataview
TABLE crop, kilograms FROM "allotment"
```

## Shows, with a difference

Every table cell aligns left, whatever the column asks for:

| Left | Centre | Right |
|:---|:---:|---:|
| a | b | c |

An image shows at full size, whatever width the embed asks for.

A single newline joins two lines.
This line follows a single newline.

End a line with a backslash\
to break it.

## A broken link

A link to a note that does not exist looks like this: [[no such note]]. The server's log names it too. This one is here on purpose.
