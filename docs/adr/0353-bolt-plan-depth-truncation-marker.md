---
type: adr
area: surface
claim: e05853d70e9f81f02a7472b3c25c5beaa7c303a8
name: 0353-bolt-plan-depth-truncation-marker
status: accepted
tags:
  - bolt
  - observability
title: Bolt RUN SUCCESS depth-truncation marker for structured plans
REFERENCES:
  - "[[adr/0224-result-column-types]]"
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
---

# ADR-0353: Bolt RUN SUCCESS depth-truncation marker for structured plans

**Later references**: [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) moves the plan summary — and with it this
marker — from the RUN SUCCESS to the SUCCESS that ends the result (the
PULL/DISCARD that consumes its last row), Neo4j's placement. The marker's
meaning and the depth-limit policy are unchanged.

## Context

[ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) carry an `EXPLAIN`/`PROFILE` statement's structured plan in
the Bolt RUN SUCCESS summary (`plan` / `profile`). A pathological
statement — e.g. a hundred chained `WITH` clauses, each nesting one
operator deeper — can nest the plan tree past packstream's container
depth limit (128). `Pack::from_mdb` then fails, and the summary carried no
plan key at all: a depth-truncated `PROFILE` was wire-indistinguishable
from a plain run (the passthrough test pins that a plain run carries
neither key), so a client could not tell "no plan" from "plan too deep to
pack" — the pack failure was silent (LABS-346 finding 1).

## Decision

When the pack conversion of the plan tree fails, the RUN SUCCESS summary
carries `planTruncated: true` instead of the `plan`/`profile` key. The
plan key itself stays absent (nothing half-packs); the marker is a
Bolt-transport fact — the depth limit is this boundary's, so no other
surface (HTTP, CLI, FFI) carries an equivalent key and the result-metadata
alignment rule is not extended. The statement's own rows are unaffected.
Pinned by the protocol test `run_success_marks_a_depth_truncated_plan`
beside the passthrough test.

## Alternatives considered

- Keep silently omitting the key — rejected: indistinguishable from a
  plain run (the failure LABS-346 filed); a client profiling a deep
  statement would believe the statement carried no plan.
- Pack a depth-truncated plan tree (cut at the limit) under the key —
 rejected: the shared `PlanNode::to_value` wire form ([ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)) would
  gain a depth-aware variant every surface reads, for a pathological
  case; a partial tree also reads as a complete plan unless every
  consumer learns the cut marker anyway.
- Fail the RUN — rejected: the statement itself executed fine; only this
  transport's depth limit cannot carry the summary metadata.
- Move the marker into the engine's `QueryResult` metadata — rejected:
  the truncation is caused by this surface's packstream limit, not by the
  engine (JSON-carrying surfaces have no such limit); embedding it in the
  shared result would force a null-ish default on every other surface.

## Consequences

- Bolt clients can distinguish a depth-truncated `EXPLAIN`/`PROFILE` from
  a plain run; the marker is advisory, and Neo4j drivers ignore unknown
  summary keys (the same posture as the `columnTypes` extension,
 [ADR-0224](./0224-result-column-types.md)).
- `docs/api/bolt/spec.md` documents the key beside the structured-plan
  section.
- The end-to-end driver-facing plan-summary shape (field names drivers
  actually parse) remains under investigation in LABS-349.

## Later references

- [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) moves the plan pair — and with it this marker — to the final
  PULL/DISCARD SUCCESS (Neo4j's placement, so clients that read only the
  drained result's summary see it). The depth policy, the marker key and
  the "never half-pack" rule are unchanged; the pinning test is
  `pull_success_marks_a_depth_truncated_plan` beside the passthrough test.
