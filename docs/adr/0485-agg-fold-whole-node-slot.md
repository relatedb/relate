---
type: adr
area: engine
claim: 890ca8e424e2b28c1e3b4b3caa60114ac8f6452a
name: 0485-agg-fold-whole-node-slot
status: proposed
tags:
  - executor
  - perf
title: Stream count(var) through the path fold's whole-node slot source
REFERENCES:
  - "[[adr/0468-soa-batch-substrate]]"
---

# ADR-0485: Stream count(var) through the path fold's whole-node slot source

## Context

The LABS-780 repro —
`MATCH (n:User)-[:Friend]->()-[:Friend]->(m:User) RETURN count(m)` — falls
off every streaming seam and materializes rows. The findings pin the exact
decline: the path fold's `path_item`
(`cypherlite-executor/src/.../agg_fold/plan.rs:246-280`) routes `count(m)`
into `node_prop_slot`, whose `cypherlite_ir::var_prop`
(`cypherlite-ir/src/ast.rs:474-481`) matches only
`Expr::Property{Variable, key}`; a bare `Expr::Variable` returns `None`, the
item loop's `?` (`plan.rs:349`) declines the whole fold, and the one-hop
fold's shape gate (`agg_fold/mod.rs:379`) then rejects the 2-rel pattern.
Every other shape gate passes, and `StreamAgg::offer_value`
(`agg_fold/acc.rs:112-132`) already absorbs any offered `Value` with the
inventory's null policy: `count(var)` counts non-null, and a bound endpoint
of a fixed non-optional pattern is never null, so `count(m)` is the group's
binding count and `count(DISTINCT m)` dedups on node id through the existing
`DistinctAcc`.

The aggregate vocabulary has a single source (`AGGREGATE_CONTRACTS` in
`cypherlite-ir/src/aggregates.rs`, re-exported through
`cypherlite-front/src/agg.rs`, classified in the executor via
`cypherlite_ir::classify_aggregate`); any streaming seam must reuse that
vocabulary and the existing `StreamAgg` accumulator set rather than
re-spelling either.

## Decision

1. **Extend the `agg_fold` family with a slot-only whole-node value
   source.** The path fold gains a `PathItem` count / count-DISTINCT over a
   bare node slot (no property), feeding the shared
   `StreamAgg::offer_value` channel keyed on the endpoint's node id (the
   plain count increments unconditionally — a bound endpoint is non-null by
   construction — and the distinct count keys `Value::Int(id)` on the
   integer fast path), so `count(m)` / `count(DISTINCT m)` stream with zero
   row materialization. The diff is one new `CountSeat::Whole` variant in
   plan.rs's count vocabulary plus a slot-id read in `offer_aggs`
   (`agg_fold/path.rs`) — the smallest change that reaches the decline
   site, reusing the single accumulator vocabulary.
2. **The seam is a typed value-source enum at the fold's absorb point.**
   `AggSeat`'s `Option<(slot, prop)>` becomes plan.rs's
   `CountSeat::{Star, Prop { distinct, slot, prop }, Whole { distinct, slot }}`
   — the seat's value source IS the compiled count operand, carried from
   plan to runtime with no second source enum beside it, and `distinct`
   unrepresentable beside `Star` — so the per-binding feed
   `offer_aggs(&mut [(CountSeat, StreamAgg)], &[NodeId])` stays a pure
   function of the binding's node ids with no row intermediate, and
   `StreamAgg::count_of(distinct)` is the one distinct→prototype policy
   site.
3. **The seam is shaped for morsel-parallel scans but implements no
   parallelism.** Bindings are independent; the only shared state is the
   group map `IndexMap<Vec<GroupKey>, (Vec<Value>, Vec<(CountSeat, StreamAgg)>)>`; and
   every `StreamAgg` variant is mergeable (counts add, `SumAcc` adds,
   `DistinctAcc` unions). A later node can partition bindings into morsels
   that each fold into a thread-local group map and merge maps
   accumulator-wise — the walk, the seats, and the feed signature are
   unchanged. This decision only keeps the feed row-free and `StreamAgg`
   the single accumulator currency.
4. **(Radix-partitioned hash aggregation over [ADR-0468](./0468-soa-batch-substrate.md) Chunks) is
   not precluded as a downstream consumer.** The fold's grouping is already
   key → accumulators via `CachedKey`/`Vec<GroupKey>`, so a
   radix-partitioned hash table over chunk columns would replace only the
   grouping substrate while consuming the same `StreamAgg::offer_value`
   value channel per partition. Choosing the `agg_fold` extension
   deliberately avoids inventing a second accumulator set in the batch
   substrate, so LABS-685 adopts `StreamAgg` rather than competing with it.

## Alternatives considered

- **A general streaming aggregation operator on the [ADR-0468](./0468-soa-batch-substrate.md) batch
  substrate** — rejected: the batch chain is scan-only (compile gate
  `batch.rs:197`, one node, no traversal), so it cannot reach the 2-hop
  repro at all; growing traversal into the batch substrate first is far
  beyond this change's bar and would duplicate the accumulator set
  `StreamAgg` already owns.
- **The named hybrid — a `KernelMode::Aggregate` on the contracted kernels
  modeled on `KernelMode::Exists` (`kernels/mod.rs:105-112`)** — rejected:
  the kernel cost gate `MAX_ESTIMATED_STAGE_ROWS = 1<<20`
  (`kernels/mod.rs:344, 392-396`) declines ~30M-row stages, the
  exact repro, and engagement requires a single seed row and a non-optional
  match (`mod.rs:143`); lifting the cost gate is a separate, risk-laden
  change to the kernel cost model, not a streaming seam.

## Consequences

- `count(m)` / `count(DISTINCT m)` over a bound endpoint of a fixed
  non-optional pattern stream through the path fold with no row
  materialization; the LABS-780 repro's decline site is removed.
- The value-source enum is the single absorb point for fold values; later
  morsel-parallel work partitions bindings and merges group maps
  accumulator-wise without touching the walk, the seats, or the feed
  signature.
- `StreamAgg` remains the one accumulator currency across the fold and any
  future batch-substrate grouping (LABS-685); no second accumulator set is
  introduced.
