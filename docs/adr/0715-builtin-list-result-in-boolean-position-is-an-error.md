---
type: adr
area: cypher
claim: fc036db2a1a85aa64fe24ef907f5eb389f346ad2
date: 2026-11-27
deciders: Relate maintainers
name: 0715-builtin-list-result-in-boolean-position-is-an-error
status: proposed
tags:
  - dialect
  - expressions
title: A builtin's list result in a boolean position is a type error
REFERENCES:
  - "[[adr/0566-compile-time-static-rejections-are-syntax-errors]]"
  - "[[adr/0711-in-non-list-operand-is-one-element]]"
---

# ADR-0715: A builtin's list result in a boolean position is a type error

## Context

A list where a boolean is required has three behaviours on the reference server,
split by how the list arrives:

- a list *literal* in a boolean operator is already the compile-time
  rejection ([ADR-0566](./0566-compile-time-static-rejections-are-syntax-errors.md);
  openCypher TCK Boolean1-3);
- a *dynamically typed* value — a parameter, a property read — takes the
  deprecated runtime list-to-boolean conversion with the 01N02
  notification (`RETURN true AND $list` answers, pinned by passing rows);
- a *builtin call whose declared return type is a list* (`WHERE
  tail([1,2,3])`) is a type error on the reference server — 22N37, "invalid
  coercion. Cannot coerce LIST to BOOLEAN" — and live Neo4j 2026.09
  answers it that way (probed directly). The engine coerced it leniently.

The engine has no general static type checker, but it does not need
one: the builtin's declared return type is a catalog fact (the function
signature registry), and the reference server's split is exactly "the type is
knowable at compile time" versus "it is not".

## Decision

The statically-list-returning builtin names are one curated fact in
`relate-core` — the lowest module the front and the executor both read,
the same home and contract [`crate::arity`]'s shared arity table
carries — pinned to the function signature registry by a drift test in
the executor (every non-aggregating `LIST<…>`-returning registry row is
in the core table, and vice versa; aggregations are absent on purpose —
their own position rules reject them before any coercion question).

The front's static boolean-position rule grows one knowable case: a
direct builtin call to one of those names is rejected wherever a
boolean is required — a boolean operator's operand, the root of a
`MATCH`/`WITH`/`CALL … YIELD … WHERE`, and every other boolean position
the front's static layer already polices through the one shared policy
(today: `allReduce()`'s predicate) — with the `TypeError` class and
the reference server's coercion sentence: `invalid coercion. Cannot coerce LIST
to BOOLEAN.` — one sentence per rule across every boolean position (a
position that pins its own literal-rule sentence, like `allReduce()`'s
predicate, keeps it for the literal rule and reads the shared coercion
sentence for the list rule). A preview-gated name (ADR-0497 §4) is deferred, not
declined: the policy's answer carries the deferral as its own outcome,
and the admission gate — later in the boundary's validation sequence,
after the aggregation-nesting rule that reads gated aggregate aliases —
answers for it, UnknownFunction while the feature is off (a type rule
answering for a hidden name would advertise it); the runtime conversion
answers while the feature is on.
A path pattern's own per-element `WHERE` is deliberately
out of scope: it is a row-level predicate inside the pattern, evaluated
like the dynamically-typed shapes, and no reference rejection pins it.

Everything whose type is not knowable at compile time keeps the
runtime conversion: parameters, property reads, and any name the table
does not carry. A user-defined function's result would belong there
too — dynamically typed by nature — which is exactly why a builtin
alias is the wrong answer for user-function rows and host registration
is the right one.

## Consequences

- `WHERE tail([1,2,3])` and `x AND tail(...)` reject at compile time;
  `WHERE $list`, `WHERE n.coll` and `RETURN tail([1])` (a projection,
  not a boolean position) keep today's answers and their pinned rows.
- The core table is the type-discipline fact; the executor's registry
  stays the catalogue — the drift test keeps the two from diverging,
  so no second name-typed table drifts anywhere.
- The runtime list-to-boolean conversion keeps its deprecation
  notification and its passing behavior; this decision adds no
  runtime change at all.
