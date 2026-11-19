---
type: adr
area: engine
claim: 8762c0120daf1131301b380b08c129cde38bb336
name: 0350-bound-node-pivot-for-pattern-expansion
status: proposed
tags:
  - pattern-matching
  - planner
title: Bound-node pivot for multi-pattern path expansion
---

# ADR-0350: Bound-node pivot for multi-pattern path expansion

## Context

A `MATCH` with several comma/pairwise patterns expands each pattern from
its **first** element (`match_node(&pm, 0, …)` in
`exec/expand.rs::match_resolved_path`), even when a later element's
variable is already bound in the incoming row. For the stackoverflow
pair query (LABS-330)

```cypher
MATCH p1=(u1:User)-[:COMMENTED]->(c1:Comment)-[:COMMENTED_ON]-(q:Question)
MATCH p2=(u2:User)-[:COMMENTED]->(c2:Comment)-[:COMMENTED_ON]-(q)
WHERE id(u1) < id(u2) …
```

the second pattern's first element `u2` is unbound, so it label-scans
every `User` per outer row — while the pattern's *last* element `q` is
bound and would answer the whole pattern with a two-hop reverse walk
from one node. PROFILE (merged per-operator metrics) shows the second
`Expand` at 6.29 s of the query's 6.31 s total, i.e. ~4.5 ms per outer
row for what is a single-node pivot. The same shape appears in the
contact-tracing and recommendations cartesians (LABS-330, both timeboxed
failures at the differential's 20 s budget).

## Decision

When a pattern element at index `k > 0` has its variable bound to a node
in the incoming row, start the expansion at `k` instead of `0`:

- the forward machinery (`match_node` → `expand_rel` → `walk`) covers the
  suffix `k..end` exactly as today, with node `k`'s candidates short-
  circuiting to the bound id (`node_candidates`'s row-bound arm);
- a new **backward walker** covers the prefix `k-1..0`, mirroring the
  forward walk over single-hop rels with each rel's direction flipped;
  it binds nodes/rels under their **original** indices, so
  `bind_named_path` and the `__node{i}`/`__trail{i}` keys stay correct;
- prefixes containing a variable-length or quantified rel, a shortest-
  path function, or a `RepeatableElements` mode fall back to the
  index-0 start (the backward walker is single-hop only in this first
  cut).

The pivot is chosen at execution time (the binding is a runtime fact):
the first element whose variable is row-bound to a `Node`, preferring
the smallest `k > 0` so the largest bound prefix is exploited.

## Alternatives considered

- **Rotate the pattern to start at the pivot and reuse the forward walk**
  — rejected: a linear path rotated at `k` gains a discontinuity (node
  `nk` would jump to node `0`), so the rotated pattern matches a
  different graph shape.
- **Reverse the whole pattern and run the forward walk** — rejected: the
  walk would bind `__node{i}` under reversed indices, so
  `bind_named_path` assembles the named path out of order; remapping
  every binding site is a wider blast radius than the dedicated walker.
- **Planner-side join reordering** — not rejected as a future step, but
  the binding that makes the pivot possible is a runtime fact; the
  executor must know how to start at a bound node regardless.
- **Do nothing (scan is "correct")** — rejected: the family is now
  timeboxed differential failures, not just slow passes (LABS-330).

## Consequences

- The cartesian family (LABS-330) stops re-scanning the pattern's first
  label per outer row; the stackoverflow pair query's second pattern
  becomes a two-hop reverse walk from `q`.
- `expand.rs` gains one mirrored single-hop walker; the forward walk is
  untouched, so no existing path changes behavior.
- Patterns whose prefix is variable-length keep today's behavior — a
  documented gap, not a silent fallback: the pivot is simply not taken
  (the expansion is still correct, just unoptimized).
- LABS-334 (F) and LABS-333 (E) keep their own algorithms; the pivot
  shrinks the pair set those see but does not replace them.
