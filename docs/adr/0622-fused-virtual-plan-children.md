---
type: adr
area: engine
claim: 26830290b52e875f17524fe653db5c42438d107e
name: 0622-fused-virtual-plan-children
status: proposed
tags:
  - observability
  - planner
title: Fused virtual children — display-only plan steps under an index-seek MATCH
REFERENCES:
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0486-range-index-starts]]"
---

# ADR-0622: Fused virtual children — display-only plan steps under an index-seek MATCH

## Context

An index-seek-started `MATCH` shows in `EXPLAIN`/`PROFILE` as one opaque
operator: the seek that actually starts it and the hops it expands are
invisible, because the clause remains the executed unit ([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)) and the
plan tree carries executed operators only. [ADR-0486](./0486-range-index-starts.md) named the seek in the
plan tree but still as part of the one operator. The surface that consumes
a plan today is a human reading the CLI's table renderer; there are no
external consumers of the plan wire yet.

## Decision

The plan tree carries **fused virtual children** under a fused `MATCH`:
a seek step, then one `Expand` step per hop of the logical pattern. The
children are display-only — the executed clause does not change ([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)
stands). A fused child carries a parent-link key naming the operator it
breaks down, and the seek step alone carries a measured pair
(seek-phase time, node reads); no fused child ever carries the PROFILE
metric block, and no value is copied from the parent. Consumers that
require per-operator metrics skip fused children (the speedscope export
does; the timeline is the executed operators).

## Alternatives considered

- **A parallel breakdown structure beside the tree** — every renderer
  would need a second walk and a second shape; the human surface reads
  the tree, so the steps belong in it. Rejected.
- **Real per-operator execution accounting for the seek and each hop** —
  contradicts [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)'s executed unit and requires per-hop counters that
  do not exist; fabricating them would mislead. Rejected for this change;
  honest metrics stay limited to what is measured.
- **Do nothing** — the opaque operator is the recurring profiling
  complaint this exists to answer. Rejected.

## Consequences

The plan wire gains the fused link key and the seek's measured pair; a
fused child never carries the metric block, which is a contract a parser
enforces rather than a convention. Renderers that assumed every child
carries metrics must skip fused children. Per-hop counters remain future
work, as does rendering the steps in surfaces beyond the table renderer.
