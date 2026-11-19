---
type: adr
area: surface
claim: f0865ebfa1c96a1f401040e96a08493005a34bf2
name: 0384-table-wrapping-to-a-width-budget
status: proposed
tags:
  - output
title: Human tables wrap to a width budget
---

# ADR-0384: Human tables wrap to a width budget

## Context

The human `table` output lays out through the shared grid engine
(`cypherlite-formatter::layout`): column widths are the measured content
width, so a wide cell — a long property, a plan operator's `Details`
pattern — pushes the grid past the terminal and the reader scrolls
sideways (LABS-399).

The engine already had one per-column knob: a user-set `.width` truncated
a cell to a fixed column width with an ellipsis. Its only writer was a
dot-command that has since been withdrawn, so the knob is dead — and it
truncates, which hides the very data the reader is looking for.

## Decision

1. **The grid takes a total-width budget.** When the measured widths
   exceed it, the widest columns give up width first, down to a floor
   (`MIN_COLUMN_WIDTH`), and the cell renderer **wraps** their content over
   as many lines as the column needs, at word boundaries — falling back to
   a character boundary for a word wider than its column, never splitting a
   multi-column character. A row whose cells all fit still renders as one
   line.
2. **The budget is the terminal width, and it is explicit when it
   matters.** `cypherlite --width N` sets it; otherwise the terminal width
   (`COLUMNS`) applies when stdout is a terminal; a piped or redirected run
   keeps the natural width, so its output stays byte-stable and diffable.
3. **`markdown` ignores the budget.** A GFM table has no wrapped cells, so
   markdown lays out at its natural width.
4. **The dead per-column `.width` knob is retired** — the truncate-with-
   ellipsis path, its `ELLIPSIS` marker and the `column_widths` field go
   with it. A total budget is what a terminal actually provides, and
   wrapping preserves the data the knob discarded.

The wrapping lives in the one shared layout engine, so the data table and
the plan table wrap by construction — no renderer re-implements it.

## Alternatives considered

- **Keep truncating (the per-column knob's behaviour).** Rejected: it
  discards the tail of the very cell the reader opened the table for, and
  the knob's only writer was already withdrawn.
- **Shrink columns without wrapping.** Rejected: a shrunk column still
  needs the content to go somewhere; truncation is the only alternative,
  and it hides data.
- **Wrap in each renderer.** Rejected: the code-shape rule is one shared
  grid engine — the table and the plan table already lay out through it,
  and a per-renderer wrap would drift (the two renderers were merged into
  this module for exactly that reason).
- **Always wrap to a hard-coded 80 columns.** Rejected: piped output would
  change shape for no reader's benefit, and a wide terminal would wrap
  needlessly.

## Consequences

- Wide results fit the terminal; the plan table's long `Details` column
  wraps instead of overflowing.
- The CLI gains `--width N` (documented in the flag table) and the
  terminal-width auto-detect; piped output is unchanged, so scripts and
  golden files stay stable.
- A very narrow budget wraps aggressively down to the floor and then stops
  shrinking — the grid may still exceed the budget when every column is at
  the floor, which is the honest outcome rather than a zero-width column.
- `markdown` output keeps its natural width; a wrapped GFM table is not
  representable.
