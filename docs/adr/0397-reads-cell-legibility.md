---
type: adr
area: engine
claim: 04125a36775f7fa6ee8cb451d6bd411d46d15bb5
name: 0397-reads-cell-legibility
status: accepted
tags:
  - observability
title: The read kinds as their own PROFILE columns
REFERENCES:
  - "[[adr/0375-profile-walk-start-and-store-read-kinds]]"
SUPERSEDES:
  - "[[adr/0375-profile-walk-start-and-store-read-kinds]]"
---

# ADR-0397: The read kinds as their own PROFILE columns

## Context

[ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md) added the store-read mix behind `Db Hits` as one compact cell:
`<hops>h <walks>w <node reads>n`. Three usability findings from reading
real PROFILE tables:

1. **The unit letters are unrememberable.** `0h 0w 51n` requires knowing
   h/w/n mean hops, neighbor walks, node-store reads — knowledge that
   lives in the docs, not at the point of reading. Header abbreviations
   (`Reads (h/w/n)`) and full-word headers (`Reads (hops/walks/nodes)`)
   were each tried: initials are still a lookup, and the full-word header
   costs 23 display columns of horizontal space.
2. **The pairs are hard to scan.** Three number-unit pairs packed into
   one cell cannot be compared vertically — spotting the outlier operator
   in a 20-row plan means parsing every cell, not letting the eye run
   down a column.
3. **The zeros are noise.** The kinds always emit together ([ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md)'s
   invariant), so a zero is a measured none — yet it renders as a
   number, crowding the one meaningful value.

## Decision

The read kinds render as **their own columns**, in plain words, between
the `Db Hits` total they break down and the `Memory (Bytes)` closer:

- **`Nodes`** — node-record reads: a scan, label-match or index-seek
  candidate each (the wire's `NodeReads` kind).
- **`Rels`** — relationships followed to their other endpoint (the
  wire's `Hops` kind): the traversal step a reader pictures.
- **`Degree`** — adjacency enumerations, one per neighbor a walk read
  (the wire's `NeighborWalks` kind): "who is this node connected to?".
  `Neighbors` was considered and rejected: the word's two spellings
  (neighbour/neighbor) split its readers; `Degree` is one spelling, six
  letters, and names exactly what the counter counts.

Two rendering policies, both carried by the shared column flags:

- **A kind column renders only when some operator in the tree reported a
  nonzero count of it** — the estimate/warnings columns' gate pattern.
  A plan with no pattern walk gains no `Rels`/`Degree` columns at
  all.
- **A zero renders empty** — measured absence as visual absence, so the
  nonzero counts stand out vertically. This stays distinct from the
  shared unavailable spelling `-`, which means "not produced" (no metric
  set), never zero.

The wire form and the `cypherlite.plan/v1` document keep their three
named fields (`Hops`/`NeighborWalks`/`NodeReads`, `hops`/`walks`/
`node_reads`) unchanged and always present: the columns are the human
table's vocabulary, the field names the engine's.

## Alternatives considered

- **One compact cell with unit letters** ([ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md)'s form) — rejected:
  the letters are a lookup and the pairs cannot be scanned vertically
  (findings 1 and 2 above).
- **The same cell with a header legend, abbreviated or full-word** —
  rejected: a legend the reader must cross-reference is the lookup
  problem moved, and the full-word header costs 23 columns of width; a
  two-line header would need multi-line layout support for a fact three
  own columns state outright.
- **Dropping the zeros inside the compact cell (`51n`)** — rejected
  before the columns: better than `0h 0w 51n`, but still one cell to
  parse, and the vertical-scan finding stands.
- **Combining the kinds into two columns (`Nodes`/`Rels`)** — rejected:
 the hop-vs-walk distinction is the diagnosis [ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md) exists for
  (a scan-turned-traversal reads differently from a pattern walk);
  collapsing it back into one number throws that away.

## Consequences

- Every PROFILE table's read-kind presentation changes; the docs'
  samples and the pinned tests follow in the same change.
- An empty kind cell is a measured zero; `-` stays "not produced"; the
  two spellings never merge.
- [ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md)'s cell format is superseded for the table presentation only;
  its wire and document fields are untouched.
