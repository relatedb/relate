---
type: adr
area: surface
claim: 7d065054eb12921de5d41a812aa3f807289aba48
name: 0359-single-plan-renderer-at-the-boundary
status: proposed
tags:
  - observability
title: One plan renderer — text is rendered at the boundary, never in the engine
REFERENCES:
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
SUPERSEDES:
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
---

# ADR-0359: One plan renderer — text is rendered at the boundary, never in the engine

## Context

`cypherlite-formatter` is the crate the plan text belongs to: its module doc
names `plan_table` (the EXPLAIN/PROFILE plan table) and the shared grid
engine in `layout` as the renderers, and it was extracted as its own surface
precisely so there is one layout. The CLI's table mode already renders the
plan through it, from the plan's wire value.

The engine nevertheless hosts a **second** plan text renderer,
`cypherlite/cypherlite/src/plan_node/render.rs` (`render_lines`,
`render_with`, `render_lines_for_statement`, `render_header_for_statement`).
It exists because [ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) §1 decided that `EXPLAIN` returns its plan as a
data column of rendered string lines ("the ASCII rendering is also carried
as the node's `string-representation` argument"), and [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) §2 kept the
`string-representation` composition engine-side. Both put the *rendering*
into the engine, which cannot depend on the formatter ([ADR-0103](./0103-engine-surfaces-testing-layout.md) R1/R3).

The result is two renderers for one thing, and they disagree: the engine's
pipe-separated text (`Operator | Id | Details …`) is what Browser's Text
view shows through `string-representation`, while the CLI table shows the
formatter's box grid. Every round that touches plan text has to be made in
two places, and the engine API carries rendered strings — "massive string
pass-through" — that every surface must forward.

## Decision

1. **The engine emits the plan tree only.** `plan_node/render.rs` is
   deleted; `PlanNode` exposes its structured value (`to_value`,
   `to_neo4j_value`) and its typed accessors, never rendered text.
2. **`EXPLAIN` no longer returns a plan-as-string-lines data column**
 ([ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) §1 superseded). The plan rides `result.plan` as the tree; the
   plan-column shape was a rendering smuggled into the result shape.
3. **Every boundary renders the plan text on demand through
   `cypherlite-formatter`** — the CLI table, the Bolt
   `string-representation`, the HTTP plan text. Surfaces may depend on the
 formatter ([ADR-0103](./0103-engine-surfaces-testing-layout.md) R3: `surfaces/*` may target `surfaces/*`); the engine
   may not, which is why the renderer cannot live there.
4. **`string-representation` is composed at the surface boundary** from
 that one rendering ([ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) §2's engine-side composition superseded);
   the tree/args decoration itself stays in the engine, only the text moves.
5. **One layout, one text**: the formatter's grid (`plan_table` + `layout`)
   is the only plan layout; the header (`Query:` echo + `Plan:` marker) is
   rendered by the same module, so every channel shows the same bytes.

## Alternatives considered

- **Keep the engine renderer and have the formatter call it** — rejected:
 the engine cannot depend on a surface ([ADR-0103](./0103-engine-surfaces-testing-layout.md)), and the formatter would
  then need the `PlanNode` type across the boundary, re-coupling what the
  wire value already decouples. (The *renderer* keeps the wire-value
  contract; only the presentation composition — which must call the
  engine's decorator — takes `PlanNode`.)
- **Keep the engine renderer and have surfaces call it** — rejected: this
  is the current state; it is the duplication the ADR removes, and it keeps
  rendered text flowing through the engine API.
- **Keep the `EXPLAIN` plan column for compatibility** — rejected: no
  consumer needs it once the surfaces render from the tree, it duplicates
  the tree in a lossy, unparseable form, and it is the reason the engine
  had to render at all. It is a documented breaking change for scripts
  reading that column.
- **Render in the model crate (`cypherlite-model`)** — rejected: the model
  is the shared vocabulary (types, wire form), not a presentation layer;
  putting layout there would give the engine a rendering dependency it does
  not need and still leave two entry points (engine and formatter).

## Consequences

- `docs/api/{cli,bolt,http}/spec.md` document the text rendering at their
  own boundary in the same change.
- `EXPLAIN` results lose the `plan` data column: breaking for scripts that
  read it; the tree carries strictly more information.
- The formatter gains the plan-text entry points (statement + tree → lines,
  header included), and the CLI, Bolt and HTTP call them; the engine's
  `PlanRenderOptions` (width) moves with the renderer.
- `string-representation` becomes a surface-composed argument: the surfaces
  that carry it (Bolt, HTTP) depend on `cypherlite-formatter`.
- The public Rust API loses `PlanNode::render_lines`/`render_with`/
  `render_header_for_statement`; the docs and examples render through the
  formatter instead.

## Later references

- Supersedes [ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) §1 (the engine-produced ASCII rendering and the
 `string-representation` argument's origin) and [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) §2's engine-side
  composition of `string-representation`.
