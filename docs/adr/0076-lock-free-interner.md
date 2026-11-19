---
type: adr
area: engine
date: 2026-09-19
deciders: CypherLite maintainers
name: 0076-lock-free-interner
status: accepted
tags:
  - catalog
  - perf
title: Interner id→string lookup is lock-free
REFERENCES:
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0075-label-only-snapshot-census]]"
---

# 0076. Interner id→string lookup is lock-free

## Context

The `Interner` maps labels, relationship types, and property keys to dense
`u32` ids ([ADR-0033](./0033-compact-live-records-typed-csr.md)). Reads are on the hot path: every materialized label,
property key, and node/rel type resolves through `Interner::get`.

Before this change the id→string table was `RwLock<Vec<Arc<str>>>`. After
[ADR-0075](./0075-label-only-snapshot-census.md) removed property-map materialization from the census, label
interning was still the largest self-time cost (**25.7%** of samples,
in-memory fraud loadtest), dominated by `to_str.read()` + `Arc::clone` per
key and the associated `__psynch_cvwait` on lock contention.

The id table is **append-only**: ids are minted monotonically, entries are
never removed or reassigned.

## Decision

Represent the id→string table as `DashMap<u32, Arc<str>>` (the same
structure as `to_id`), plus a dedicated `AtomicU32` id counter:

- `get(id)` is a single shard read: `to_str.get(&id).map(clone)`.
- `intern(s)` reserves its id from the counter and inserts `to_str` **before**
  the id becomes reachable via `to_id` (`entry` on `to_id` holds the shard
  write lock, so a concurrent first-sight intern of the same string takes
  the occupied arm and both callers agree on one id).

Id assignment order and publication order are the contract: a `get` for an
id already handed out always finds the string.

## Alternatives considered

- **Keep `RwLock<Vec>` and hoist the lock out of the per-key loop.** Rejected:
  it fixes the census call site only; every other materialization path keeps
  paying the lock. The append-only property makes the lock unnecessary
  everywhere.
- **Id = `to_str.len()` (no separate counter).** Rejected: two threads
  interning distinct new strings concurrently can observe the same length and
  overwrite each other. The atomic counter removes the race.
- **Keep the lock.** Rejected: 25.7% self-time for a lookup whose data is
  immutable-after-insert.

## Consequences

- In-memory fraud loadtest: ~21,300 -> **~28,200 ops/s** (with [ADR-0075](./0075-label-only-snapshot-census.md),
  **16,043 -> ~28,200, +76% total**). `Interner::get` self-time 25.7% ->
  3.6%.
- Both directions of the interner are now `DashMap`; the executor no longer
  holds a `RwLock` on this path.
- Regression pin: `intern_concurrent_ids_are_stable_and_unique` runs 8
  threads × 64 strings and asserts idempotence, no duplicate ids across
  distinct strings, and immediate reverse resolution.

## Later references

- Method and measurements: [`docs/performance-notes.md`](../performance-notes.md).
