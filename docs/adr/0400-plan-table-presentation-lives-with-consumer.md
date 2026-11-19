---
type: adr
area: surface
claim: 32510e65ef0d13a8c589f6ce52ed2c29592e2795
name: 0400-plan-table-presentation-lives-with-consumer
status: accepted
tags:
  - observability
  - output
title: The plan table's presentation lives with its one consumer
REFERENCES:
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0359-single-plan-renderer-at-the-boundary]]"
SUPERSEDES:
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
---

# ADR-0400: The plan table's presentation lives with its one consumer

## Context

[ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) placed the plan table's presentation model (the column
vocabulary, the flags, the cell assembly) in `cypherlite-model`'s
`wire::plan_table`, "shared by the engine renderer and every surface's
table layout, so a renamed or added column is a compile error in the
other renderer." Two renderers justified the shared module.

That premise expired: the engine's own text renderer was deleted
([ADR-0359](./0359-single-plan-renderer-at-the-boundary.md)'s composition — every surface renders through
`cypherlite-formatter`), leaving the shared module with exactly one
consumer. LABS-449's descriptor refactor made the one-consumer state
visible: the model carried presentation items no production code outside
the formatter ever called, the module's own doc still told the
two-renderer story, and the visibility rule ("pub(crate) until a second
crate needs them") was inverted — a whole presentation vocabulary sat
`pub` in the engine's model crate for a formatter-only concern.

## Decision

Split the module by consumer:

- **The wire contract stays in `cypherlite-model`** (beside the wire
  types it names): the typed warning vocabulary (`WARNING_*`,
  `warning_text`, `warning_message`) and the estimation-miss policy
  (`estimation_miss`, `ratio_defined`) — the pieces the engine's own
  warning construction and the wire accessor's validation read.
- **The presentation moves to `cypherlite-formatter`** (its
  `plan_table.rs`, beside the walk that feeds it): the `PlanColumn`
  descriptor list, the flags and their per-row fold, the cell triads,
  the never-executed derivation, the glyphs, the footer — module-private
  to the formatter except the marks its own tests pin, one consumer, one
  home. The never-executed Option wrapper (`never_executed_node`) is
  deleted with the move: its one caller (the flag fold) calls the
  metrics-level core directly.

The presentation items are no longer `pub` anywhere: a second renderer
would bring them back out — at that point, and only then, a shared home
returns.

## Alternatives considered

- **Keep the shared module "for when a second renderer comes back"** —
  rejected: speculative surface (the code-shape rules forbid pub until a
  second crate needs it); the move back is mechanical if a renderer ever
  returns, and the one-consumer state is the present fact.
- **A third, presentation-only crate between the model and the
  formatter** — rejected: one consumer means no sharing to name; a crate
  per concern is layering theater while the count is one.
- **Move the contract (the warning vocabulary) to the formatter too** —
  rejected: the engine's `PlanWarning` construction and the wire
  accessor's name validation are contract readers; the vocabulary stays
  with the wire it validates.

## Consequences

- `wire::plan_table` disappears; the contract pieces are re-exported
  from `wire` directly, and the engine's warning construction follows.
- The formatter's module gains the presentation as private items; its
  tests pin the marks by `crate::plan_table` paths.
- [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)'s "shared by both renderers" rationale is superseded for
  the module placement (their metric and column contracts are
  unchanged).
