---
type: adr
area: engine
date: 2026-08-31
deciders: MemoryDB maintainers
name: 0027-live-row-variables
status: proposed
tags:
  - executor
title: Rows carry only live variables between clauses
REFERENCES:
  - "[[adr/0009-logical-planner]]"
  - "[[adr/0024-match-rewrite-planner]]"
  - "[[adr/0026-eager-residual-match-predicates]]"
---

# 0027. Rows carry only live variables between clauses

## Context

MATCH binds full `Node` / `Relationship` values. `project_rows` cloned
the entire prior row, evaluated RETURN/WITH items, then trimmed to
output names. Unused hop variables (and their property maps) stayed in
every intermediate row. [ADR-0026](./0026-eager-residual-match-predicates.md) does not change row width.

`WITH` is already the language barrier. TCK requires `ORDER BY` /
`LIMIT` on the pre-projection row and `WITH` WHERE to see
pre-projection names. `WITH *` / `RETURN *` keep visible columns.

## Decision

A row carries only bindings that a later clause in the same query part
still needs. After each MATCH, drop keys that the remaining suffix does
not reference. `WITH` remains the projection barrier.

Projection evaluates into a new row. It does not clone the full input
row and then discard keys. `WITH` WHERE and `ORDER BY` still run on a
merged row that includes every name those clauses mention (TCK).

Entities stay full `Node` / `Relationship` values. This ADR does not
introduce lazy property maps.

## Alternatives considered

- **Keep wide rows until WITH/RETURN trim** — correct, copies unused
  entities through multi-clause pipelines. Rejected as the remaining
 [ADR-0009](./0009-logical-planner.md) projection pushdown.
- **Hydrate properties on access** — changes value identity and SET
  semantics. Rejected; larger TCK risk than dropping unused variables.
- **LogicalOp Project nodes** — same rejection as [ADR-0024](./0024-match-rewrite-planner.md) / [ADR-0026](./0026-eager-residual-match-predicates.md).

## Consequences

- `MATCH (a)--(b) RETURN b.id` does not keep `a` after MATCH.
- `WITH *` and `RETURN *` keep today’s visible columns (no drop).
- `ORDER BY` / `LIMIT` stay on the pre-projection row
  ([`docs/tck/compliance.md`](../tck/compliance.md)).
- `WITH` WHERE still sees pre-projection names.
