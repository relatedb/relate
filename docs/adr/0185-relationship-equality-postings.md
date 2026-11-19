---
type: adr
area: engine
date: 2026-10-08
deciders: Oskar Hane, CypherLite maintainers
name: 0185-relationship-equality-postings
status: accepted
tags:
  - catalog
  - index
title: Lazy relationship equality postings for relationship uniqueness probes
REFERENCES:
  - "[[adr/0030-string-prefix-trigram-indexes]]"
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0171-lazy-per-label-snapshot-probe]]"
  - "[[adr/0172-incremental-constraint-validation]]"
  - "[[adr/0173-overlay-owned-redo-log-and-scratch-index]]"
  - "[[adr/0174-committed-catalog-constraint-rechecks]]"
  - "[[adr/0184-composite-key-smallest-bucket-probe]]"
---

# 0185. Lazy relationship equality postings for relationship uniqueness probes

## Context

[ADR-0172](./0172-incremental-constraint-validation.md) §4 gives relationship `unique` and `key` constraints a fallback
walk: no structure indexes relationship property values, so each
validation that touched a relationship of the type buckets the touched
tuples and makes one pass over every live relationship of the type
(`ConstraintCheck::rel_partners`,
`cypherlite/cypherlite-executor/src/exec/constraint_validation.rs:508-546`),
reading the constrained properties of each
(`ExecGraph::live_rel_ids_of_type`,
`cypherlite/cypherlite-executor/src/overlay/validation.rs:103-117`). That
is O(R) ids plus O(R_T·|P|) property reads per constraint per statement,
with one fast path: touching no relationship of the type with a complete,
NaN-free tuple.

This is the last residual after ([ADR-0171](./0171-lazy-per-label-snapshot-probe.md)..[ADR-0174](./0174-committed-catalog-constraint-rechecks.md))
removed the node-side scans. Measured on main `87bd55e5` (CypherLite
v0.11.5, macOS arm64), `:STEP` relationships preloaded, relationship
`UNIQUE` on `r.id`, one `MATCH (s:S) CREATE (s)-[:STEP {id:-1}]->(s)`:

| Preloaded `:STEP` rels | with UNIQUE | without |
| --- | --- | --- |
| 10k | 3.5 ms | 0.5 ms |
| 50k | 23.5 ms | 0.7 ms |
| 200k | 86.4 ms | 2.1 ms |

200 autocommit creates at 50k take 3.6 s against 0.27 s when the same
work is one UNWIND (load included). The cost grows with the type's size,
not with the touched tuple: the walk reads every `:STEP` relationship to
find the candidate equal to `id = -1`.

Node probes already use lazy committed equality postings plus a scratch
twin ([ADR-0172](./0172-incremental-constraint-validation.md) §3, [ADR-0063](./0063-lazy-string-index-postings.md) contract). [ADR-0172](./0172-incremental-constraint-validation.md)'s alternatives reserved
this change: "A later ADR can still add relationship equality postings if
a workload needs them."

## Decision

Relationships get the same lazy equality postings nodes have.

- **Committed postings.** `Graph` gains `rel_prop_pairs:
  DashMap<(u32, u32), RelPropPairIndex>`, keyed by `(relationship type
  id, property key id)` — the rel twin of `prop_pairs`
  (`graph/mod.rs:225-228`) with `built`,
  `postings: GroupKey → HashSet<RelId>` and `dirty`. A pair is built on
  its first probe by one pass over all relationships, holding **every
  version** of every relationship of the type that carries the key — the
 [ADR-0063](./0063-lazy-string-index-postings.md) superset contract: stale versions stay posted and the probe's
  residual re-verification filters them. `Graph::rel_equality_postings`
  and `ExecGraph::rel_equality_candidates` mirror `equality_postings`
  (`graph/index.rs:102-110`) and `equality_candidates`
  (`overlay/seek.rs:67-76`).
- **Dirty marking.** `install_rel` (`graph/mod.rs:1027-1070`) and
  `set_property_rel` (`graph/mod.rs:1306-1337`) dirty-mark a built pair
  only after the version is installed: `install_rel` after the version is
  pushed into `rels`, `set_property_rel` after `put_rel`, both before the
  commit is published. A pair nobody probes costs one missed hash probe
  per written property; an unbuilt pair is not marked, because its build
  walks the live versions.
- **Build/lock ordering.** The build holds the pair guard until it is
  marked built — the argument `prop_postings` records
  (`graph/index.rs:70-94`). A concurrent commit's `install_rel` waits on
  the pair guard, then finds `built` and dirty-marks; no relationship can
  slip into the gap between the build's candidate copy and `built = true`.
  The lock order (pair guard → `rels` / interner) and the dashmap shard
  reasoning are unchanged from the node side.
- **GC.** `gc_rel_chain` (`graph/index.rs:398-411`) prunes the collected
  versions' postings from built pairs, as `index_drop_unused`
  (`graph/index.rs:413-467`) does for nodes. A stale id that is never
  pruned is a performance leak, never a correctness one (superset).
- **Rebuild.** `rebuild_indexes` (`graph/index.rs:327-359`) clears the
  pairs alongside `prop_pairs`; the next probe rebuilds from the live
  graph.
- **Scratch side.** `WriteScratch` gains a relationship equality index,
  the twin of `overlay/prop_ix.rs`, posted only from
  `WriteScratch::push_redo` (`overlay/mod.rs:107-166`): on `CreateRel`,
  every property under the relationship's type; on `SetRelProp` with a
  non-null value, the changed key under the relationship's type (the
  scratch's `rels` copy holds it — the same pattern as `SetNodeProp`).
  Additions only; every seek re-verifies. `CALL { … } IN TRANSACTIONS`
  clones and restores it with the scratch.
- **Validation.** Relationship `unique`/`key` probes each touched tuple
 the way nodes are probed ([ADR-0172](./0172-incremental-constraint-validation.md) §3): for a touched relationship `x`
 with values `V`, choose the property per [ADR-0184](./0184-composite-key-smallest-bucket-probe.md) (smallest
  committed-plus-scratch bucket, ties to the lowest index), take
  candidates from that same property on both sides — the committed
  relationship postings and the scratch seek — and verify each `y ≠ x`
  live, of type T, with `eq_non_null` on every component. This replaces
 the [ADR-0172](./0172-incremental-constraint-validation.md) §4 walk (`rel_partners`). Error selection ([ADR-0172](./0172-incremental-constraint-validation.md) §6)
  is unchanged: the same `FirstFailure`/`conflict_position` logic
  consumes the partners.
- **Full re-validation is unchanged.** [ADR-0172](./0172-incremental-constraint-validation.md) §5's `recheck` still
  makes one pass over the live relationships of the type, bucketing
  tuples by key. It runs for `CREATE CONSTRAINT` and catalog drift
 ([ADR-0174](./0174-committed-catalog-constraint-rechecks.md)), not per statement, and stays O(R_T) there.

## Alternatives considered

- **Keep the [ADR-0172](./0172-incremental-constraint-validation.md) §4 walk.** It is the smallest change and needs no
  new state. Rejected: measured 86.4 ms (86 ms) per CREATE at 200k
  relationships (3.5/23.5/86.4 ms at 10k/50k/200k), and 200 autocommit
  creates at 50k take 3.6 s against 0.27 s as one UNWIND; the cost grows
  with the type's size, not with the touched tuple, so any
  relationship-unique workload stays O(R_T) per statement.
- **A relationship-by-type index.** Postings `type → rel ids`, so the
  walk no longer filters every relationship by type. Rejected: it still
  reads every relationship of the type per statement (O(R_T)); the
  measured cost is the per-relationship property reads, not the type
  filter.
- **Eager relationship property postings.** Maintain them on every
 `install_rel` / `set_property_rel`, [ADR-0030](./0030-string-prefix-trigram-indexes.md) style. Rejected: the write
 path would pay for pairs no probe ever reads — the exact tax [ADR-0063](./0063-lazy-string-index-postings.md)
  removed for nodes — while most stores have no relationship `unique` or
  `key` constraint at all.
- **A committed, versioned unique-value index per constraint.**
 [ADR-0172](./0172-incremental-constraint-validation.md)'s alternatives already considered it and deferred it: it needs
  MVCC versioning by commit ts for transaction snapshots, maintenance in
 apply, GC and checkpoint restore, and [ADR-0042](./0042-graph-memory-limit.md) accounting. Rejected:
 same reasons; the lazy pair is the narrower fix [ADR-0172](./0172-incremental-constraint-validation.md) anticipated.

## Consequences

- **Cost.** A validation probes each touched relationship tuple in
  O(candidates) — the chosen property's committed bucket plus its scratch
  bucket — instead of one O(R_T) walk per constraint per statement. The
  measured 86.4 ms single CREATE at 200k becomes the chosen bucket's
  candidate count (one, for a unique property).
- **Build.** The first probe of a `(type, key)` pair pays a one-time O(R)
  pass over all relationships; the pair lives for the process or until
  `rebuild_indexes` clears it. Later probes pay the dirty catch-up only.
- **Write path.** No cost for pairs never probed: `install_rel` and
  `set_property_rel` pay one missed hash probe per written property;
  built pairs are dirty-marked, unbuilt pairs are not.
- **Memory.** A built pair holds every version of every relationship of
  the type that carries the key. Like the node pairs, it sits outside
 [ADR-0042](./0042-graph-memory-limit.md)'s logical charge (`IndexCharge::from_details`); charging it
  needs a later ADR.
- **Correctness.** The postings are a superset ([ADR-0063](./0063-lazy-string-index-postings.md)); the probe
  re-verifies each candidate on the snapshot+scratch view, so
  accept/reject decisions, error texts and error selection are unchanged,
  and the differential oracle keeps passing. `gc_rel_chain` prunes
  collected versions so the superset does not grow forever.
- **Scratch.** The scratch-side index rides the same redo funnel
 ([ADR-0173](./0173-overlay-owned-redo-log-and-scratch-index.md)) as `prop_ix`, is additions-only, and is cloned and restored
  with the scratch by `CALL { … } IN TRANSACTIONS`.
- **Concurrency.** The pair guard is held across the build and the drain,
  and the dirty-mark happens after the version is installed — the
 ordering [ADR-0063](./0063-lazy-string-index-postings.md) established for nodes and `prop_postings`' lock
  argument pins for the committed side.
