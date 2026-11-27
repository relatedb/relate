---
type: adr
area: engine
claim: da9a915e660f2ed5a039bb4c70c7a2dd384449df
name: 0705-vector-search-packed-matrix
status: proposed
tags:
  - perf
  - search
title: Vector SEARCH packed f32 matrix — a lazily built, incrementally refreshed cache per node index
REFERENCES:
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0256-vector-search-flat-scan-rewrite]]"
---

# 0705: Vector SEARCH packed f32 matrix — a lazily built, incrementally refreshed cache per node index

## Context

[ADR-0256](./0256-vector-search-flat-scan-rewrite.md) rewrote the vector
flat scan and recorded the residual cost — "the per-row `Value`-coordinate
walk and property lookup" — and deferred the remedy: a packed matrix cached
per `(labels, property)`, needing dirty-marking on writes, rebuild on open,
memory accounting, and the f64-accumulated, f32-rounded score contract.

That residual dominates embedded workloads that search on every request
(agent memory is the motivating one). Measured on a release build, one
process, 4 cores, 10,000 nodes × 384-dimension FLOAT32 vectors:

| Store | Search (no filter) | Search (`WITH` filter) |
| --- | --- | --- |
| `:memory:`, nodes carry only id, filter key, vector | 19 ms | 20 ms |
| directory store, nodes carry 10 properties | 38 ms | 36 ms |

The cost grows with the number of properties on a node, not with the
vector math. Each candidate pays: a label-index sweep and sort over every
carrier, a `graph.node(id)` read that rebuilds the node's full property map
(`CompactProps::to_map`, a `String` per key), the vector property's lookup
by name, and a walk over 384 tagged `Value` coordinates (48 bytes each)
that the compiler cannot vectorize.

Agent-memory stores also write constantly to the nodes they search — an
access counter, a last-used timestamp — so a cache that rebuilds whenever
the label sees a write would rebuild on almost every search.

## Decision

Add a **vector matrix**: per `(label id, property key id)` of a node vector
index, the visible carriers' vectors packed as contiguous `f32` rows, kept
on `Graph`'s `Indexes` beside the lazy postings
([ADR-0063](./0063-lazy-string-index-postings.md)).

1. **Built lazily.** The first vector SEARCH over the pair builds the
   matrix from the label's carriers visible at the reader's snapshot. A
   pair nobody searches costs nothing. `Indexes::clear` drops every
   matrix, so open, checkpoint restore and an aborted write rebuild on the
   next search — the lazy postings' lifecycle.
2. **Refreshed incrementally, by commit timestamp.** The node write hooks
   that already maintain the label and declared postings
   (`Graph::index_add`, `Graph::suspect_postings`) record each written
   carrier in the matrix's dirty map, stamped with the writing commit's
   timestamp (the oldest and newest unapplied ones). A reader refreshes
   only the rows whose writes its snapshot can see and keeps newer stamps
   for later readers. A write to another property of a carrier costs one
   row refresh, not a rebuild.
3. **Served only when exact for this reader.** A reader at snapshot `ts`
   uses the matrix when the matrix was last brought current at or before
   `ts` and no unapplied write is stamped at or before `ts`. Rows are
   never rewound: a reader whose snapshot is older than the matrix takes
   the `Value` scan ([ADR-0018](./0018-concurrent-snapshot-reads.md)
   snapshots stay exact). A **first build** also declines while the
   store's head is past the reader's snapshot: a write that landed before
   the slot was registered was never dirty-marked, so a build behind the
   head could publish rows stale for every later reader — the reader
   takes the `Value` scan and a reader at the head builds fresh. The
   matrix also declines a view with uncommitted writes of its own (it
   reads them through the `Value` scan), a relationship index, a
   multi-label index, and a query whose length differs from the rows'.
4. **Exact coordinates only.** A carrier is packed only when every
   coordinate survives the round trip through `f32` (FLOAT32, FLOAT16,
   BFLOAT16, INT8, INT16, and lists of such values). While any carrier
   holds a coordinate that does not (FLOAT64, large INT32/INT64), the
   matrix declines and the `Value` scan answers. Carriers the `Value` scan
   skips (wrong length, a non-number or non-finite coordinate, no
   property) are not packed.
5. **Scores bit-identical.** `similarity_row` widens each `f32` to the
   same `f64` the `Value` walk reads and keeps the eight-lane
   accumulation, the tail, the all-zero guard and the `f32` rounding chain
   of the ADR-0256 kernels.
6. **Filters keep their pre-ranking semantics.** A row is scored first;
   only a row the top-k window would retain (`TopWindow::admits`, the
   push's own gate) has its node read and its `WITH` filters checked. A
   failing row never enters the window, so the passing rows fill it — the
   `Value` scan's answer — without reading the properties of rows that
   could not place.

## Alternatives considered

- **Rebuild on any write to the label (commit-keyed cache, the
  `ContainsMemo` shape).** Rejected: an access counter written after every
  search would rebuild the matrix on every search.
- **Keep every version's row and filter by visibility per row (the
  postings' superset contract).** Rejected for now: a visibility check per
  row brings back a per-candidate version-chain read, and readers at older
  snapshots are rare enough to take the `Value` scan.
- **Pack vectors in the value model itself** (`Value::Vector` holding a
  typed buffer instead of `Vec<Value>`). Not rejected, but a separate
  decision: it changes every surface that matches on vector coordinates
  and the WAL and snapshot encodings, and it addresses memory and open
  time as well as scan time. This cache is independent of it and keeps
  the matrix's benefit if it lands.
- **Accumulate in `f32`.** Rejected: it changes user-visible scores
  (the ADR-0256 contract).
- **An approximate index.** Rejected again for the reasons ADR-0256
  records; the matrix keeps the scan exact.

## Consequences

- Unfiltered and filtered vector SEARCH at 10,000 × 384 drops from 19–38 ms
  to about 2.4 ms (8–16×) on the measurements above, independent of the
  number of other properties on the nodes.
- `db.index.vector.queryNodes` takes the same path (it calls the same scan).
- Memory: `4 × dimensions` bytes per packed carrier plus its id, held
  outside the logical graph charge (ADR-0042), like the lazy postings and
  key statistics it sits beside. Folding derived caches into the charge is
  a separate question for all of them.
- Relationship vector indexes, multi-label indexes, and readers with
  uncommitted writes keep today's scan unchanged.
- A `test-hooks` seam (`take_vector_matrix_serves`) lets the facade's
  differential test prove the matrix side ran; production builds carry no
  counter.

## Verification

- Unit: the packed-row kernels match the `Value` kernels bit-for-bit across
  lengths 1–1536, four scales, both metrics and the all-zero row; packing
  mirrors the scan's skip and exactness rules; the dirty map drains only up
  to the reader's snapshot.
- Integration (`vector_matrix_differential`): a seeded walk of 150 writes —
  creates, vector rewrites, unrelated property writes, label drops and
  re-adds, property removals, wrong-length and FLOAT64 vectors, deletes,
  committed multi-write transactions — compares every filtered and
  unfiltered answer (ids, order, score bits) with the `Value` scan at the
  same snapshot, and asserts the matrix served the searches; a reader
  pinned before a rewrite keeps its own answer after a newer reader has
  refreshed the matrix. A third test pins a reader before the first
  search, commits a carrier behind its back, and checks that the pinned
  search declines to build while the fresh reader's matrix carries the
  new carrier — the first-build staleness regression. Removing the row
  refresh, or the first-build decline, fails the tests.
- The existing vector, `SEARCH` and CIP-eval suites pass unchanged.
