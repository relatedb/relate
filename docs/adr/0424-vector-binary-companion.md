---
type: adr
area: engine
claim: 51ef0840692a8347043affd20595e8f6392c5bd9
name: 0424-vector-binary-companion
status: proposed
tags:
  - search
  - values
title: Vector values carry a derived binary companion for exact pruning
REFERENCES:
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0256-vector-search-flat-scan-rewrite]]"
---

# ADR-0424: Vector values carry a derived binary companion for exact pruning

## Context

[ADR-0256](./0256-vector-search-flat-scan-rewrite.md) landed the exact flat vector search (SIMD scan, bounded k-heap)
and explicitly deferred the packed matrix cache as its own decision.
LABS-442 collected the PRISM/pg_vectorsearch technique set and asked
which parts improve the flat scan without a physical or durable index.
The measured evidence (the vector-bench campaign, preserved at
`~/d/llm/vector-bench`) shows a binary-quantized prefilter at 0.7 ms vs
12.6 ms flat on the 90k x 1536 real corpus (18x) with 0.982 recall at
ef=100 and 0.998 at ef=500 — but every published shape of that prefilter
stores the quantized codes somewhere a scan has to trust, which is a
structure somebody has to keep up to date.

The constraint decided in conversation (2026-10-25): no scan-side
in-memory structure to maintain or invalidate — yet. The accepted
insight: the structure problem disappears if the binary code is **part
of the value it derives from**. A vector property's value can carry its
own binary twin, computed once at construction and carried inside the
value: it is born with the vector, dies with it, and updates atomically
with it. Nothing scan-side ever invalidates, because there is nothing
scan-side.

`Value::Vector` already exists
(`cypherlite-core/src/value/mod.rs`: `Vector { coord: VectorCoordKind,
values: Arc<Vec<Value>> }`) with construction sites across the model,
wire, serde, convert and parameter paths. The project is pre-publish:
value-type changes land without migration of stored databases.

## Decision

1. **Extend `Value::Vector` with a derived binary companion**: the
   coordinate signs as `ceil(d / 64)` `u64` words (`bits: Arc<[u64]>`,
   96 bytes at 768 dims — 3% of an f32 coordinate list) plus the
   per-vector quantization error factor RaBitQ requires (`err`, one
   `f32`). Both are pure functions of `coord` and `values`.

2. **One canonical constructor derives them.** A single
   `Value::vector(coord, values)` constructor computes the companion;
   every existing construction site (wire decode, serde, convert,
   parameters, examples) calls it. No site may build a `Vector` literal
   with hand-computed fields — the companion cannot desync from the
   coordinates because it is derived once, where the value is born.

3. **The scan consumes the companion as a pruning bound, not an
   approximation** (RaBitQ, arXiv 2405.12497): XOR + POPCNT Hamming
   distance over the `u64` words gives each candidate a provable
   distance bound; a candidate whose bound cannot beat the current
   k-th best is skipped; survivors are rescored by the existing exact
 SIMD path ([ADR-0256](./0256-vector-search-flat-scan-rewrite.md), unchanged). The result is **exact top-k with
   identical semantics to today** — pruning only removes candidates
   that provably cannot place. The code order is plain per-vector signs
   first; a per-scope anchor (RaBitQ residual anchoring, LABS-442
   item 2) tightens the bound later without changing this contract.

4. **Serialization follows the value** (pre-publish, no migration): the
   WAL and checkpoint encodings of the vector value carry the companion
   as part of its tagged form, and decoders rebuild through the
   canonical constructor (a decoder may also discard and re-derive —
   the companion is a function of the coordinates, so round-tripping
   through coordinates alone is always correct).

5. **Parallel scan is deferred** — recorded here so the deferral and
   its reopening condition are on the ledger: with the bits in the
   value, the prefilter costs ~96 bytes of XOR/POPCNT per candidate,
   so parallelism only pays above roughly a million vectors. When it
   does, the pattern is a static partition at ~n_threads chunks
   (uniform per-candidate work, no work-stealing; rayon stays excluded
   by R1/R6), per-chunk local heaps, one k-way merge, and a size
   threshold below which the sequential path runs unchanged
   (spawn+merge is tens of microseconds, so the break-even is roughly
   when the sequential scan would exceed ~1 ms). The scoped-thread
   precedent lives in storage (`open.rs`, `checkpoint.rs`, the Parquet
   codec); the executor is single-threaded today, so the first
   parallel execution path gets its own ADR note before landing.

## Alternatives considered

- **The lazy packed matrix cache (original item 1, [ADR-0063](./0063-lazy-string-index-postings.md)
  postings-style memoization)** — rejected by decision: even with
  dirty-bit invalidation it is a scan-side structure to keep up to
  date; the value-embedded companion delivers the same packing with
  zero management.
- **Scan-side lazy quantization per query** — rejected: computing the
  codes during the scan costs more than they save (each query would
  re-quantize the corpus).
- **A property-store decoration beside the value** — rejected: parallel
  state beside the value it derives from (the code-shape rule); the
  companion belongs inside the value.
- **Approximate-only search (ef re-rank as the only mode)** — rejected
  as the default: exact top-k is the shipped semantic; an opt-in
  approximate mode calibrated by the bench numbers (0.982 @ ef=100,
  0.998 @ ef=500) may follow as a knob on top of the same companion.
- **IVF routing, SOAR, partition machinery** — excluded by LABS-442's
  scope decision (physical/durable index structures).

## Consequences

- Vector values cost ~3% more memory (f32 coordinates) and one O(d)
  pass at construction (CREATE/SET of a vector property, parameter
  decode, wire decode) — amortized where the value is born, not in the
  scan.
- Every `Value::Vector` construction site (the model, wire, serde,
  convert, parameter, and example paths — ~10 files) routes through
  the canonical constructor; the TCK floor is untouched (the scan's
  results are identical, pruning provably removes only losers).
- The value's equality and hashing compare the full struct — safe
  because the companion is a deterministic function of the coordinates
  (two equal-coordinate vectors derive equal companions).
- The durable forms grow the ~3% (WAL JSON, checkpoint properties);
  pre-publish, no migration is owed.
- The executor stays single-threaded; parallelism reopens per §5 when
  measured corpora cross the threshold, with the query-latency harness
  as the arbiter.
