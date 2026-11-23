# REL8-1982 round 1 — results (in progress)

Measured on `d98aee8f` with vortex 0.93.0, lab crate `testing/relate-vortex-lab`.
Every measurement carries the 1-minute load average; the box is shared
(load 13–35 during these runs), so ns figures are noisy while **bytes and
allocation counts are the load-robust signal**.

## Size: bytes per property (vortex, per-column compressed sizes)

### Comment, 151,043 rows — 14.96 MB raw -> 6.23 MB vortex (41.6%)

| column | type as loaded | B raw/value | B vortex/value |
|---|---|---|---|
| content | Str | 33.4 | **19.50** |
| creationDate | Str | 28.0 | 13.34 |
| id | Int | 8.0 | **5.12** |
| locationIP | Str | 12.7 | 2.02 |
| length | Int | 8.0 | 0.89 |
| browserUsed | Str | 9.0 | 0.38 |

### Post, 135,701 rows — 10.79 MB -> 3.46 MB (32.1%)

`content` 6.53, `creationDate` 12.60, `id` 5.11, `locationIP` 0.74,
`length` 0.40, `browserUsed` 0.05, `language` 0.05 B/value.

### Forum, 13,750 rows — 0.82 MB -> 0.44 MB (53.7%)

`id` 5.36, `title` 13.40, `creationDate` 13.40 B/value.

### Person, 1,528 rows — 66,377 B -> 43,356 B (65.3%)

`id` 8.1, `firstName` 6.5, `birthday` 6.4, `lastName` 5.5, `browserUsed` 1.1,
`gender` 0.8 B/value.

### Synthetic flat, 150,000 rows, 4 int attrs — 7.95 MB -> 1.50 MB (18.8%)

`id` (dense 0..n) **0.000 B/value** — 776 B for 150k values;
`key` (unique strings) 5.5; `attr0..3` (periodic ints) 1.0–1.3 B/value.

### Findings that matter more than the ratios

1. **Dense internal ids are free; sparse business IDs are not.** The
   synthetic `id` column (dense `0..n`) compresses to 0.000 B/value, but the
   real LDBC `id` costs **5.12 B/value** — business ids are sparse snowflake
   numbers. Retiring the engine's id-vec slots rests on *internal* dense
   node-ids; it does NOT extend to a sparse business `ID` property. Do not
   quote the 0.0 B/value figure without this distinction.
2. **Dates loaded as strings are the worst column** (12.6–13.4 B/value):
   the lab loads `creationDate`/`birthday` as `Str`, so vortex's
   `datetime_parts` encoding never applies. A `Ty::Temporal` column is a
   concrete, unmeasured win to add (the engine's wide codec keeps temporal
   values residual too, so this is a shared gap).
3. **Low-cardinality strings compress 20–100x** (`browserUsed` 0.05–0.38,
   `language` 0.05, `gender` 0.8 B/value) — the dictionary path is where the
   property-bytes win concentrates.
4. **High-cardinality text compresses only ~1.5–2x** (`content` 19.5 vs 33.4
   raw, `title` 13.4 vs 23.8) — free text is the floor of the size story and
   it dominates the Comment table's absolute bytes (2.94 MB of 6.23 MB).
5. **Repeating ints compress 6–20x** (`length` 0.40–0.89 vs 8 raw).

## Kernels O1/O2/O3 (first pass, load 13–20 — ns noisy, allocs reliable)

- O1 point read on a warm decoded column: 2,083 ns, 3 allocs/value.
- O2 three columns at the same rows: 36,555 ns, 28.5 allocs — **18x for 3x**
  (see the probe investigation; per-value string extraction is
  allocation-heavy, ~9.5 allocs per string value).
- O3 seek, 150k rows: full scan+compare 457 µs vs binary search on the
  sorted key **586 ns** (780x).
- O3 key-column decode (mmap + read_all): 1.19 ms, 1425 allocs, 182 KB.
- O10b `with_row_indices` 1024-row gather: 899 µs = 878 ns/row, 1.4
  allocs/row — suspicious (see probe investigation).

## O19 traversal set state (measured, load 27–36; ratios stable across 3 runs)

Seen-set, 1M visited ids:

| representation | bytes/set | B/node |
|---|---|---|
| HashSet<u64> | 18,874,376 | 18.9 |
| HashSet<u32> | 10,485,768 | 10.5 |
| dense bitset (7-bit spacing) | 875,000 | 0.875 |
| dense bitset (compact 0..N space) | 125,000 | **0.125** |

21.6x smaller than HashSet<u64>. **Corrects the spec's "~48+ B/node"** — that
figure came from a different structure (the lazy postings' per-bucket cost),
not from a visited HashSet; hashbrown at 1M entries is 18.9 B/node (11.8 at
100k, power-of-two bucket rounding).

Frontier algebra, 100 BFS levels of 10k new ids:
- bitset `new = next & !seen; seen |= new`: 42–49 µs/level, **0 allocs**
- HashSet (`difference().collect()` + extend): 151–171 µs/level, 13 allocs,
  262 KB/level
- 3.4–3.7x faster, alloc-free.

Order preservation: arrival-order parallel `Vec` costs **+9 µs/level (+29%)**;
sort-at-end 58 µs/level; ascending materialization from the bitset 86 µs/level
(the mandatory space scan, not the ordering). Keeping the Vec is the cheap
ordered frontier.

frontier ∩ predicate (10k frontier, ~50% selective): bitset probe **6.6 µs**
vs HashSet probe 65–76 µs (10x); bitset AND-scan 23–25 µs.

Crossover (the surprise): the bitset wins memory while reachability density
exceeds ~1% of the id space (space ≲ 9.4M bits), and wins time up to ~12–15M
bits for a 10k frontier — beyond that the space scan dominates and the
HashSet wins. LDBC/Pokec BFS-like reachability (14% density in this kernel)
sits deep in the bitset's win region.

## O7/O8/O11 joins and compares (measured, release, load 17.4 -> 16.3)

Join on a low-cardinality int key (attr0, 1000 distinct values, 5,625,000
matching pairs over two 75k-row halves — the corpus `a.birthday = b.birthday`
shape; all three arms assert the same match count):

| arm | ns/op | allocs/op | B/op |
|---|---|---|---|
| O7a sort-merge (both sides key-sorted) | 45,908 | **0** | 0 |
| O7b hash join (build + probe) | 1,044,608 | 6,001 | 5,333,384 |
| O7c sort both sides + merge (no index) | 1,931,231 | 2 | 2,400,000 |

**Merge join is 23x faster and allocation-free — but only when both sides
arrive key-sorted.** Without an index the sort dominates (1.93 ms) and the
hash join wins. This is the measured payoff of the `(key, node_id)` sorted
index arm: it does not just serve point seeks, it turns the corpus's
property-equality joins into alloc-free merges.

Compare kernels:

| arm | ns/op | allocs/op |
|---|---|---|
| O8a intra-row compare, decoded vec per row | 21,948 | 0 |
| O8b intra-row compare, vectorized kernel | 22,055 | 7 |
| O8c cross-row (i vs i+1) via two slices | 17,272 | 7 |
| O11a gather 1024 rows, then compare | 69,660 | 640 |
| O11b compare all rows, filter at 1024 positions | 132,165 | 592 |

- **The vectorized compare is NOT faster than an autovectorized scalar loop**
  (22.1 vs 21.9 µs) — LLVM emits the same lane kernel. Its value is comparing
  encoded/lazy arrays without materializing values, not per-element speed;
  it costs 7 allocs/op (output bitmap + scaffolding).
- **Late materialization pays in miniature**: gathering 1024 of 150k rows and
  comparing only those beats comparing everything and filtering —
  **1.9x** (69.7 vs 132.2 µs) even with the gather's 640 allocs/op. The
  gather path is the one the probe agent is investigating for exactly that
  allocation cost.
- Cross-row comparison (the fraud `ti.date < tj.date` shape) via two
  constant-time slices costs no extra decode (17.3 µs).

API facts: the vectorized compare is
`vortex_array::scalar_fn::fns::binary::compare::execute_compare`, reached
from `lt(col("l"), col("r"))` → `.bind(dtype)` → `ArrayRef::apply_bound` →
`execute::<BitBuffer>(&mut ctx)`; `BitBuffer::true_count()` gives the count.
`vortex::compute` is deprecated and effectively empty — kernels live under
`scalar_fn::fns/`. Comparisons were measured over in-memory canonical arrays;
**encoding-space compare pushdown (comparing without decompressing) is not
yet measured** — the obvious next kernel.

## O4/O5/O12/O16 filters, pruning, presence (measured, load 20.5)

Prune unit: `vfile.splits()` on the 150k synthetic store returns **5 ranges of
8k–40k rows** (compressor-chosen), not 1024 — coarser than a morsel.

| kernel | ns/op | allocs | B/op |
|---|---|---|---|
| O4a range scan+count `attr0<500` | 2.78 ms | 716 | 123 KB |
| O4b `can_prune(attr0>100000)` | 22.7 µs | 63 | 5.8 KB |
| O4b scan `attr0>100000` (whole-file prune) | 134 µs | 301 | 37 KB |
| O4c exists `.with_limit(1)` | 2.71 ms | 745 | 126 KB |
| O4d per-split stats-only prune pass | 36 µs | 35 | 47 KB |
| O4e range scan `id<75000` (monotone col) | 583 µs | 641 | 112 KB |
| O5a conjunction (`attr0<500 AND attr1>100`) | 8.14 ms | 1202 | 515 KB |
| O5b sequential two scans + gather | 15.6 ms | 1520 | 4.19 MB |
| O12a prefix range `node-0001` | 10.7 ms | 986 | 1.74 MB |
| O12c prefix binary search (in-memory) | 624 ns | 0 | 0 |

### Findings

1. **`with_limit(1)` is not an exists short-circuit** (2.71 vs 2.77 ms — 98% of
   the full count; identical at `with_concurrency(1)`). The limit truncates
   output, it does not stop decoding. A cheap exists exists only as the
   *file-level* prune (134 µs). Corrects the O4 spec's early-exit assumption.
2. **`can_prune` works and is decode-free, but is file-level only.** Per-split
   pruning requires `LayoutReader::pruning_evaluation(split, &bound,
   Mask::new_true(len))` — a real API gap: no public "which splits does this
   filter prune". The stats-only pass over 5 splits costs 36 µs / 35 allocs.
3. **Conjunction prunes per column, but a zone survives iff *every* conjunct
   keeps it** — so a conjunction never prunes more than its most selective
   conjunct (`attr0<500` 0/5, `attr1>100` 0/5, conjunction 0/5; `id<75000` 3/5,
   conjunction 3/5). The measured win is elsewhere: **one scan with per-column
   zone masks beats executor-side sequential re-filter by 1.9x in time and 8x
   in memory traffic** (8.14 ms/515 KB vs 15.6 ms/4.19 MB), because the
   sequential path materializes 75k survivor ids and gathers them.
4. **My synthetic `attr0` distribution silently defeats zone-map pruning.**
   `attr0 = (i*3)%1000` means every 8k–40k-row zone spans the full 0..999
   range, so a range predicate can never prune. `attr0<500` prunes 0/5 splits;
   the honest demonstration needs a monotone column — `id<75000` prunes 3/5
   and is 4.8x faster (583 µs vs 2.78 ms). **Any range-pruning claim must use
   a monotone or clustered column**; the synthetic shapes in `store.rs` need a
   monotone variant before the next campaign.
5. **A string range predicate is expressible** —
   `and(gt_eq(col("key"), lit(lower)), lt_eq(col("key"), lit(upper)))` with
   `upper = prefix + "\u{10FFFF}"` binds and runs, and prunes 4/5 splits — **but
   it is a loss**: 10.7 ms / 1.74 MB per scan vs 0.71 ms to decode the whole key
   column plus 1.5 ms to `starts_with` it in memory (binary search: 624 ns). The
   Utf8 comparison path dominates. So STARTS WITH via range is *expressible and
   prune-friendly, not a win* at this size.
6. **Presence: a property some nodes lack is not representable as a null.**
   The store writes NonNullable columns, so the footer records
   `NullCount = Exact(0)` for every promoted column, `can_prune(IS NULL)` is
   always true and `can_prune(IS NOT NULL)` always false. `arr.statistics()` on
   the *decoded* array reports `NullCount = Absent` (stats are not carried
   footer → canonical; `compute_null_count` recomputes in 98 ns). Consequence
   for our lenient-presence semantics: per-key presence needs the residual
   column or per-label sparse columns — a promoted non-null column cannot
   express "absent".
7. No count-without-materialization: the scan builder has no popcount/mask
   terminal, so counts include materializing the projected column at survivors.

## Open

- O4–O20 kernel results (fan-out in progress).
- The two anomalies above (O2 cost scaling, O10b gather behaving like a
  re-scan).
- The engine-side A/B (CompactProps + postings) — needs the container
  harness or the `research`-feature read path; `CompactProps` is `pub(crate)`.
- `Ty::Temporal` column for date properties.
- The VFS mmap prototype (round-1 deliverable, step 60).
