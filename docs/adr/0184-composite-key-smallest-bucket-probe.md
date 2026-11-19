---
type: adr
area: engine
date: 2026-10-08
deciders: Oskar Hane (agreed in conversation), CypherLite maintainers
name: 0184-composite-key-smallest-bucket-probe
status: accepted
tags:
  - catalog
title: Composite uniqueness probes pick the smallest committed-plus-scratch bucket
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0171-lazy-per-label-snapshot-probe]]"
  - "[[adr/0172-incremental-constraint-validation]]"
  - "[[adr/0173-overlay-owned-redo-log-and-scratch-index]]"
  - "[[adr/0174-committed-catalog-constraint-rechecks]]"
  - "[[adr/0185-relationship-equality-postings]]"
---

# 0184. Composite uniqueness probes pick the smallest committed-plus-scratch bucket

## Context

[ADR-0172](./0172-incremental-constraint-validation.md) §3 probes a touched node of a `unique` or `key` constraint
through the lazy equality postings of the constraint's **first** property
(`ConstraintCheck::node_partners`,
`cypherlite/cypherlite-executor/src/exec/constraint_validation.rs:481-506`):
`Graph::equality_postings(L, p0, V[0])`
(`cypherlite/cypherlite-executor/src/graph/index.rs:102-110`) plus the
scratch's `prop_ix` seek
(`cypherlite/cypherlite-executor/src/overlay/prop_ix.rs:34-52`), then
re-verifies every candidate against the full tuple. LABS-155
([ADR-0171](./0171-lazy-per-label-snapshot-probe.md)..[ADR-0174](./0174-committed-catalog-constraint-rechecks.md)) removed the reported scans that way, but a
composite constraint inherits the cardinality of its first property: the
DDL property order is the only thing that decides the bucket size, and it
is invisible to the workload.

Measured on main `87bd55e5` (CypherLite v0.11.5, macOS arm64), with
`UNWIND range(1, N) AS i CREATE (:Step {a: i % 10, b: i})`:

| N | under NODE KEY `(a, b)` | the same data under NODE KEY `(b, a)` |
| --- | --- | --- |
| 16k | 5.4 s | 76.8 ms |
| 32k | 27.5 s | 167.5 ms |
| 64k | 145.6 s | 325.3 ms |

Steady-state single creates after the 64k load: 2.5 ms under `(a, b)`
against 0.4 ms under `(b, a)`. The first property `a` has ten distinct
values, so every touched node verifies a bucket of about N/10 candidates;
`b` is unique, so its bucket holds one. The same load is 448× slower by
declaration order alone, and its growth is quadratic in N.

Both structures the probe reads are lazy and per `(label, key)` pair:
committed equality postings ([ADR-0063](./0063-lazy-string-index-postings.md) contract, `Graph::prop_postings`,
`graph/index.rs:83-94`) and the scratch's `prop_ix`. Nothing about the
probe requires the first property; the tuple-verification step
([ADR-0172](./0172-incremental-constraint-validation.md) §3 step 3) re-reads the full tuple from the live view whichever
component produced the candidate.

## Decision

A uniqueness probe over a constraint with two or more properties chooses
its property per touched tuple:

1. For every property `p_i` of the constraint, compute the committed
   bucket length plus the scratch bucket length at `V[i]`: the size of
   `Graph::equality_postings(L, p_i, V[i])` plus the size of the
   `prop_ix` seek `(L, p_i, V[i])`.
2. Pick the `i` with the smallest sum; ties go to the lowest `i`.
3. Take candidates from that same `p_i` on **both** sides: the committed
   equality postings for `(L, p_i)` at `V[i]` and the scratch seek for
   `(L, p_i)` at `V[i]`. Union them and re-verify.

Both sides must name the same property because the two structures cover
different populations under one key. The committed postings hold every
committed version of every committed node under its committed value. The
scratch `prop_ix` is an additions-only log of the pairings the scratch's
writes introduced ([ADR-0173](./0173-overlay-owned-redo-log-and-scratch-index.md)): a scratch-created node is posted under
every property, but a committed node copied into the scratch by a `SET`
of another property is posted there only under the changed key. Suppose
the probe named `p_i` for the committed side but a different `p_j` for
the scratch side, and the statement changed `y.p_i` to `V[i]`, copying
`y` into the scratch: `y`'s committed posting for `p_i` carries its old
value, and its only scratch posting is under the changed key `p_i`, so a
scratch seek under `p_j` misses it and the conflict is missed. The union
at one `(p_i, V[i])` covers every partner:

- a committed-only node is found on the committed side, its live value
  being its committed value;
- a node the scratch changed on `p_i` is found on the scratch side, which
  posted the changed key;
- a node the scratch copied because of a change to another property is
  found on the committed side, which holds its unchanged `p_i` value;
- a scratch-created node is found on the scratch side, which posts every
  property at `CreateNode`.

A property whose value has no key (NaN) counts as an empty bucket on both
sides; a tuple with a NaN component equals nothing, so choosing that
property yields no candidates, which is the correct verdict.

Verification is [ADR-0172](./0172-incremental-constraint-validation.md) §3 step 3 unchanged: every candidate `y ≠ x` is
checked on the snapshot+scratch view for liveness, for the constraint's
label, and for `eq_non_null` on every component. Error selection is
[ADR-0172](./0172-incremental-constraint-validation.md) §6 unchanged: a conflict is still raised at the later id of the
pair, and the first constraint in name order that fails produces the
error. Only the bucket that supplies candidates changes.

A single-property constraint keeps today's probe: its only property is
the chosen one.

Cost: the length computation materializes the pair it counts, so the
first probe of a composite constraint pays the one-time lazy build of one
pair per constrained property (each O(|L|) once, [ADR-0063](./0063-lazy-string-index-postings.md)), where
[ADR-0172](./0172-incremental-constraint-validation.md) §3 built only the first. Every later probe pays the smallest
bucket's candidates plus dirty catch-up. Bucket lengths are superset
counts (stale versions included), so the choice is a heuristic;
correctness never depends on it.

## Alternatives considered

- **Keep the first-property probe.** It is the smallest change and needs
  no selection logic. Rejected: the measured 145.6 s (≈146 s) at 64k
  under `(a, b)` — against 325.3 ms under `(b, a)` — is the LABS-180 shape
  still alive behind a declaration-order accident; the first property is
  often the low-cardinality one, and the probe stays linear in its bucket
  and quadratic in the load.
- **Tuple-keyed postings per constraint.** One structure keyed by the
  whole `Vec<GroupKey>` per constraint, built lazily and dirty-marked
  like `prop_pairs`, gives exact probes with no candidate
  re-verification. Rejected: it is a new maintained structure and its
  memory per constraint, duplicating the superset, dirty-mark, GC and
  lock-ordering machinery the per-property lazy pairs already implement;
  the smallest-bucket choice over those pairs needs no new state.
- **Document a property-order rule for users.** Tell users to declare the
  selective property first, and re-declare constraints to fix existing
  ones. Rejected: an invisible performance contract — the 146 s to
  325 ms swing is decided by DDL order, not by data or queries; users
  cannot be expected to know the internal bucket shape, and every
  existing constraint would need a re-declaration to benefit.
- **Union the candidates from every property.** Probe all properties and
  merge their buckets, so no selection heuristic is needed. Rejected: it
  pays every property's bucket on every probe, re-introducing the
  low-cardinality bucket's cost that is exactly the residual being fixed;
  the smallest bucket alone is already a superset of the tuple-equal
  partners.

## Consequences

- Property order stops mattering. The `(a, b)` UNWIND and the
  steady-state single creates take the `(b, a)` path (325.3 ms at 64k,
  0.4 ms steady state in the measured shape); the reverse ordering stays
 where it is. [ADR-0172](./0172-incremental-constraint-validation.md)'s consequence "a composite constraint whose first
  property has low cardinality therefore degrades toward one label scan
  per touched entity" no longer applies.
- A composite constraint's first probe materializes one equality pair per
 constrained property — each a one-time O(|L|) build ([ADR-0063](./0063-lazy-string-index-postings.md)) — where
 [ADR-0172](./0172-incremental-constraint-validation.md) §3 materialized the first property's pair only. Later probes
  cost the smallest bucket plus dirty catch-up.
- The lowest-index tie rule keeps the choice deterministic, so probe
  behaviour and the `CONSTRAINT_INSPECTS` counter stay stable across runs
  and between the incremental validator and the differential oracle.
- Accept/reject decisions, error texts and error selection are unchanged
 ([ADR-0172](./0172-incremental-constraint-validation.md) §3 step 3, §6).
- The materialized pairs stay outside [ADR-0042](./0042-graph-memory-limit.md)'s logical charge, as
 [ADR-0172](./0172-incremental-constraint-validation.md)'s "Lazy postings" consequence records; charging them still
  needs a later ADR.
- [ADR-0185](./0185-relationship-equality-postings.md) applies the same rule to relationship tuples, choosing among
  the constraint's properties per this ADR.
