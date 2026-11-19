---
type: adr
area: engine
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0009-logical-planner
status: accepted
tags:
  - planner
title: Logical planner is a real pass, not only parse-then-execute
REFERENCES:
  - "[[adr/0010-label-property-indexes]]"
  - "[[adr/0011-snapshot-isolation]]"
  - "[[adr/0024-match-rewrite-planner]]"
  - "[[adr/0026-eager-residual-match-predicates]]"
  - "[[adr/0027-live-row-variables]]"
---

# 0009. Logical planner is a real pass, not only parse-then-execute

## Context

The first planner was a pass-through of OpenCypher clauses. Dataset-scale
MATCH (IMDb, fraud) needs expand order and later predicate pushdown. A
separate decision keeps “we will plan” from mixing with indexes ([ADR-0010](./0010-label-property-indexes.md))
or transactions ([ADR-0011](./0011-snapshot-isolation.md)).

## Decision

`memorydb-planner` produces a `LogicalPlan` that the executor interprets.
v1 validates the query and preserves clause order (UNION, MATCH, WITH,
CALL). Later revisions of this crate may reorder disconnected MATCH
patterns and push predicates without changing the public Cypher contract.

## Alternatives considered

- **Stay pass-through forever** — simplest, but fraud/IMDb scans stay
  quadratic. Rejected as the long-term shape.
- **Cost-based planner in v1** — no statistics yet. Rejected until indexes
 exist ([ADR-0010](./0010-label-property-indexes.md)) and we have cardinality hints.
- **Compile to LLVM/DataFusion** — huge surface. Rejected; interpret first.

## Consequences

- Executor consumes `LogicalPlan`, not the raw AST, so plan rewrites are
  possible without parser changes.
- A later ADR may introduce physical operators (vectorized batches).

## Later references

[ADR-0024](./0024-match-rewrite-planner.md): MATCH / OPTIONAL MATCH are rewritten after bind (equality
fold, endpoint orient, disconnected order). Clause order of WITH,
writes, and UNION is unchanged. No cost model.

[ADR-0026](./0026-eager-residual-match-predicates.md): residual MATCH `WHERE` conjuncts run when their variables
bind; relationship equality folds into the rel map. No LogicalOp tree.

[ADR-0027](./0027-live-row-variables.md): rows keep only live variables between clauses; projection
does not clone the full input row.
