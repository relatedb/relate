---
type: adr
area: engine
claim: d6fa67d25bc682d31fd0b198f0888472ea01f4f0
name: 0357-plan-summary-placement-and-neo4j-plan-args
status: accepted
tags:
  - observability
title: Plan summary on the drained result, with Neo4j-shaped plan args
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0293-configurable-bolt-hello-identity]]"
  - "[[adr/0309-bolt-summary-metadata-neo4j-4-4]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0353-bolt-plan-depth-truncation-marker]]"
  - "[[adr/0359-single-plan-renderer-at-the-boundary]]"
  - "[[adr/0374-plan-artifacts-on-query-result]]"
---

# ADR-0357: Plan summary on the drained result, with Neo4j-shaped plan args

## Context

[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) promised the structured plan of an `EXPLAIN`/`PROFILE` statement
in Bolt summary metadata; the surface put it in the **RUN SUCCESS**
summary. Verified against real Neo4j 5.26 (the driver's raw
`summary.metadata`): Neo4j carries `plan`/`profile` in the **final PULL
SUCCESS** summary — beside `type`, `db`, `t_first`, `t_last`, `fields`,
`server`, `bookmark` — and not on RUN. The JS and Python drivers merge both
summaries, so CypherLite's placement worked there, but cypher-shell reads
only the drained result's summary and rendered **no plan at all**
(LABS-349, reproduced). Three further gaps surfaced in the same comparison:

- Neo4j's plan root args carry `string-representation` — the rendered plan
  text, headed by `Cypher`/`Planner`/`Runtime` lines that tooling parses
 (cypher-shell's summary block, Browser's footer and Code tab). [ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)
  promised this argument; it was never implemented, and Browser's footer
  rendered `Cypher version: , planner: , runtime: .` empty.
- Neo4j sends `t_first` on both the RUN and the ending SUCCESS and
  `t_last` on the ending one; the Bolt surface sent `t_first` on RUN only
 ([ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md)), so cypher-shell — which reads the ending summary — printed
 `Time: -1`. The ending SUCCESS now repeats `t_first` ([ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md)'s keys
  plus the plan pair).
- Neo4j's plan args carry the planner/runtime identity
  (`planner`/`planner-impl`/`planner-version`,
  `runtime`/`runtime-impl`/`runtime-version`, `version`) and `Memory` on the
  root; `EstimatedRows` is a float.

## Decision

1. **Placement**: the Bolt `plan`/`profile` pair rides the SUCCESS that
   **ends** the result — the PULL or DISCARD that consumes its last row,
 the same SUCCESS [ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md) gives `type`, `t_last`, `db`, `stats` and
 `bookmark` (and the depth-truncation marker of [ADR-0353](./0353-bolt-plan-depth-truncation-marker.md)) — never the
 RUN SUCCESS, which stays [ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md)'s `{fields, columnTypes, t_first}`
   plus the transaction `qid`. The ending SUCCESS repeats `t_first`
   (Neo4j carries it on both), and a DISCARD that ends the result carries
   the plan pair too.
2. **Neo4j-shaped plan args**: `PlanNode::to_neo4j_value(&self, statement)
   -> Value` — the engine hosts the one decoration, the lowest shared
   module for its three sibling surfaces. Called on a result's plan
   **root** (the callers hold it as `result.plan: Option<Box<PlanNode>>`
   and destructure their own `Option` once, which they need anyway for the
   `is_profiled` key pick): the decoration is root-only by intent — the
   representation renders the whole statement's plan and the
   planner/runtime identity is a root fact — never a child node. It adds
   `string-representation` (the engine's one rendering, headed by the
   `Cypher`/`Planner`/`Runtime`/`Runtime version` lines), `version` (`5` —
 the Cypher language version, [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)'s openCypher 5 front-end),
   `planner`/`planner-impl`/`planner-version` and
   `runtime`/`runtime-impl`/`runtime-version` (all `cypherlite` — the honest
   names; never Neo4j's `COST`/`SLOTTED`, which would claim a planner and
   runtime this engine does not run), and `Memory` (the root's
   row-materialization bytes, mirrored under Neo4j's arg name).
   `EstimatedRows` mirrors as a float. The decorated args are built from
   the same node/args assembly `to_value` uses, never a re-parse of the
   serialized form, and the typed table/speedscope renderers keep walking
   the undecorated `to_value` tree — the engine-form wire accessor stays
 strict (an integer or absent, anything else fails loudly, [ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)),
   so the float mirror is read only by Neo4j-form consumers, never by the
   plan-table layout.
3. **The engine's own wire form stays unchanged**: `Id`, `Details`,
   `EstimatedRows` (integer), `Rows`, `TimeNs`, `DbHits`, `MemoryBytes`.
   The mirroring lives in the separate `to_neo4j_value` entry, never in
   `to_value`, and every surface calls it before converting the value to
   its own wire type — so Bolt, the HTTP Query API and the CLI's machine
   modes carry the same plan metadata without `to_value` growing
   Neo4j-specific fields.
4. The driver smoke (`python_driver_smoke.py`) pins the placement and the
   decorated args through the official driver.
5. **Statement retention is accounted**: an open result keeps the RUN's
   statement text for the ending summary's plan pair, and that text counts
 against `--max-result-bytes` ([ADR-0110](./0110-bolt-connection-query-governance.md)) beside the rows' logical bytes —
   reserved in both the buffered and the execution-failed arm — so a
   transaction full of huge statements cannot park unbounded text outside
   the cap.

## Alternatives considered

- Keep the plan on RUN and add it to PULL as a duplicate — rejected: two
  placements for one fact; Neo4j has one, and clients that read either
  summary get the same answer from the drained result.
- Put the Neo4j args in the engine's `to_value` — rejected: the engine's
  wire form would carry Neo4j-specific vocabulary (a planner/runtime
  identity it does not have) that the CLI's own renderer and the FFI do not
  want; the decoration is a separate entry (`to_neo4j_value`) the surfaces
  call, never a change to `to_value`.
- Spread the decoration into each surface — rejected: three surfaces would
  carry three copies of one vocabulary; the engine is their lowest shared
  module, so the one `to_neo4j_value` lives there and every surface calls
  it at its boundary.
- Claim Neo4j's planner/runtime names (`COST`/`SLOTTED`) for tool
  compatibility — rejected: a false product claim; the honest
  `cypherlite` values render fine in Browser and cypher-shell (verified).
- Skip `string-representation` and let tools render from the operator tree
  — rejected: cypher-shell's summary block and Browser's Code tab read the
 text form, and [ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) already committed to it.

## Consequences

- cypher-shell renders the plan summary block (Version/Planner/Runtime
  from the representation header, `Time` from `t_first`); two of its
  numbers (`Rows`, `Memory (Bytes)`) still read `0`/`?` — a remaining
  cosmetic gap noted in LABS-349.
- `operatorType` stays unsuffixed (Neo4j appends `@neo4j`); Browser renders
  the unsuffixed names fine. Projection `identifiers` stay empty (Neo4j
  names the returned columns) — a content gap, pinned as shape-only in the
  smoke until the engine stamps them.
- cypher-shell and the Python 5.x driver still hard-check the HELLO agent
 string; the documented `--identity`/`CYPHERLITE_IDENTITY` shim ([ADR-0293](./0293-configurable-bolt-hello-identity.md))
  is the compatibility path, unchanged by this decision.
- `docs/api/bolt/spec.md`, `docs/api/http/spec.md`,
  `docs/api/cli/spec.md`, `docs/api/rust/spec.md` and the CHANGELOG carry
  the placement, the args and the new engine method.

- [ADR-0359](./0359-single-plan-renderer-at-the-boundary.md) supersedes item 2's engine-side composition of
  `string-representation`: the surface renders the plan text through
  `cypherlite-formatter` and passes it to
  `PlanNode::to_neo4j_value(plan_text)`; the engine only heads it with the
  identity block.

## Later references

- [ADR-0374](./0374-plan-artifacts-on-query-result.md): decision 2's `result.plan: Option<Box<PlanNode>>` shape becomes
  `Option<Box<PlanArtifacts>>` — the root beside the notifications derived
  with it — read through `QueryResult::plan_root()`; the placement, args and
  decoration decisions above are unchanged.
