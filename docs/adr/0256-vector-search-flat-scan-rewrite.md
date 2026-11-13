# ADR-0256: Vector SEARCH flat-scan rewrite — bounded top-k heap, no per-row Value clones

Status: Accepted

## Context

`vector_index_candidates` (`cypherlite-executor/src/exec/vector.rs`) is a
brute-force scan by design: the vector index is schema metadata (name, labels,
property, dimensions, similarity function — ADR-0040 left vector indexes out
of the postings machinery), and the embedded scale target does not warrant an
ANN structure. The scan's *shape*, however, is off by 8-15x from what the same
brute-force scan costs when written well (#311):

- it clones a `Value::Node`/`Value::Relationship` per scored candidate into a
  growing `Vec` — for a 1M-vector index that is 1M entity materializations
  before any windowing;
- it scores with a sequential scalar `f64` dot product that the compiler
  cannot vectorize (a single accumulator is a data dependency chain);
- it fully sorts every scored candidate (`sort_and_window`) before slicing
  `SKIP`/`LIMIT` — `O(n log n)` plus a full-length collect where
  `O(n log (offset+limit))` suffices.

Benchmark evidence (issue #311; three passes over a throwaway crate:
synthetic 100k-50M, real 90k×1536 neo4jlabs `companies` Chunk embeddings, and
real 90k×768 MRL-trained `MongoDB/mdbr-leaf-ir` embeddings):

- Flat SIMD-style f32 scan + bounded k-heap is **exact** and 8-15x faster
  than the current scalar-f64 collect-then-sort shape; 1M×768d serves in
  61 ms single-thread, 12 ms thread-parallel.
- The embedded memory envelope is ≤1M vectors (1M×1536 f32 = 6 GB just for
  vectors), and within that envelope no index structure earned its keep:
  IVF loses to a parallel flat scan below ~10M and costs 11 s+ of build at
  90k; MRL early-abandon and tiered-prefix rescoring are **slower** than the
  plain flat scan even on genuinely MRL-trained embeddings (the bound
  arithmetic and horizontal reductions eat the saved MACs); a bit-quantized
  prefilter is the only approximate path that beat the flat scan on real
  data (0.5-1.0 ms at 0.888-0.998 recall@10) — a possible opt-in follow-up,
  not part of this change.
- `WITH`-pushdown stays exact and free on a flat scan, which an
  ANN structure cannot promise for filtered queries.

## Decision

Rewrite the flat scan inside `vector_index_candidates`; keep the vector index
metadata-only and keep the scan exact. Engine-measured on a 50k×768 real
mdbr-leaf-ir corpus through `Database::query` (p50, 50 queries): 155 ms →
93 ms (1.7x); the residual is the per-row `Value`-coordinate walk and property
lookup, which the packed-matrix follow-up below removes:

1. **No per-row Value clones.** The scan collects `(element_id, score)` pairs
   only. `Value::Node`/`Value::Relationship` is materialized solely for the
   ≤ `offset + limit` window survivors at the end. When a `WITH`
   filters are present, a `Value` is materialized per *filter-checked*
   candidate (as before) because `vector_filter_passes` evaluates properties
   through the expression evaluator — the filtered path keeps today's
   per-row clone, the common unfiltered path drops it.
2. **Bounded binary heap of size `offset + limit`** replaces the full sort.
   Ordering contract is unchanged: score descending, then element id
   ascending on ties (`sort_and_window`'s comparator), realized as a min-heap
   whose root is the worst retained candidate. `SKIP`/`OFFSET` and `LIMIT`
   windows therefore cost `O(n log k)` and no full-length collect.
3. **Scan-path kernels over `Value` coordinate lists, no per-row
   conversion.** The scan scores candidates straight from the property's
   `Value` list — eight partial-sum lanes for the dot, the candidate norm,
   or the squared distance — with the query norm hoisted once per search.
   The per-candidate `Vec<f64>` allocation of `vector_operand` is gone from
   the scan (query-side parsing keeps it). The float32-rounding contract of
   vector-search suite scores (Neo4j parity) is preserved: an equivalence
   test asserts the kernels round to the same `f32` value as the sequential
   pre-ADR-0256 reference over random, tail-carrying, and scale-varied
   inputs, and a second test pins the candidate-skip contracts (non-number
   or non-finite coordinate, non-list property, all-zero cosine candidate).
4. **No index structure** (no IVF, no bit quantization, no MRL tricks) ships
   in this change; all three remain rejected at embedded scale per the #311
   benchmarks, and any future approximate path must layer on top of this
   scan (prefilter + exact rescore), not replace it.

## Alternatives considered

- **IVF (k-means centroids, probed lists).** Rejected: recall 0.948 at 90k
  with 11.7 s build, slower than a 16-thread flat scan; only wins past ~10M
  vectors, which exceeds the embedded memory envelope.
- **Bit-quantized sign prefilter (RaBitQ-style) + exact rescore.** Rejected
  *for this change*: it is the one approximate path that beat the flat scan
  on real data (0.5-1.0 ms, 0.888-0.998 recall@10) but it changes the
  user-visible result set (approximate top-k). Revisit as an opt-in if a
  query budget ever demands sub-10 ms at ≥1M.
- **MRL early-abandon bound checks / tiered prefix rescoring.** Rejected:
  measured slower than the plain flat scan on both MRL-trained
  (mdbr-leaf-ir) and non-prefix-ordered real embeddings — the per-block
  bound arithmetic and second-pass strided rescoring cost more than the
  saved MACs.
- **Packed matrix cached per `(labels, property)` (the full 8-15x).**
  Deferred, not rejected: the throwaway benchmark's 8-15x came from a packed
  f32 matrix walked by SIMD lanes, and the engine measurement confirms the
  residual after this change is exactly the per-row `Value`-coordinate walk
  and property lookup. It did not ship here because it needs the declared
  index machinery as a fourth access-method kind (dirty-marking on writes,
  relationship twins, memory accounting, rebuild on open) and a cache that
  walks packed rows must keep the f64-accumulated-then-f32-rounded score
  contract (f32 accumulation changes it). It is the recorded follow-up to
  #311 with its own ADR when the remaining 4-6x is needed.
- **Keep the current shape.** Rejected: 8-15x on the vector SEARCH hot path
  for no semantic gain.

## Consequences

- `vector_index_candidates`' unfiltered path no longer allocates a
  candidates Vec proportional to the graph; memory per query is
  `O(offset + limit)`.
- Ordering and scores are bit-identical to the previous implementation at
  the f32-rounded contract level (the unrolled accumulator splits the f64
  summation tree; the equivalence test and the TCK corpus gate this).
- Filtered (`WITH`) searches keep the per-row materialization cost — they
  remain exact and take the same flat path, which is the design's answer to
  filtered-query degradation in ANN systems.
- A future bit-prefilter or f32-cache must coexist with the heap rewrite
  (prefilter → heap → exact rescore of the kth-best neighborhood), and will
  need its own ADR.

Linked: #311 (findings and benchmark tables), ADR-0040 (vector indexes out
of postings scope), ADR-0063 (lazy-materialization contract if a cached
structure ever ships).
