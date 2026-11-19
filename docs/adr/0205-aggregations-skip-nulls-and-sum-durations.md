---
type: adr
area: cypher
date: 2026-10-11
deciders: CypherLite maintainers
name: 0205-aggregations-skip-nulls-and-sum-durations
status: proposed
tags:
  - functions
  - semantics
title: Aggregations skip nulls; `sum`/`avg` accept durations
REFERENCES:
  - "[[adr/0036-fold-one-hop-aggregates]]"
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0291-graph-examples-extras-and-schema-ddl]]"
---

# 0205. Aggregations skip nulls; `sum`/`avg` accept durations

- **Related**: [ADR-0036](./0036-fold-one-hop-aggregates.md) (one-hop aggregate fold), [ADR-0144](./0144-graph-examples-differential-suite.md) (graph-examples
 differential), [ADR-0291](./0291-graph-examples-extras-and-schema-ddl.md)

## Context

The graph-examples differential ([ADR-0144](./0144-graph-examples-differential-suite.md)) flagged the contact-tracing
walkthrough (`contact-tracing:164`, `contact-tracing:209`) on

```cypher
… WITH hp, sum(duration.inSeconds(maxStart, minEnd)) AS overlaptime …
```

with `type error: sum() needs numbers`. LABS-254's first hypothesis was
that aggregations passed null rows into the accumulator; probing the built
CLI shows every aggregation already skips nulls (`sum`/`collect`/`min`/`max`
filter them, `count(x)` counts non-null, `count(*)` counts rows) — that is
the pinned openCypher TCK behavior and it passes 3892/3892.

The actual divergence is the value type: `duration.inSeconds` returns a
`DURATION` (the pinned TCK, `Temporal10`, mandates `PT…` duration results,
and the Neo4j reference documents the same return type), so the rows
reaching `sum()` are durations and `sum()` only accepted numbers. The
reference engine's `sum()` and `avg()` accept durations and return a
duration, so the reference computes the query while CypherLite errors.

Two inconsistencies sat next to this:

- The [ADR-0036](./0036-fold-one-hop-aggregates.md) one-hop fold (`pushdown.rs`) returned `null` for `sum`
  over only-null (or zero) rows while the grouped path (`aggregate.rs`)
  returns `0`; the reference returns `0`.
- `avg()` silently dropped non-numeric values, so `avg` over durations
  returned `null` instead of a value or an honest error.

## Decision

1. **Null-skipping stays as is** — every aggregation except `count(*)`
   skips null input values; this is confirmed by unit tests pinning the
   existing behavior (mixed null/number, all-null, `collect` ignoring
   nulls, `count(x)` vs `count(*)`).
2. **`sum()` accepts `DURATION` values.** Durations accumulate
   component-wise (`months`/`days`/`nanos`, via the existing
   `temporal::add_duration`) and the result is a `DURATION`. Mixing
   numbers and durations in one `sum()` is a type error. `sum()` over
   zero non-null values stays `0` (INTEGER) in every path — the grouped
 evaluator, the horizontal evaluator, and the [ADR-0036](./0036-fold-one-hop-aggregates.md) one-hop fold
   (which previously returned `null` there).
3. **`avg()` accepts `DURATION` values** the same way, returning the
   accumulated duration scaled by `1/n` (`temporal::scale_duration`, the
   engine's duration-division semantics). Mixing numbers and durations is
   a type error; `avg()` over zero non-null values stays `null`.
4. **`count`, `collect`, `min`, `max`, `stDev`, `stDevP`,
   `percentileCont`, `percentileDisc`** keep their current (TCK-passing)
   semantics; only their null-skipping is pinned harder by tests.

## Alternatives considered

- **Make `duration.inSeconds` return an INTEGER** (legacy Neo4j behavior).
  Rejected: the pinned openCypher TCK (`Temporal10`) requires a `DURATION`
  result (`'PT269112H'` and friends), and the current Neo4j reference
  documents `DURATION` too; changing it would break the TCK floor.
- **Teach the differential harness to rewrite the walkthrough query** (an
 [ADR-0291](./0291-graph-examples-extras-and-schema-ddl.md) "extra"). Rejected: the query is plain Cypher both engines
  accept; rewriting hides a real semantic gap instead of closing it, and
  the same gap hits any user query summing durations.
- **Error on nulls inside `avg`/`stDev`/percentiles for non-numeric
  non-null input.** Rejected for this change: out of LABS-254's scope, and
  the current lenient filter is what the TCK corpus exercises; tightening
  it is a separate decision.

## Consequences

- The two contact-tracing DIFFs close: `sum(duration.inSeconds(…))`
  aggregates durations instead of erroring.
- `sum()` over only-null rows is `0` on every evaluation path; the
  one-hop fold and the grouped path can no longer disagree.
- The hot loop gains one `match` arm per accumulated value (a
  `Temporal::Duration` discriminant check); the no-null, all-numeric path
  keeps its existing shape — no extra passes, no per-row allocations.
- Any future aggregate over a non-numeric, non-duration value still hits
  the existing type error.
