---
type: adr
area: engine
date: 2026-09-19
deciders: CypherLite maintainers
name: 0075-label-only-snapshot-census
status: accepted
tags:
  - storage
title: Label-only read for the snapshot label census
REFERENCES:
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0076-lock-free-interner]]"
  - "[[adr/0171-lazy-per-label-snapshot-probe]]"
---

# 0075. Label-only read for the snapshot label census

## Context

`ExecGraph::ensure_snapshot_label_n` builds, lazily per statement, a census
of how many snapshot-visible nodes carry each label. It is what
`+labels`/`-labels` accounting (`label_in_use`, `label_on_snapshot`,
`label_gain_deltas`) consults before a `CREATE` decides a label is new.

The census needs only labels, but its node walk called `Graph::node_at`,
which materializes the **full property map** (`LiveNode::to_node` ->
`CompactProps::to_map` -> an `Interner::get` per property key). Profiling the
in-memory fraud loadtest (`--ops 6000 --warmup 400 --workers 8`) put
`Interner::get` at **42.5% of self-time**, reached through exactly this path
(`ensure_snapshot_label_n` -> `node_at` -> `to_map` -> `Interner::get`).

The census is per-statement: it is rebuilt from scratch on the next
statement's first label check.

## Decision

Add `Graph::node_labels_at(id, ts) -> Option<Vec<String>>`, backed by
`LiveNode::label_strings` (label ids only), and use it in
`ensure_snapshot_label_n`. The census never materializes a property map.

This follows the existing precedent of `Graph::rel_ends_at` ([ADR-0033](./0033-compact-live-records-typed-csr.md)):
"expand / BFS must not materialize property maps" extends to any reader that
needs one facet of a record.

## Alternatives considered

- **Maintain a global label -> count index across statements.** Rejected:
  it adds a structure to keep correct under MVCC visibility and deletes,
  where the census already exists and is per-statement. Removal-first.
- **Cache the census across statements.** Rejected: invalidation would have
  to track every commit's label changes; the per-statement rebuild is
  cheaper than the bookkeeping, and correctness for free.
- **Leave it.** Rejected: it is 42.5% of self-time on the profiled workload,
  pure materialization of data the caller does not read.

## Consequences

- In-memory fraud loadtest: **16,043 -> ~21,300 ops/s (+33%)**;
  `Interner::get` self-time 42.5% -> 25.7%.
- Any future reader that needs only labels, or only endpoints, should use a
  facet accessor rather than `node_at`/`rel_at`. `node_at` is for the API
  boundary and full-record reads.
- `node_labels_at` returns owned `String`s; a later change could return
  interned ids to callers that only compare, if a hot caller appears.

## Later references

- [ADR-0076](./0076-lock-free-interner.md) removes the remaining `Interner::get` cost on this path.
- Measurements and method: [`docs/performance-notes.md`](../performance-notes.md).
- [ADR-0171](./0171-lazy-per-label-snapshot-probe.md) removes the census: `+labels` / `-labels` accounting probes `Graph::label_index` per touched label with early exit. It adds no global count and no cross-statement cache, so the alternatives rejected here stay rejected.
