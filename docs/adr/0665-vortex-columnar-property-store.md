---
type: adr
area: engine
claim: 6b343959c1b56fc36abea6d22e0ccff10046f6c6
date: 2026-11-23
deciders: the maintainers
name: 0665-vortex-columnar-property-store
status: proposed
tags:
  - storage
title: "Vortex columnar property store: node-major nullable columns behind the existing seams, an experimental codec option"
REFERENCES:
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0050-snapshot-codecs]]"
  - "[[adr/0072-virtual-filesystem]]"
  - "[[adr/0105-optional-parquet-codec]]"
  - "[[adr/0534-wide-column-parquet-shards]]"
  - "[[adr/0614-equality-match-lazy-postings-serve]]"
---

# 0665: Vortex columnar property store — node-major nullable columns behind the existing seams, an experimental codec option

## Context

Node properties live today as per-version compact lists
([ADR-0033](./0033-compact-live-records-typed-csr.md)) inside the live
graph, with equality lookups served by lazy postings
([ADR-0614](./0614-equality-match-lazy-postings-serve.md)); the checkpoint
already promotes hot keys to typed Parquet columns
([ADR-0534](./0534-wide-column-parquet-shards.md), made the only Parquet
encoding by [ADR-0553](./0553-wide-columns-are-the-parquet-checkpoint-format.md)).
That checkpoint format exists to make checkpoints small — the live store
still materializes every property as a per-node list.

The question this ADR proposes to answer experimentally: can the
**checkpoint file and the in-memory store be the same object** — a
columnar store where row index == node id, promoted per-key columns are
decoded on demand, and the property paths the query engine exercises
(point reads, key seeks, range filters, batched morsel checks, joins,
group-bys) get faster or at least neutral, at lower memory?

Round-1 lab measurements (Vortex 0.93, LDBC SF0.1 + synthetic shapes,
allocation-counted; the lab and its results travel with the experiment):

- **Size**: property tables compress to 32–54% of raw value bytes
  (Comment 41.6%, Post 32.1%, Forum 53.7%). The win concentrates in
  dictionary columns (low-cardinality strings 0.05–0.38 B/value); free
  text compresses only ~1.5–2×; dates-as-strings are the worst column and
  a typed temporal column is an open win.
- **Seek and join**: binary seek on a sorted key column is ~780× a full
  scan; a sort-merge join over two key-sorted sides is 23× a hash join and
  allocation-free.
- **Range pruning**: with clustered (monotone) data, zone-map pruning
  skips 3/5 storage splits for a 1/3-range predicate (1.85× faster,
  −19% allocations); pruned splits under conjunction are the **union** of
  the conjuncts' pruned sets.
- **Traversal state**: dense bitsets are 21.6× smaller than hash sets at
  1M visited nodes (0.125 B/node) with alloc-free frontier algebra.
- **Hard limits in the format** (all tracked upstream): no
  arbitrary-row-id batched gather (scattered selections degrade to
  decode-and-mask; the honest primitive is a contiguous 1024-row range —
  which the row==node-id layout provides for id-ordered morsels); no
  exists early-exit (`limit(1)` costs ~98% of a full count); statistics
  are discoverable only at footer/zone level, not on decoded arrays.

## Decision (proposed — an experiment, not an adoption)

Evaluate the Vortex-backed store as an **experimental snapshot codec
option** in the shape of the existing optional codecs
([ADR-0105](./0105-optional-parquet-codec.md)), following
[ADR-0534](./0534-wide-column-parquet-shards.md)'s precedent — column
files inside a codec, not a new flag — and **loading through the existing
VFS** ([ADR-0072](./0072-virtual-filesystem.md)) only: no new I/O
mechanism is introduced together with the new store. The store's shape:

1. **Node-major table, row index == node id.** The executor's existing
   node-id binding is the row handle; no second handle type.
2. **Per-key nullable columns, added as the workload earns them.** Null
   means absent (the engine's lenient presence semantics — no sentinel
   values); a mostly-absent key costs ~its present-ids plus values; the
   long tail is a typed `list<struct{key,value}>` residual column, not
   JSON. Promotion is a read-shape dial, not a memory dial.
3. **A sorted (key, row) side-file** for equality seeks and allocation-free
   sort-merge joins — the natural replacement for the
   [ADR-0614](./0614-equality-match-lazy-postings-serve.md) postings.
4. **Immutable files are the checkpoints.** The WAL stays the append log;
   the live tier stays the in-memory overlay
   ([ADR-0016](./0016-write-overlay.md)'s shape); the checkpoint fold
   rewrites only the column files whose keys changed. Per-column files
   make add-a-column and compaction incremental, at a measured cost: each
   file's footer parse is ~1.7k allocations, so a per-column layout opens
   lazily per touched column. Where mapping is eventually permitted,
   untouched segments never fault — a one-column-of-six read resident-grows
   42× less than the whole file — so per-query memory tracks touched
   segments, not the store.
5. **Store additions are shaped like the upstream proposed APIs** (a
   probe-shaped morsel seam carrying a density hint; a membership-mask
   batched key check; a limit-shaped exists terminal), so landing upstream
   implementations is a delegation change, not a redesign.

Seam verdicts from the round-1 research (full detail in the experiment's
seam findings):

- **VFS** ([ADR-0072](./0072-virtual-filesystem.md)): fits unchanged — a
  Vortex store reads its bytes through `Vfs`/`VfsFile`; the trait is
  byte-copy, so zero-copy decode is out of scope for the experiment.
- **Codec seam** ([ADR-0050](./0050-snapshot-codecs.md)): fits — a new
  codec kind is an additive `SnapshotCodecKind` + `CodecOps` + evidence
  row; the Parquet codec already publishes immutable per-column files with
  add-column-at-checkpoint. Two extensions if the experiment graduates:
  `dump` currently sees the graph only as materialized rows, and
  dirtiness is exported as ids only — per-key dirty statistics are the
  input per-column compaction needs.
- **Property read path**: the one seam that does not exist — property
  reads are concrete (`CompactProps` inside live records inside MVCC
  chains, no trait between executor and storage). The experiment's
  engine-side work is a property-read trait at the point-read /
  scan-predicate boundary, through which the overlay reads, the restore
  path installs, and the charge walk
  ([ADR-0042](./0042-graph-memory-limit.md)) charges.
- **Memory**: the store charges logical (uncompressed-size-style) bytes to
  the [ADR-0042](./0042-graph-memory-limit.md) `used` counter; whether
  mmap'd resident pages count toward anything is deliberately unanswered
  here and needs its own decision record if the store adopts mapping.

## Alternatives considered

- **Extend the wide Parquet checkpoint instead.** The codec machinery
  exists; rejected for this round because the question under test is the
  in-memory decode/compression interplay (dictionary + zone-map pruning +
  scan kernels serving query operators), which the Parquet checkpoint does
  not provide. It remains the fallback if the engine-side A/B is neutral.
- **Keep [ADR-0033](./0033-compact-live-records-typed-csr.md) records
  unchanged.** The default: the property-read trait makes both layouts
  swappable, and the Vortex store must win the measured A/B against the
  current records + postings before anything changes for users.
- **Other columnar engines.** Split to a separate follow-up evaluation;
  not blocking this decision.

## Consequences

- Experimental-only: feature-gated, off by default, no user-facing
  behavior change until the engine-side A/B on the query-latency suite
  decides adoption.
- The lab requires a newer Rust toolchain than the repo pin
  (Vortex 0.93 needs 1.95; the pin is 1.88). The experiment is lab-gated
  accordingly; merging any engine-side code requires either an MSRV
  decision or pinning an older Vortex — recorded here as the known
  blocker, not silently worked around.
- The property-read trait is engine work independent of Vortex: it lands
  or dies on its own review, and any other backing store can implement it.
- If the experiment graduates, per-key dirty statistics in the export
  surface and an mmap-accounting decision record follow as their own
  changes.
