---
created: 2026-11-21
type: reference
tags: [showcase, allotment]
---

# Planting calendar

## The year as a diagram

A fence marked `mermaid` draws as a diagram, offline too.

```mermaid
gantt
    title Sowing and harvest
    dateFormat YYYY-MM-DD
    axisFormat %b
    todayMarker off
    section Beans
    Sown    :2026-03-01, 30d
    Picked  :2026-07-01, 90d
    section Carrots
    Sown    :2026-04-01, 30d
    Lifted  :2026-07-01, 90d
    section Squash
    Planted :2026-05-15, 20d
    Cut     :2026-08-01, 75d
```

## A decision as a diagram

```mermaid
flowchart TD
    A[Frost forecast?] -->|Yes| B[Fleece the blossom]
    A -->|No| C[Leave it open]
    B --> D[Take the fleece off by ten]
```

## Code

```python
def rows(bed_length_cm: int, spacing_cm: int) -> int:
    """How many rows fit in a bed."""
    return bed_length_cm // spacing_cm
```

Inline code works too: `rows(240, 20)` gives `12`.

## Raw HTML in a note

A folded block:

<details><summary>Why sow salad every fortnight?</summary>

A short row every two weeks gives leaves all summer. One long row **bolts** all at once. See [[Allotment plan]].

</details>

A figure with a caption:

<figure>

![[allotment-beds.svg|The four beds and the fruit corner]]

<figcaption>The plot, seen from the gate.</figcaption>

</figure>

A keyboard key: press <kbd>/</kbd> to open Find.
