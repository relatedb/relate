---
type: adr
area: infra
date: 2026-10-13
name: 0246-defer-query-latency-plan-goldens
status: accepted
tags:
  - latency-suite
title: Defer the query-latency plan goldens
REFERENCES:
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0229-remove-system-ontologies-cypher-surface]]"
  - "[[adr/0243-query-latency-suite]]"
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
  - "[[adr/0247-query-latency-sf0-1-literals]]"
---

# ADR-0246: Defer the query-latency plan goldens

> Renamed from [ADR-0229](./0229-remove-system-ontologies-cypher-surface.md): the number collided with
> docs/adr/0229-* on main (both were taken by parallel PRs). The
> decision is unchanged; references were updated in the same change.

## Context

[ADR-0243](./0243-query-latency-suite.md) item 5 pins `EXPLAIN` of each graph-benchmark-ldbc query at SF0.1
in `workloads/gbl/plans/sf0.1/qNN.plan`, as "the noise-free check for
planner cliffs of the K98 kind": comma-joined MATCH parts run in an order
that starts from the wrong node, turning a millisecond query into seconds
or minutes (q19, q22, q27 at SF1 on 0.11.8 and 0.12.0).

Building the suite showed that `EXPLAIN` cannot see those cliffs. It
renders the logical plan clause by clause
(`cypherlite/cypherlite/src/explain.rs`, `describe_clause`): q19 explains
as

```text
1. MATCH (2 patterns) + WHERE filter
2. RETURN (2 items) + ORDER BY + LIMIT
```

The decisions that made K98 slow — the order the parts run in
(`cypherlite-planner` `order_patterns`), the node each part starts from and
its orientation ([ADR-0218](./0218-stats-based-match-orientation.md)), and whether an index serves the start — are not
in that output. A golden of it would change only when a query's clause
structure changes, which never happens for a fixed query text. It would
pass through every regression it was meant to catch.

## Decision

1. **No plan goldens.** The query-latency suite does not pin `EXPLAIN`
 output. [ADR-0243](./0243-query-latency-suite.md) item 5 is withdrawn; nothing under
   `workloads/gbl/plans/` is created, and there is no
   `QUERY_LATENCY_UPDATE`.
2. **Planner cliffs are caught by latency.** [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md)'s per-query compare
   (p50 ≤ max(1.5 × envelope, envelope + 0.5 ms)) carries that job: a K98
   cliff is a 100× or larger slowdown, far outside the band. The
 correctness checks of the suite are the answers ([ADR-0243](./0243-query-latency-suite.md) item 4),
   errors and timeouts.
3. **An `EXPLAIN` that shows the physical plan is out of scope** for this
   work (maintainer decision). If a later ADR makes `EXPLAIN` render the
   part order, start node and index choice, plan goldens can be
   reconsidered under a new ADR.

## Alternatives considered

- **Keep the goldens on the current `EXPLAIN`** — rejected: they would
  look like a planner gate while checking only clause structure, which a
  fixed query text never changes.
- **Extend `EXPLAIN` to the physical plan first, then pin it** —
  rejected for this work (maintainer decision): it changes a public
  surface across the engine and every client, with its own ADR and
  documentation, well beyond a test suite.
- **Pin a private planner dump** (an internal debug rendering of the
  ordered parts) — rejected: it would make a test crate depend on planner
  internals that the R1-R3 boundaries keep private, and freeze an
  undocumented format.

## Consequences

- [ADR-0243](./0243-query-latency-suite.md) and [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md) carry **Later references** notes naming this ADR;
  their mentions of plan goldens no longer apply.
- A plan change that does not move latency beyond the band goes
  unnoticed by the suite. That is accepted: the suite gates latency, not
  plan shape.
- The 19 weak SF0.1 answers ([ADR-0243](./0243-query-latency-suite.md) Consequences) now rest on the SF1
  answers, run locally, and on the latency gate.

## Later references

- [ADR-0247](./0247-query-latency-sf0-1-literals.md): SF0.1 runs 14 queries with literal substitutions, so the
  weak SF0.1 answers this ADR's Consequences mention are down to the five
  negative checks upstream also has at SF1; the SF0.1 answers carry
  correctness weight again.
