---
type: adr
area: surface
claim: d3eea77402bb5046b38a44399d25859321049487
name: 0345-plan-surface-alignment-cli-bolt-http
status: accepted
tags:
  - observability
title: Plan surface alignment — CLI text rendering, Bolt and HTTP passthrough
REFERENCES:
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
  - "[[adr/0359-single-plan-renderer-at-the-boundary]]"
---

# ADR-0345: Plan surface alignment — CLI text rendering, Bolt and HTTP passthrough

## Context

[ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) defines the `PlanNode` tree. Every surface that can show a query
plan must expose it in the same change (AGENTS.md §2 result-metadata
alignment): the CLI renders it for humans, Bolt and the HTTP Query API carry
it for client tooling. Neo4j's own field shape (`queryPlan` /
`profiledQueryPlan`, operator `operatorType` + `identifiers` + `arguments`
incl. the full ASCII `string-representation`) is what existing drivers and
Browser-like tooling already render — matching it means those tools work
against this engine unmodified. The current `_profile_*` result columns
(LABS-163) corrupt the result shape and are not renderable.

## Decision

1. CLI: `EXPLAIN`/`PROFILE` print the Neo4j-compatible fixed-width ASCII
   tree — columns `Operator | Id | Details | Estimated Rows | Rows |
   DB Hits | Memory (Bytes)`, tree glyphs `+`/`| |`/`|\`, and the footer
 totals line — omitting the Page Cache column ([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)). The ASCII
   rendering is also carried as the node's `string-representation`
   argument so any structured consumer can show the text form.
2. Bolt: EXPLAIN/PROFILE responses carry the plan as SUCCESS summary
   metadata (`plan` / `profile`), in the Neo4j dict shape.
3. HTTP Query API: the response envelope carries `queryPlan` (EXPLAIN) /
   `profiledQueryPlan` (PROFILE) with the recursive node shape.
4. The `_profile_*` data-row columns are deprecated: phase 1 stops adding
   them to EXPLAIN output; their removal from PROFILE lands with phase 2
   (PROFILE tree + metrics) so the metrics they approximated remain
   available in a non-corrupting shape in between.

## Alternatives considered

- A CypherLite-specific plan JSON shape — rejected: client tooling that
  already renders Neo4j plans would need per-engine forks; matching the
  incumbent shape costs nothing here.
- Keep `_profile_*` columns alongside the tree — rejected: two
  representations of the same facts is exactly the parallel-state shape
  AGENTS.md's code-shape rules forbid; deprecate and remove.

## Consequences

- `docs/api/{cli,bolt,http}/spec.md` document the plan output and the
  field shape in the same change as phase 1.
- Client tooling that renders Neo4j plans renders this engine's plans
  unmodified.
- The `_profile_*` removal is a documented breaking change for any script
  parsing those columns; the tree supersedes them with strictly more
  information.

## Later references

- [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) moves item 2's Bolt summary metadata from the RUN SUCCESS to
  the final PULL/DISCARD SUCCESS (Neo4j's placement, so clients that read
  only the drained summary see it) and implements item 1's
  `string-representation` argument on the plan root, with the rest of the
  Neo4j-facing root args. The field shape itself is unchanged.

- [ADR-0359](./0359-single-plan-renderer-at-the-boundary.md) supersedes item 1's engine-produced rendering and the
  `string-representation` argument's origin: the engine emits the plan tree
  only, `EXPLAIN` no longer returns a plan column, and every boundary
  renders the plan text through `cypherlite-formatter` on demand.
