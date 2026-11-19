---
type: adr
area: engine
date: 2026-08-30
deciders: MemoryDB maintainers
name: 0024-match-rewrite-planner
status: accepted
tags:
  - index
  - planner
title: MATCH rewrites choose the seek, not Cypher text order
REFERENCES:
  - "[[adr/0009-logical-planner]]"
  - "[[adr/0010-label-property-indexes]]"
  - "[[adr/0026-eager-residual-match-predicates]]"
  - "[[adr/0027-live-row-variables]]"
SUPERSEDES:
  - "[[adr/0009-logical-planner]]"
---

# 0024. MATCH rewrites choose the seek, not Cypher text order

## Context

[ADR-0009](./0009-logical-planner.md) ships `LogicalPlan` as a validated clause list and leaves
expand order and predicate pushdown to a later revision. The executor
still seeds a path at `nodes[0]`: label+equality uses [ADR-0010](./0010-label-property-indexes.md) indexes,
otherwise `all_node_ids()`. `WHERE` runs after the path is produced.
A MATCH written from the unselective end (or with equality only in
`WHERE`) therefore scans, then throws the hops away.

Indexes and MVCC already exist. A cost model does not. The fraud
load-test queries already put `{id: $id}` on the left; this ADR is for
inverted patterns, disconnected MATCH, and `WHERE` equality.

## Decision

`plan()` stays **validate, then rewrite**. Bind/type errors are
unchanged (TCK). Pass 2 rewrites only `MATCH` / `OPTIONAL MATCH`
clauses, in this order:

1. **Fold.** AND-equality `n.k = literal|$param` (either side) and
   `n:Label` conjuncts on a node **introduced in this MATCH** are
   merged into that node’s label list / property map so `lookup_nodes`
   is an IndexSeek. Residual `WHERE` stays on the same clause.
2. **Orient.** For a connected path with no path variable, compare
   seed scores of the **endpoints** only. If the last node is strictly
   more selective, reverse the path and flip hop directions. Middle
   starts are out of scope (the matcher is still left-to-right).
3. **Order.** Disconnected comma patterns are stable-sorted by the
   seed score of each path’s `nodes[0]` (after orient), most selective
   first.

Seed score, highest first: already-bound variable, then label+equality
map, then label only, then bare node.

Logical meaning (realized by the existing executor, not a new IR):

| Op | How |
| --- | --- |
| IndexSeek / LabelScan / AllScan | `lookup_nodes` / `all_node_ids` |
| Expand | `neighbors` after the seed |
| Filter | residual `WHERE` after `match_paths` |

**Barriers.** Do not rewrite `CREATE` / `MERGE` / `DELETE` / `SET`.
Do not reorder across `WITH` or `UNWIND`. Do not reverse a path that
binds a path variable (`MATCH p = …`) — `nodes(p)` / `relationships(p)`
follow written order. Do not fold `OR` / `XOR`, non-constant equality,
or predicates that only mention variables bound before this MATCH.
Do not introduce a cost model or physical/vectorized plan.

## Alternatives considered

- **Keep clause order forever** — correct, inverted MATCH stays a
 full scan. Rejected; that is the [ADR-0009](./0009-logical-planner.md) follow-on.
- **New LogicalOp tree + executor rewrite in the same change** —
  larger TCK risk for the same access methods. Rejected until a rewrite
  cannot be expressed as an equivalent path.
- **Cost-based join order** — no cardinality stats. Rejected (same as
 [ADR-0009](./0009-logical-planner.md)). Revisit after label/seek counts exist.
- **Start in the middle of a path** — needs bidirectional expand.
  Rejected for this ADR.

## Consequences

- Cypher text order is not execute order for MATCH. Results (as a
  multiset of bindings) stay the same; named-path orientation stays
  the written path.
- [ADR-0009](./0009-logical-planner.md)’s “v1 preserves clause order” is superseded for MATCH
  pattern order and `WHERE` equality only.
- EXPLAIN and a LogicalOp IR can come later without changing this
  contract.

## Later references

[ADR-0026](./0026-eager-residual-match-predicates.md): residual MATCH `WHERE` conjuncts run when their variables
bind; relationship equality folds into the rel map. Filter is no longer
only after `match_paths`. The LogicalOp rejection still stands.
Variable-length relationship lists (`-[r*]-`) keep written hop order
and are not reversed (same reason as named paths).

[ADR-0027](./0027-live-row-variables.md): rows keep only live variables between clauses.
