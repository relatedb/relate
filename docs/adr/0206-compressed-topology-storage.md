---
type: adr
area: engine
name: 0206-compressed-topology-storage
status: proposed
tags:
  - storage
title: Compressed topology storage — conditional adopt
REFERENCES:
  - "[[adr/0035-ordered-expand-ts]]"
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0202-graph-floor-memory]]"
  - "[[adr/0203-newest-first-expansion-order]]"
  - "[[adr/0289-graph-shape-memory-pins]]"
---

# ADR-0206: Compressed topology storage — conditional adopt

## Context

CypherLite is an in-memory engine, so the adjacency store's footprint is
the product's core cost driver (memory cap and floor accounting, [ADR-0042](./0042-graph-memory-limit.md)
as amended by [ADR-0202](./0202-graph-floor-memory.md)). Today
each adjacency hop is an uncompressed 16-byte `AdjHop { ts: Option<i64>,
id: RelId }` inside per-(node, rel type, direction) `TypedAdj` chunks kept
in ts-DESC order ([ADR-0035](./0035-ordered-expand-ts.md), newest-first expansion [ADR-0203](./0203-newest-first-expansion-order.md)). The
topology-compression experiment measured, on Apple M4 Max (macOS/aarch64)
across 16 datasets (13 pulled graph-examples stores + 3 synthetic memory
shapes), what compressed adjacency would buy and cost: a baseline harness
(topology-twin logical bytes + live 1-hop expansion throughput), a scalar
codec prototype (two schemes, `for128` frame-of-reference bit-packing and
`hybrid` GDS-style zigzag-delta packed tail, both over a ts-presence
bitmap + delta-encoded ts gaps), and a NEON SIMD block decode variant.

ADR number note: 0204 and 0205 are reserved by pending PR #243
(0204-nightly-pull-retry-and-diagnostics.md,
0205-aggregations-skip-nulls-and-sum-durations.md), so this decision takes
0206.

Measured evidence (all numbers from the report above):

- **Memory:** compressed chunks save 95.89–99.31% of a dataset's
  topology-twin logical bytes (hybrid ≥ for128 on every dataset). That
  figure is against the whole twin (which includes per-node/per-chunk
  overhead), so the honest per-hop number is the raw codec ratio —
  raw hop bytes (16 B/hop) / encoded bytes — of **1.1x–12.7x** (movies
  2.14x/2.27x, twitch 3.42x/4.24x, shape-dense-ring 9.38x/12.70x). Both
  views are real: the end-to-end footprint win is the 96–99%, the pure
  codec win is the 1.3–14 B/hop.
- **Decode cost:** scalar decode runs at 35.9M–309.6M hops/s — **33x–173x**
  the live 1-hop expansion baseline (0.72M–1.80M hops/s). Decode is far off
  the query critical path.
- **SIMD:** the aarch64 NEON block decoder does **not** beat scalar —
  0.72–0.87x of scalar for for128 (one exception, legis-graph at 1.22x),
  0.91–1.07x for hybrid (movies the low outlier at 0.83x) — while still
  beating the live baseline 21x–187x. Scalar decode is sufficient; SIMD
  is not needed.

The prototype lives in `testing/cypherlite-datasets` ([ADR-0103](./0103-engine-surfaces-testing-layout.md) group
`testing/`): `src/codec.rs`, `src/bin/codec-prototype.rs`,
`src/bin/benchmark-harness.rs`, `src/bench.rs`. It never touches the live
store; hop streams are reconstructed from query results in the store's
exact `cmp_ts_desc` order and every chunk roundtrips exactly (asserted).

## Decision

**Conditional adopt.** Compressed committed adjacency chunks are a strong
memory win (95.89–99.31% of topology-twin bytes; raw ratio 1.1x–12.7x per
hop) with decode far off the critical path (33x–173x headroom over live
expansion), so we adopt the direction — but compression is **append-hostile**
(bit-packed blocks cannot absorb an inserted hop), so adoption is
conditional on two things:

1. **A transactional delta-store design** must exist first: compressed
   committed chunks plus a small uncompressed per-chunk write delta,
   merged at read time and compacted into the compressed form at
   checkpoint. Until that design is implemented and measured, the live
   store keeps uncompressed `AdjHop` chunks.
2. **The tiered-arena layout must be prototyped first** (one
   `Vec<AdjHop>` arena per node-direction + a `(type_id, offset, len)`
   index): it is strictly negative memory and touches the sorted-insert
   invariant, so it is the cheapest structural rehearsal of the layout
   changes compressed chunks would force.

**SIMD is explicitly not part of the recommendation** — the measured NEON
decoder fails to beat scalar (0.72–1.07x, one 1.22x outlier) because scalar
decode already outruns every consumer; the SIMD path stays
prototype-only.

The two scalar schemes remain as measured candidates, `hybrid` preferred
(smaller than `for128` on every dataset; tolerates unsorted ids).

Independent of compression, three access-pattern optimizations are
**adopted now** as follow-up work (they pay off on the uncompressed store
too): batch cursors for bulk expansion, id-only trails that skip
rel-stub materialization when no rel properties are accessed, and a
per-query visited-node bitset for var-length pruning.

## Alternatives considered

### Rel-type grouping within a node-direction

- **(a) Sort chunks by ascending cardinality — ADOPT NOW.** Zero extra
  memory and faster common filtered lookups. Rejected as *the* answer to
  compression because it does not shrink anything by itself; adopted as a
  free-standing win.
- **(b) Split high-degree rel-types into side structures — experiment
  later.** Helps skewed distributions. Rejected for now: adds a second
  lookup path and unproven benefit before tiered arenas exist.
- **(c) Tiered arena layout — experiment later (precondition of this
  ADR).** Strictly negative memory but touches the sorted-insert
  invariant; rejected as an immediate change because it must be measured
  before compressed chunks stack on top of it.
- **(d) Neo4j-style chained group records — REJECTED.** Chains and
  pointers add memory in an engine whose value proposition is in-memory
  footprint.

### CSR vs alternatives for the primary adjacency store

- **Batch-built CSR — REJECTED as primary store.** ts-DESC pushdown
 ([ADR-0035](./0035-ordered-expand-ts.md)), snapshot visibility, and the per-type split are load-bearing,
  and a full rebuild fights the single-writer redo path. Worth revisiting
  only as a checkpoint-rebuilt read-only acceleration structure.
- **Tiered CSR + mutable delta region (LLAMA/GraphChi-style, folded at
  checkpoint) — experiment later.** Structurally the best answer to
  append-hostility; rejected for now because it subsumes the tiered-arena
  prototype this ADR requires first.
- **Per-node HashMap adjacency — REJECTED.** Per-node HashMap overhead is
  exactly the memory this experiment is trying to remove.
- **Arena-packed `TypedAdj` — experiment later (first step).** Cheapest
  move toward tiered CSR; rejected as immediate because it should be
  measured together with the tiered-arena prototype.

### Access-pattern optimizations

- **Batch cursors for bulk expansion — ADOPT NOW.** Removes per-hop
  iterator overhead for scans and var-length; independent of compression.
- **Id-only trails (skip rel-stub materialization when no rel properties
  are accessed) — ADOPT NOW.** Pure win, independent of compression.
- **Per-query visited-node bitset for var-length pruning — ADOPT NOW.**
  Bounded per-query memory, independent of compression.
- **Storing the target `NodeId` inside `AdjHop` — REJECTED.** Pure
  memory-for-speed trade that contradicts the memory goal.
- **Early-termination BFS for `SHORTEST` — experiment later.** No measured
  workload yet justifying the added expansion state.
- **Sorted-by-target runs for intersection/triangles — rejected for now.**
 Conflicts with the ts-DESC invariant ([ADR-0035](./0035-ordered-expand-ts.md)) and no measured workload
  demands it.

### Do nothing (keep uncompressed `AdjHop` forever)

Rejected: the 95.89–99.31% topology-twin savings (1.3–14 B/hop raw) is the
largest single lever on the engine's defining cost, and decode headroom
(33x–173x over live expansion) shows the read path can absorb it.

### Adopt compressed chunks unconditionally, now

Rejected: compression is append-hostile; without the delta-store design
every write would either rewrite a chunk's bit-packed blocks or decompress
on write — a regression on the single-writer redo path and the WAL/snapshot
roundtrip.

## Consequences

- The engine store (`cypherlite-executor`) stays uncompressed until the
  two preconditions land: the transactional delta-store design (compressed
  committed chunks + per-chunk write delta merged at read, compacted at
  checkpoint) and a measured tiered-arena prototype.
- The prototype tooling remains in `testing/cypherlite-datasets` as the
  reproducible measurement harness for both preconditions; the SIMD decode
  path stays there, out of the engine.
- The three adopt-now access-pattern optimizations (batch cursors, id-only
  trails, visited-node bitset) proceed as independent follow-ups, each
  with its own tests and measurements.
- Chunk-order semantics are unchanged: compressed decode re-emits hops in
 the original ts-DESC order ([ADR-0035](./0035-ordered-expand-ts.md) / [ADR-0203](./0203-newest-first-expansion-order.md) newest-first expansion is
  preserved byte-identically).
- Any future re-measurement (e.g. in the [ADR-0289](./0289-graph-shape-memory-pins.md) memory-shape baseline
  environment for gate-comparable numbers) lands beside this ADR.
