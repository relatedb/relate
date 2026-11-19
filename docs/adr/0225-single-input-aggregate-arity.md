---
type: adr
area: cypher
name: 0225-single-input-aggregate-arity
status: accepted
tags:
  - functions
title: Single-input aggregates require exactly one argument
REFERENCES:
  - "[[adr/0216-javascript-client-ergonomics]]"
  - "[[adr/0217-shared-single-input-aggregate-table]]"
---

# ADR-0225: Single-input aggregates require exactly one argument

> Renamed from [ADR-0216](./0216-javascript-client-ergonomics.md): the number collided with
> docs/adr/0216-javascript-client-ergonomics.md (both landed on main via
> parallel PRs). The decision is unchanged; references were updated in the
> same change.

## Context

LABS-276. `sum()`, `avg()`, `min()`, `max()` and `collect()` with zero
arguments were accepted and evaluated: `agg_eval`
(`cypherlite-executor/src/exec/aggregate.rs`) pushed the constant `1` per
input row for an empty argument list, so `RETURN sum()` returned `1`,
`avg()` `1.0`, `min()`/`max()` `1` and `collect()` `[1]`. Over an empty
input the same calls returned `0`/`null` — the zero-input-row shape, as if
the call had been `sum(<something null>)`.

Reference behavior (Neo4j 2026.09-enterprise, probed 2026-10-12,
LABS-276): every one of the five is error `42I13` — invalid number of
function arguments — naming the expected signature
(`sum(input :: INTEGER|FLOAT|DURATION)`, `min(input :: ANY)`, …).
Confirmed-matching shapes that must not change: `count()` already rejects
(the parser's count keyword path requires an expression), `count(*)` is
`1`, `min(*)` errors, and zero-INPUT-ROW semantics stay as they are
(`sum(x) = 0`, `avg/min/max(x) = NULL` over an empty input — the
reference pins these).

## Decision

Two layers:

1. **Front validation (primary).**
   `validate_aggregation_semantics`
   (`cypherlite-front/src/validate/aggregation/mod.rs`) gains
   `check_aggregate_arity`: every single-input aggregate call
   (`sum`, `avg`, `min`, `max`, `collect`, after canonical-alias
   resolution) with an argument count other than one raises
   `ErrorKind::InvalidNumberOfArguments`, modeled on Neo4j's 42I13 (it
   names the function and the expected signature). It runs over the
   RETURN/WITH item expressions, the GROUP BY keys and the ORDER BY
   expressions, riding the module's shared expression walker and its
   `agg_enter_child` edge vocabulary — the same reach as the other
   aggregation checks (a non-aggregating call's arguments stay opaque,
   matching the module's documented historical verdict). `count` is
   absent from the table: the parser already guarantees it one argument
   or `CountStar`, and `percentileCont`/`percentileDisc` keep their own
   two-argument check at their call site.
2. **Executor defense (secondary).** `agg_eval` replaces the
   constant-`1` empty-argument arm with an `InvalidNumberOfArguments`
   error, checked before the row loop so an empty group cannot slip a
   zero-argument call through. No IR construction — the planner-ir
   diagnostic parser included — can evaluate a zero-argument aggregate.

## Alternatives considered

- **Reject in the parser.** Rejected: generic function-call parsing is
  arity-agnostic (zero-argument calls are legitimate for other
  functions), and `count` already gets its guarantee from its dedicated
  keyword path. Teaching the generic production each aggregate's arity
  duplicates the front's canonical name table in a lower layer.
- **Only the executor error (no front validation).** Rejected: the
  executor error fires per group during evaluation; front validation
  gives the compile-time 42I13 shape the reference reports, before any
  write executes, and one home for the arity table next to the other
  aggregation semantics checks.
- **Also enforce arity for `count` and the percentile aggregates in the
  same table.** Rejected: `count` cannot reach the check with zero
  arguments, and the percentile aggregates take two arguments — a
  different table shape for one call site that already errors. Scope is
  the five the reference pins in LABS-276.

## Consequences

- `RETURN sum()` / `avg()` / `min()` / `max()` / `collect()` (and the
  same inside `WITH`, GROUP BY keys and ORDER BY expressions) is now a
  validation error naming the function and the expected argument count;
  no previously-valid spelling changes meaning.
- `agg_eval` no longer fabricates the constant 1; its zero-argument arm
  is an error instead.
- Zero-input-row semantics are untouched: `UNWIND [] AS x RETURN
  sum(x), avg(x), min(x), max(x)` still returns `0, null, null, null`.
- The openCypher TCK corpus is unaffected (it only uses well-formed
  calls); `cypherlite-tck` stays 3892/3892.

## Later references

- [ADR-0217](./0217-shared-single-input-aggregate-table.md) unifies the class this ADR pinned into one shared
  `SINGLE_INPUT_AGGREGATES` table in `cypherlite-ir` (adding `stdev`/
  `stdevp`, which this ADR's itemized list omitted though its normative
  "every single-input aggregate" wording included them), makes the
  executor backstop exact-arity, and leaves the percentile call-site
  check as this ADR decided.
