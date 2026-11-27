---
type: adr
area: engine
claim: e827f96cebdee104b528f5508c3a653372d06578
date: 2026-11-24
deciders: the maintainers
name: 0673-mmap-store-memory-accounting
status: proposed
tags:
  - memory
  - storage
title: "Mapped-store memory accounting: logical bytes in the cap, resident pages outside it"
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0072-virtual-filesystem]]"
  - "[[adr/0526-memory-controls-gate]]"
  - "[[adr/0665-vortex-columnar-property-store]]"
---

# 0673: Mapped-store memory accounting — logical bytes in the cap, resident pages outside it

## Context

[ADR-0665](./0665-vortex-columnar-property-store.md) proposes the Vortex
columnar store as an experimental codec and deliberately leaves one
accounting question open: the store charges logical (uncompressed-size-style)
bytes to the [ADR-0042](./0042-graph-memory-limit.md) `used` counter, but
whether mmap'd resident pages count toward any cap is unanswered there and
"needs its own decision record if the store adopts mapping". This is that
record. The store reads through the existing VFS
([ADR-0072](./0072-virtual-filesystem.md)); mapping is an I/O backend for
the sealed files, not a new storage mechanism.

The lab's second round measured the question (Vortex 0.93, the lab and its
results travel with the experiment; one load-stamped run).

Table 1 — mmap resident accounting, per shape:

| shape | rows | file bytes | `UncompressedSizeInBytes` | full-touch RSS | partial-touch RSS |
| --- | --- | --- | --- | --- | --- |
| synthetic-flat-150k | 150,000 | 1,546,320 | 10,350,000 | 672 KB | 48 KB |
| synthetic-mono-150k | 150,000 | 2,326,880 | 10,350,000 | 256 KB | 16 KB |
| pokec-100k | 100,000 | 182,672 | 4,000,000 | 48 KB | 16 KB |

Table 2 — decoded-chunk cache arms (attr0, 150,000 rows, 3 chunks): the
whole-column decode + `Vec` misses at 126,710.6 ns/op, hits at 2.5 ns/op,
and retains 1,200,000 B; the per-chunk `repeated_probe` misses at 115.1
ns/op (per chunk), hits at 142.2 ns/op, and retains 576 B. A full 3-chunk
sweep hits at 426.7 ns against a 345.3 ns miss.

Table 3 — density-hint cache size (attr0, 150,000 rows, column 1,200,000 B):
at 1%, 10% and 100% density the touched set is 3/3 chunks and the retained
cache is 1,200,000 B, against an ideal `d × column` of 12,000, 120,000 and
1,200,000 B.

## Decision

1. **Charge the logical uncompressed size to the [ADR-0042](./0042-graph-memory-limit.md) `used` counter.**
   The store's charge is its logical size — `UncompressedSizeInBytes`
   summed over the arrays it holds — entered exactly as the row-oriented
   store enters `Value::logical_bytes`. The cap keeps meaning "the customer
   data the store can decode into memory"; a compressed file size would
   understate that (table 1: a 1.55 MB file decodes to 10.35 MB), and a
   resident-page count is what a query has read, not what the store holds.

2. **Mapped resident pages stay out of any cap — the Parquet/object-store-buffer precedent.**
   [ADR-0042](./0042-graph-memory-limit.md)'s out-of-budget list already
   excludes "Parquet / object-store buffers" and states process RSS is the
   wrong meter; a mapped column file is the same class of buffer. That the
   mapped store *is* the live store is why the carve-out needed its own
   record, not why it should change. Table 1 is the measurement: on files
   of 0.18–2.33 MB whose logical size is 4–10.35 MB, a full touch faults
   only 48–672 KB and a one-column partial touch 16–48 KB. The resident set
   tracks pages read, not the store, so a cap over it would bound a query's
   working set rather than the stored data — and would double-charge against
   the process RSS / cgroup limit the operator already sets at the process
   boundary. Page residency is the operating system's page cache, not a
   structure the engine allocates and can meter.

3. **A retained decode cache, if the store keeps one, is charged at its live retained bytes.**
   The design is per-chunk with a density hint. Table 2 upper-bounds the
   arms: a whole-column decode + `Vec` retains 1,200,000 B where the
   per-chunk `repeated_probe` retains 576 B for the same column, and hits
   at 2.5 ns against 142.2 ns — a latency-for-memory trade the accounting
   does not have to pick. Table 3 is why the charge is the retained figure
   and not the `d × column` ideal: the touched set is chunk-granular, so at
   1% density all 3/3 chunks are held (1,200,000 B against a 12,000 B ideal),
   and charging the ideal would understate what the process holds by up to
   100× at low density.

## Alternatives considered

- **Charge the resident RSS or the mapped pages.** Rejected: the resident
  set is page-granular (table 1: 16–672 KB) and moves with what a query has
  read, so it caps the working set, not the store; the engine cannot read
  page residency deterministically, and the process RSS / cgroup already
  caps it at the boundary that owns it.
- **Charge the compressed file size.** Rejected: decoding expands the file
  (table 1: 1.55 MB → 10.35 MB), so a file-size charge would let a store
  decode past the graph cap while the meter reports a smaller number.
- **Charge the `d × column` ideal for a density-hinted cache.** Rejected:
  the touched set is chunk-granular, so at 1% density all three chunks are
  retained (table 3); the ideal (12,000 B) understates the 1,200,000 B
  actually held 100×.
- **Keep the whole-column decode + `Vec` as the cache.** Rejected: it
  retains 1,200,000 B against the per-chunk probe's 576 B for the same
  column (table 2), and the probe's per-chunk hit stays in the hundreds of
  nanoseconds; the whole-column arm spends memory it does not need for the
  design's read shape.
- **Leave the question to each codec.** Rejected: [ADR-0042](./0042-graph-memory-limit.md)
  is one customer-data ceiling across layouts; a second meter per backing
  store would report two caps for one graph.

## Consequences

- The columnar store enters [ADR-0042](./0042-graph-memory-limit.md)'s
  `used` with one figure — its logical decoded size — so the customer-data
  ceiling and `memory_usage()` keep their meaning across both layouts; there
  is no second meter.
- Mapped pages and the page cache never refuse a write: a store can be
  fully mapped and still admit within the logical cap, exactly as the
  Parquet checkpoint's buffers do today.
- The gap between the logical charge and the resident pages shows up where
  the repo already watches resident memory per live logical byte — the
  `memory-shape` and `memory-controls` lanes
  ([ADR-0526](./0526-memory-controls-gate.md)) — as a property of the
  backing store, not as a per-store cap.
- This is an accounting contract for the experimental store; it changes no
  user-facing behaviour and no code until the store graduates, and the
  property-read trait's charge walk follows it from the start.
- [ADR-0665](./0665-vortex-columnar-property-store.md) carries a Later
  references note pointing here.
