# REL8-1982 round 1 — results (in progress)

Measured on `d98aee8f` with vortex 0.93.0, lab crate `testing/relate-vortex-lab`.
Every measurement carries the 1-minute load average; the box is shared
(load 13–35 during these runs), so ns figures are noisy while **bytes and
allocation counts are the load-robust signal**.

> **Provenance note (review round, 2026-11-24).** The review round before
> the PR refactored the kernel bodies (decomposition, one shared helper
> per policy) without changing any kernel's semantics, with three
> exceptions that move the numbers below: the probe-row pick is now one
> `bench::spread_row` (Knuth step everywhere; the old `7919` sites sample
> different rows of the same shape), the sets kernels 4/5 seed their own
> RNG (their frontiers no longer follow the BFS stream), and the row-store
> arm no longer carries an identity key-list scan (`S1a point` is a Vec
> index now, ~equal to the column store instead of linear). The tables in
> this document were measured before that refactor; the shapes and the
> load-robust signals (bytes, allocation counts) are unchanged, and the
> affected ns figures should be re-stamped by the next lab run.

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

## Kernels O1/O2/O3 (quiet-box re-run 2026-11-23, load 5.3–6.0 — supersedes
the load-13–20 first pass whose ns figures were 3–17x inflated; allocs were
stable across both runs)

- O1 point read on a warm compressed column: **258 ns, 3 allocs/value**.
- O2 three columns at the same rows: **2.1 µs, 26.5 allocs** — linear in
  columns (~8.8 allocs per string-column value; see the probe
  investigation for the per-encoding breakdown), no superlinearity.
- O3 seek, 150k rows: full scan+compare **176 µs** vs binary search on the
  sorted key **490 ns** (~360x).
- O3 read_all (compressed arrays, whole file): 205 µs, 1,393 allocs, 181 KB.
- O10b `with_row_indices` 1024-row gather: 106 µs per gather (~103 ns/row),
  1.45 allocs/row — still the decode-and-mask fallback (see the probe
  investigation; the per-row cost only looks cheap because the fallback
  decodes once per gather call, not per row).

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

### Monotone re-run (2026-11-23, load 6.4 — the honest range-pruning shape)

The `mono` synthetic shape (attr values grow with position, date-like, with
jitter; added to `store.rs` after the methodology correction). Same session,
same code, same load as the flat rows beside it — the honest A/B:

| kernel | ns/op | allocs | B/op |
|---|---|---|---|
| O4m range scan `attr0<⅓range` — **3/5 splits pruned** | 39.0 µs | 572 | 97 KB |
| same-session flat anti-shape `attr0<500` — 0/5 pruned | 72.2 µs | 706 | 122 KB |
| O4m no-match range `attr0>max` (file prunes) | 10.3 µs | 292 | 37 KB |

- **1.85x faster, −19% allocs, −21% bytes** with 3/5 splits pruned — and the
  selectivity differs (33% vs 50% survivors), so if anything the prune gain
  is understated.
- **Conjunction pruning is a UNION**: `attr0<⅓` prunes 3/5, `attr1>⅓`
  (opposite end) 1/5, conjunction **4/5** — the union of the two pruned sets.
  A zone survives iff every conjunct keeps it, so the pruned set is the
  union, *at least* the most selective conjunct and more when conjuncts
  prune different zones.

### Findings

1. **`with_limit(1)` is not an exists short-circuit** (2.71 vs 2.77 ms — 98% of
   the full count; identical at `with_concurrency(1)`). The limit truncates
   output, it does not stop decoding. A cheap exists exists only as the
   *file-level* prune (134 µs). Corrects the O4 spec's early-exit assumption.
2. **`can_prune` works and is decode-free, but is file-level only.** Per-split
   pruning requires `LayoutReader::pruning_evaluation(split, &bound,
   Mask::new_true(len))` — a real API gap: no public "which splits does this
   filter prune". The stats-only pass over 5 splits costs 36 µs / 35 allocs.
3. **Conjunction prunes per column, and pruned zones are the UNION of the
   conjuncts' pruned sets** — a zone survives iff *every* conjunct keeps it,
   so the pruned set is the union: at least the most selective conjunct, and
   more when conjuncts prune different zones (measured on the monotone store:
   3/5 ∪ 1/5 → **4/5**; corrected 2026-11-23 — this section earlier claimed
   the opposite direction, derived from flat-store 0/5-everything numbers
   that could not distinguish). The executor-side win stands: **one scan
   with per-column zone masks beats executor-side sequential re-filter by
   1.9x in time and 8x in memory traffic** (8.14 ms/515 KB vs
   15.6 ms/4.19 MB), because the sequential path materializes 75k survivor
   ids and gathers them.
4. **The flat store's `attr0` distribution defeats zone-map pruning — the
   monotone shape replaces it for range claims.** `attr0 = (i*3)%1000` means
   every 8k–40k-row zone spans the full 0..999 range, so a range predicate
   can never prune (0/5). `store.rs` now carries a `mono` shape (values grow
   with position, plus jitter); the monotone re-run table above is the
   quotable range-pruning result, and **any range-pruning claim must come
   from that shape or a real clustered column** (`id<75000`: 3/5 pruned,
   4.8x).
5. **A string range predicate is expressible** —
   `and(gt_eq(col("key"), lit(lower)), lt_eq(col("key"), lit(upper)))` with
   `upper = prefix + "\u{10FFFF}"` binds and runs, and prunes 4/5 splits — **but
   it is a loss**: 10.7 ms / 1.74 MB per scan vs 0.71 ms to decode the whole key
   column plus 1.5 ms to `starts_with` it in memory (binary search: 624 ns). The
   Utf8 comparison path dominates. So STARTS WITH via range is *expressible and
   prune-friendly, not a win* at this size.
6. **Presence was a lab write-shape artifact, not a vortex limit (corrected
   2026-11-23).** The store wrote NonNullable columns, so the footer recorded
   `NullCount = Exact(0)` for every promoted column — that measured our
   write shape, not vortex. Written **nullable**, promoted columns express
   the engine's lenient semantics directly: **null = absent, no sentinel
   values needed**. Cost: a validity bitmap where nulls exist (~1 bit/row;
   all-present columns do not pay), and `NullCount` then serves IS NULL /
   IS NOT NULL pruning per zone (the O16 mechanism, now expressible).
   Mostly-absent keys compress to ~the validity bits, so promotion stays
   cheap; the promotion threshold still prefers keys present on most rows.
   To re-measure: write the table nullable and confirm the per-zone
   IS NULL prune fires. (Decode-side stats stay absent either way — see
   the aggregates section, finding 5.)
7. No count-without-materialization: the scan builder has no popcount/mask
   terminal, so counts include materializing the projected column at survivors.

## Probe: the two anomalies, resolved (measured, load 8.5 -> 8.1)

**O2 was column mix, not superlinearity.** `read_all` returns *compressed*
arrays (dict / onpair / fastlanes.bitpacked), not decoded ones — the old
"column decode" label was wrong. Per-read cost is encoding-dependent:
`firstName` 16 allocs/read (its codes child is `fastlanes.bitpacked.v2`, so
each value probe decompresses a 2848-code child), `lastName` 6.5, `birthday`
6.0, `gender` 3.0 (dict over FSST, 2 values). O1's 2.1 µs / 3 allocs was the
*cheapest* column. O2's 28.5 allocs = 16.0 + 6.5 + 6.0 exactly. `repeated_probe`
does not help — Dict/OnPair/BitPacked all declare `ProbeState = ()`.

The honest morsel read path, measured:

| path | ns/row | allocs/row |
|---|---|---|
| per-value `execute_scalar` on compressed arrays | 32,737 | 35.5 |
| `take` per column (4 cols, compressed) | 805 | 0.11 |
| decode column once (`execute::<Canonical>`) | 305 (119 on the key col) | 0.04 |
| canonical per-value reads after decode | 63 (44.5) | 0 |
| prepared contiguous `with_row_range` (1024) | 181 | 0.22 |
| prepared sparse `with_row_indices` (1024 of 150k) | 699 | 0.93 |

**O10b is a decode-and-mask, not a gather.** `with_row_indices` becomes
`Selection::IncludeByIndex`; `sparse_ranges` bails when a range span reaches
`MAX_RANGE_SIZE = 4000` rows with gaps below `MIN_GAP_BETWEEN_RANGES = 50_000`
(the lab's ids were ~7,919 apart), so it falls back to natural splits + a row
mask — **the whole file is decoded**. A 1-row read costs ~0.37 ms = one chunk
decode. `take` is a real gather only on already-canonical columns (on a
compressed Chunked column it is slower than a full scan).

**Verdict: the arbitrary-id batched gather does not exist in vortex 0.93.**
What does: (a) decode a column once per morsel and read canonically
(119 + 44.5 ns/row), (b) a *prepared* `RepeatedScan` with a **contiguous**
1024-row `with_row_range` (181 ns/row, chunk-granular), (c) `take` on
canonical arrays. Design consequence: **a store-side morsel push must carry
contiguous 1024-row ranges and cache decoded chunks** — which the node-major
row==node-id layout provides naturally for id-ordered morsels.

## O6/O13/O14/O15/O20 aggregates (measured, load 10.4–18.3)

Encodings the default compressor chose on the synthetic store: `id` →
`vortex.sequence`, `key` → `vortex.onpair`, `attr0`/`attr3` →
`fastlanes.bitpacked.v2`, `attr1`/`attr2` → `vortex.dict`; every column
`vortex.zoned` (zone = 8192 rows → 19 zones); chunks 40,960 rows sharing ONE
values array (`ArrayRef::ptr_eq` across chunks → column-global dict codes).

| kernel | path | cost |
|---|---|---|
| O13 countDistinct | footer layout walk (`zoned > dict` values child count) | **1.2 µs, 0 allocs** |
| O13 | scan + union chunk dict values | 0.23 ms |
| O13 | decode + HashSet | 27.2 ms |
| O14 histogram | count dict codes (no value decode) | 2.48 ms |
| O14 | decode + HashMap | 27.6 ms |
| O14 zone read | `dyn_slot` + `dyn_new_reader` + `projection_evaluation` → 19 Min/Max pairs | 35 µs |
| O14 quantile | position read on the sorted `sequence` column | **378 ns, 0 allocs** |
| O14 quantile | decode + full sort | 0.65 ms |
| O15 limit | `.with_limit(100)` scan vs full scan | 0.40 vs 0.82 ms |
| O15 top-k | `select_nth_unstable` vs full sort | 5.6 vs 31.5 ms |
| O6 group-by | per-code counts on a dict column | 2.48 ms |
| O6 | decode + HashMap | 26.6 ms |
| O20 distinct pairs | packed u32 code-pair HashSet | 28.5 ms, 1.28 MB |
| O20 | decoded tuple HashSet | 37.3 ms, 5.5 MB |

### Findings

1. **The dictionary is the cheap aggregate path — but only when the
   compressor chose it.** `countDistinct` is a 1.2 µs layout-tree walk for a
   dict column (no segment reads), 0.23 ms via chunk dict values, 27 ms by
   decode+hash. `attr0` got *bitpacked* instead → no encoded distinct path at
   all. The compressor's dict-vs-bitpack choice is not controllable through
   the default write options the lab uses — a knob to find before promising
   these wins.
2. **The exactness flag is unusable**: the layout's `all_values_referenced`
   is not exported, and `DictArray::has_all_values_referenced()` reads false
   (unknown) even when the count is right — so a reader cannot validate that
   no dictionary value is unreferenced. You trust the writer's dedup.
3. **Zone statistics give a column-wide bound, not a histogram.** The
   zone-table read works without decoding (35 µs, 19 Min/Max pairs) but 0/19
   zones fall inside a single equi-width bin — the zone length (8192) exceeds
   the value period (1000), the same distribution flaw the filter kernel
   found. Exact bins need a decode.
4. **Sortedness is not discoverable from the file.** The quantile position
   read (378 ns) works on the `sequence`-encoded column, but `IsSorted` is
   not persisted in `file_stats` (only commutative stats are) — it appears
   only because the encoding supplies it on the decoded chunk. A reader would
   have to know the layout.
5. **Decoded chunks carry no statistics.** The decoder does not populate
   Min/Max/Sum/NullCount on decoded arrays — stats are footer-only at file
   level and zone-only at chunk level. Any aggregate-from-stats design must
   read the footer/zone tables, not the decoded array.
6. **No top-k operator** (5.6× for `select_nth_unstable` over a full sort, but
   that is a client-side kernel), and `with_limit` buys only ~2× because the
   first row-group of every projected column is read regardless — consistent
   with the filter kernel's "no exists early-exit".
7. **Code-pair distinct works and is a memory win**: 4.3× less memory
   (1.28 vs 5.5 MB) and 1.3× faster, valid only because the dicts are
   column-global and shared; it correctly reports "not available" when chunks
   do not share one values array.

## Design decisions from the round-1 discussion (2026-11-23)

Post-measurement discussion with the round-1 owner; these shape round 2.

1. **Nullable promoted columns replace the JSON residual.** Every property
   key gets its own nullable column, added as we go: null = absent (the
   engine's lenient semantics, no sentinel values), a mostly-absent key
   costs ~its present-ids + values (Sparse), and `NullCount` serves IS
   NULL / IS NOT NULL per zone. The long tail that would never earn a
   column is a typed `list<struct{key,value}>` residual — scan-only, no
   JSON parse. The JSON residual was a wide-parquet-checkpoint concern, not
   an in-memory one.
2. **Immutable vortex files are the checkpoints — same lifecycle, no new
   machinery.** WAL is the append log (unchanged), the live/delta tier is
   the in-memory overlay (ADR-0016's shape), the checkpoint fold
   rewrites/compacts the touched column files, reads mmap the sealed
   files. Vortex 0.93 needs no append API because the engine never asked
   the snapshot format for one.
3. **Layout decided: row-range shards, not per-column files** (2026-11-23,
   with the round-1 owner). File-per-column was prototyped and rejected —
   reads are layout-neutral (VFS prototype: within 5% of allocations
   either way) while every per-column file pays its own footer parse
   (~1.7k allocs, V1), so it buys nothing on the read path and costs N
   footers at open. The store shards like the Parquet checkpoint already
   does (ADR-0022 id-window shards): column-complete struct files per
   node-id window, the checkpoint fold rewrites only dirty shards, and the
   dirty-node ids the engine already exports are sufficient — **no
   per-key dirty statistics extension needed**. Add-a-column lands in the
   live overlay and materializes at the next seal via the existing
   wide-schema re-derivation (ADR-0534).
4. **Engine integration goes through the existing seams only.** The
   experimental store loads through the regular VFS APIs (ADR-0072) — no
   new I/O mechanism introduced together with the new store — and slots in
   as an experimental storage option in the shape of the other optional
   codecs (ADR-0105), following ADR-0534's precedent (column files inside
   a codec, not a new flag). mmap is a later, separate step where the VFS
   permits.
5. **Store additions are shaped like the upstream proposed APIs** (the
   rule recorded under Upstream tracking): probe-shaped morsel seam with a
   density hint, membership-mask batched key check, limit-shaped exists.
   When vortex lands them, our seam delegates instead of reimplementing.

## VFS prototype (2026-11-23, load 99.5 — ns noise-dominated; allocs and
RSS deltas are the signal)

`ops/vfs.rs`: the same Person table (1,528 rows, 6 columns) as one struct
file (47,312 B) and as 6 column files (55,056 B).

| measurement | struct file | column files |
|---|---|---|
| V1 open | 1,765 allocs / 788 KB | **10,140 allocs / 4.65 MB** (6× footers) |
| V3 single-column read | 219 allocs / 35 KB | 210 allocs / 34 KB |
| V4 3-column read | 629 allocs / 101 KB | 602 allocs / 98 KB |

- **Reads are layout-neutral; opens are not.** Per-file footer parse costs
  ~1.7k allocs and ~790 KB regardless of file size — the per-column
  layout's tax is paid at open, and at 60 columns an eager open is
  ~100k allocs / ~47 MB transient. Lazy open per touched column is
  mandatory in a per-column layout.
- V6 compaction remap: open of a rewritten column file = one footer
  (1,691 allocs), first read on the new inode is cold pages — the honest
  per-column fold cost.
- **V7 page-level skipping is real and large**: on the 6.2 MB Comment
  file, a projected read of one column of six faults **16 KB** of pages;
  reading all columns faults **672 KB** — the untouched columns' pages
  never enter RSS (42x). Per-query resident memory ≈ touched segments,
  which is the mmap store's whole memory story in one number. (RSS deltas,
  load ~20.)

## Seam fit vs the engine (2026-11-23, full detail in `seam-findings.md`)

| Seam | Verdict |
|---|---|
| VFS (ADR-0072) | **Fits** — a new backend is a new `Vfs` impl, no trait change; caveat: `VfsFile` is byte-copy, no borrowed-slice arm, so zero-copy decode mmaps outside the trait or the trait gains one optional method |
| Snapshot codec (ADR-0050/0105) | **Fits** — a `SnapshotCodecKind::Vortex` + `CodecOps` + evidence row is additive; the Parquet codec already publishes many immutable shard files with add-column-at-checkpoint and dirty-shard incremental rewrites (ADR-0022/0089/0534). Extends: `dump` sees the graph only as `GraphImport` *rows* (no columnar export). Per-key dirtiness is **not** needed — row-range shards compact on the exported dirty ids |
| Property read path (ADR-0033/0016/0614) | **New seam needed** — `CompactProps` (pub(crate)) inside `LiveNode` inside `Graph` MVCC chains; zero traits between executor and property storage. A property-read trait at the `node_prop_id_at` / scan-predicate boundary (plus the overlay `base`, the restore path, and the ADR-0614 postings — the sorted (key,row) side-file is their natural replacement) is the one hard-wired seam |
| Checkpoint lifecycle (ADR-0012/0014/0022/0089/0183) | **Fits** — immutable publication, dirty-id incremental rewrite, orphan sweep, follow reload all exist; per-key dirty tracking is the extension |
| Memory metering (ADR-0042/0463) | **Extends** — charge logical (uncompressed-size-style) bytes to the ADR-0042 `used` counter; mmap resident-page accounting is undecided and needs its own decision record |

## Impact vs the engine today (2026-11-23, quiet box load 5.3–6.0)

Baseline anchors re-measured this session on current main (`a3eaa5a2`),
reused SF0.1 store, same box as the prototype runs:

- **plan_bench**: 928.2 allocs/stmt, 143.5 KiB/stmt requested — matches the
  d98aee8f anchor (927.6 / ~143 KiB): the engine's property paths are
  unchanged, so cross-comparison is valid.
- **query-latency 30/30 ok**: fast band (14 queries ≤ 0.04 ms p50 —
  fixed-cost dominated), slow band q11 3.25 / q19 3.80 / q27 2.86 /
  q5 0.56 / q17 0.58 ms. (The earlier noisy-box run's slow-band p50s were
  ~1.5–2× lower; wall-clock p50s move between runs — the allocation anchor
  is the stable one.)

**What can be determined today, and what cannot:**

1. **End-to-end impact is not yet measurable.** The prototype is not wired
   into the engine (step 85, the property-read trait, gates it); the load
   and performance suites exercise `CompactProps` + postings, which this
   branch does not touch. Everything below is a per-operator projection
   from measured numbers on both sides — labeled as such.
2. **Memory is the strong claim.** Whole property tables at 32–54% of raw
   value bytes (dictionary columns 0.05–0.38 B/value) vs the per-node
   compact-list layout's Vec + Value boxing; traversal state 21.6×
   smaller as bitsets; the sorted (key,row) side-file replaces the
   postings' resident buckets (post-#672 comparison pending — step 85).
   The suite does not measure resident bytes, so this win is invisible to
   the current gates and needs the container-harness run (step 70).
3. **Latency: neutral-to-negative on the fast band.** 14 of 30 queries sit
   at ≤ 0.04 ms p50, dominated by fixed statement costs — a columnar read
   path can only add decode overhead there (per-value probe on compressed
   arrays is 32.7 µs/row unless chunks are decoded once and cached —
   63 ns/row after decode; the store design makes the decoded-chunk cache
   mandatory).
4. **Latency: real wins on specific slow shapes, bounded by the property
   share.** Dict-served group-by/countDistinct are 12–27× the decode+hash
   path (95.7 µs vs 1.19 ms group-count; 1.2 µs vs 27 ms countDistinct);
   conjunctive single-scan 1.9× and 8× less memory traffic than
   sequential re-filter; zone-pruned ranges 1.85× on clustered data. But
   the measured profile of the slow queries puts property access at
   ~1–3% of their time (q20: `node_prop_resolved` 0.3%, `prop_read`
   0.3%) — traversal and adjacency dominate. So on THIS suite the
   projected p50 movement is small; the wins need property-dominated
   workloads (BI-style distinct/group-by, larger SF, conjunctive
   filters). The suite is the wrong instrument to demo the read-path
   win; it is the right instrument to prove no regression.
5. **Where the store does not help**: CONTAINS (6/30 queries — text index
   territory, unchanged), adjacency-dominated plans, and the load path
   (WAL + overlay unchanged; only the checkpoint codec changes).
6. **No impact on current tests until wired**: the branch changes no
   engine code; the property-read trait keeps `CompactProps` as the
   default, so the eventual A/B is opt-in and the existing gates stay
   green by construction.

## The property-store seam, wired and A/B'd (2026-11-23, load 4.8–5.5)

The store is wired in behind one trait (`seam.rs`), with two
implementations: `RowProps` (the compact-records-shaped baseline: per-row
interned key list + values, ADR-0033's shape) and `ColProps` (the vortex
store: mmap file, decoded-column cache, pushed scans, sorted-key arm —
`colstore.rs`). Every operator answers identically through both
(asserted). The A/B (`ops/seam_bench.rs`, 150k rows, mono shape):

| op | row store | col store | col effect |
|---|---|---|---|
| point attr0 | 20.9 ns | 8.0 ns | 2.6x |
| batch 1024 rows x 2 keys | 7.05 µs / 2 allocs | 2.23 µs / 2 allocs | 3.2x, alloc parity |
| expand conjunction (50k survive) | 2.99 ms / 15 allocs / 1.05 MB | 146 µs / **1,116 allocs** / 1.41 MB | 20x wall, **74x allocs** |
| expand cap=100 | 1.12 ms | 120 µs | 9.3x — cap works, but only 18% cheaper than uncapped: vortex's limit truncates without stopping decode (#11808, through the seam) |
| seek (sorted key arm) | 178 ns | 173 ns | parity — same index on both; postings parity by design |
| distinct attr0 (150k distinct) | 8.24 ms | 7.44 ms | parity — naive path; the dict-native rung is the real aggregation answer |
| top-k 10 attr1 | 863 µs / 8.4 MB | 209 µs / 2.4 MB | 4.1x, 3.5x memory |

Readings:
- **The wall-time win trades allocations**: one pushed expand costs ~1.1k
  allocs of scan machinery — a whole statement's plan_bench budget (928).
  Fast-band statements cannot absorb that per lookup; slow-band ones don't
  notice. The engine-side design must batch expands (one scan serving a
  morsel's whole predicate set), not expand per row.
- **Aggregation pushdown must ride the dictionary rung**, not the naive
  path — `stats_short_circuit`'s ladder shape (ADR-0611 precedent); the
  dict-native distinct is 22 µs / 247 allocs (O13b2) vs 7.4 ms here.
- **Seek parity is the design**: both stores use the same sorted
  (key,row) arm; swapping representation changes nothing until the
  side-file replaces the postings — that win is memory, not time.

## Seam ↔ engine API mapping (research: `exec-surface-findings.md`)

| Seam method | Engine API it serves | Adapter note |
|---|---|---|
| `expand(pred, cap)` | `lookup_nodes_upto(labels, props, cap)` → `Graph::lookup_nodes_upto_at` (index_lookup.rs:123), the candidates funnel's scan tail | same cap semantics (stop at cap+1); candidates are an owned `Vec<NodeId>` — a streaming sibling is the adapter for a lazy store |
| `seek` | ADR-0614 serve: `equality_postings_upto(labels, props, ts, cap)`, `EqualityRoute` decline/narrow | mirror the signature + decline semantics |
| `batch` | `Chunk` typed buffers (CHUNK_ROWS=512 / BATCH_ROWS=1024), `RowFeed` pull/push | emit Chunk-shaped, never a third exchange type |
| `distinct` (aggregation) | next rung of the `stats_short_circuit` ladder (ADR-0611 `index_aggregates` precedent) | value-level store reads feeding the shared group table |
| `topk(k)` | `TopKWindow` / `TopWindow`; ordered top-k scan pushdown is the engine's recorded gap | the seam fills the gap |
| `point` | `node_prop_id_at` / `node_prop_resolved` | the trait sits exactly at that boundary |
| `Pred` | IR `Expr` conjuncts + `Residual` — no plan→storage pushdown trait exists today | the seam's `Pred` is that missing trait; executor-side recognizers (Seek, WalkSpec, BatchPred) compile to it |

### Fast batch node+property filters — the seam closures (2026-11-23, load 7.8)

`expand` now takes labels (the `lookup_nodes_upto(labels, …)` shape) and
`member(key, sorted_vals)` serves the batched key check; adjacency stays
in the CSR arena by decision.

| op | row store | col store | effect |
|---|---|---|---|
| S7 label{0,1} + conjunction | 2.57 ms / 14 allocs | 387 µs / 2,020 allocs | **6.6x** — labels AND into the ONE pushed scan (or-of-eq over the dict label column) |
| S8 member, 1024 uniform keys | 463 µs | 435 µs | parity with individual binary seeks (~425 ns/key) — the suffix window barely shrinks on a uniform spread |
| S8c member, 1024 clustered keys | 161 µs | 157 µs | **153 ns/key, 2.8x better** — the morsel shape; the #9551 win is locality-dependent |

- The label filter riding the same scan works and is 6.6x, but costs +80%
  allocs over the property-only scan (2,020 vs 1,116 — or-of-eq decodes
  the label column per scan); a per-label bitset/bucket intersect inside
  the store is the cheaper future form.
- `member`'s galloping suffix search wins exactly where #9551 assumes it
  does: clustered batches. On uniform spreads it is seek-parity. The
  arm's pointer-heavy `Vec<(String,u64)>` layout is the limiter — a
  columnar arm (keys and rows in separate contiguous arrays, or the merge
  running over the still-encoded column) is the next optimization, not
  the algorithm.
- Anti-join (O17) composes as `member`'s complement — free.

## Memory kernels: mmap resident accounting, the decoded cache, the pokec shape
(measured 2026-11-24, load 225.6 on the shared box — ns figures are
noise-dominated at that load; **bytes, allocs and the RSS/page direction are
the signal**; the lab prints the load stamp per section)

`./target/release/relate-vortex-lab memory` (vortex pinned at
`relatedb/vortex#deecb674`) prints three tables; `… pokec` runs the fixture
shape's kernel. Resident bytes in table 2 come from the counting allocator's
live-heap gauge (`bench::live_bytes`), RSS from the process probe
(`bench::rss_kb`), `UncompressedSizeInBytes` from
`Stat::UncompressedSizeInBytes` on the decoded root (the canonical
representation — the "logical bytes" the ADR-0042 charge would use).

### Table 1 — per-shape file bytes vs `UncompressedSizeInBytes` vs full-touch RSS vs partial-touch RSS

| shape | rows | file bytes | `UncompressedSizeInBytes` | full-touch RSS KB | partial-touch RSS KB |
|---|---|---|---|---|---|
| synthetic-flat-150k (6 cols) | 150,000 | 1,546,320 | 10,350,000 | 736 | 32 |
| synthetic-mono-150k (6 cols) | 150,000 | 2,326,880 | 10,350,000 | 256 | 16 |
| pokec-100k (`:User`, 4 cols) | 100,000 | 182,672 | 4,000,000 | 48 | 16 |

- **`UncompressedSizeInBytes` is the canonical layout, not the value bytes.**
  Ints are 8 B/value; a `VarBinView` string costs 16 B (the view) plus its
  out-of-line bytes — strings ≤ 12 bytes ride inline in the view, so pokec's
  `gender` ("man"/"woman") costs 16 B/value and the table is exactly
  24 + 16 = 40 B/row (4.0 MB). The synthetic `key` is 13 chars → 16 + 13 =
  29 B/row (69 B/row with `id` + 4×8 B attrs). **This is the number an
  mmap-charging ADR has to define against**: canonical bytes (10.4 MB) are
  ~6.7x the synthetic file (1.5 MB) and ~22x the pokec file (183 KB).
- **Full-touch RSS is far below the file size** (736 KB of 1.5 MB; 256 KB of
  2.3 MB; 48 KB of 183 KB): reading every column still faults only part of
  the mapped file — the page-skipping claim (V7) holds for a whole-table
  read, not just a projected one. At load 225 the exact deltas move between
  runs; the direction (full ≫ one projected int column: 32/16/16 KB) is
  stable. Caveat: macOS does not return freed RSS promptly, so these deltas
  are lower bounds.
- pokec compresses hardest (183 KB, 4.6% of canonical): `id` is a dense
  `1..=n` sequence and `gender` is a 2-value dictionary — the exact shape the
  dictionary/String-data findings already predicted.

### Table 2 — whole-column cache vs per-chunk `repeated_probe` cache (attr0, 150k rows, 3 chunks)

| arm | miss ns/op | hit ns/op | resident B |
|---|---|---|---|
| whole-column decode + `Vec<i64>` | 129,947 | 6.1 | 1,200,000 |
| per-chunk `repeated_probe` (per chunk) | 224.5 | 132.2 | 576 |

- **The whole-column cache is the only real decoded cache.** It retains
  exactly the canonical column (150k × 8 B = 1.2 MB) and turns a read into a
  6 ns Vec index; the miss is the one-time 130 µs decode.
- **The `repeated_probe` cache retains nothing (576 B).** These encodings
  declare `ProbeState = ()` (the earlier probe investigation's finding:
  Dict/OnPair/BitPacked), so a "hit" still pays a block decode — 132 vs
  225 ns is within the load-225 noise, not a cached read. **Consequence for
  the design: a per-chunk decoded cache has to be our own chunk→canonical
  map; the upstream probe seam alone does not provide one in 0.93** — exactly
  what #8694's density-hinted canonicalization proposes and our `colstore.rs`
  upper-bounds today.
- Per full 3-chunk sweep: hit 396.5 ns / miss 673.4 ns (load 227.1). The
  payoff of the decoded map is 22x on the hit path; its cost is 1.2 MB per
  int column, which is the density-hint question table 3 sizes.

### Table 3 — density-hint cache size at three densities (attr0, 1.2 MB column, 3 chunks of 50k rows)

| density | rows requested | chunks touched | cache B (touched chunks) | ideal B (d × column) |
|---|---|---|---|---|
| 1% | 1,500 | 3/3 | 1,200,000 | 12,000 |
| 10% | 15,000 | 3/3 | 1,200,000 | 120,000 |
| 100% | 150,000 | 3/3 | 1,200,000 | 1,200,000 |

- **At the file's chunk granularity the cache is all-or-nothing**: a uniform
  1% selection already touches every chunk (chunk rows ≫ the morsel), so a
  cache-the-touched-chunks policy retains the whole column — 100x the ideal
  12 KB. The density hint only earns the whole-column resident cost at
  clustered selections; for a uniform morsel spread the per-chunk probe path
  (table 2, ~0 resident) is the only memory-safe arm. This is the
  measured answer to "decoded-cache cost unmeasured at real shape": per-chunk
  caching through the current Probe API saves no bytes; our own chunk map
  saves bytes only under clustering.

### pokec kernel: the fixture shape runs

`./target/release/relate-vortex-lab pokec` (load 227.0): 100,000 `:User`
rows, 4 columns, file 182,672 B; `age > 50` answers **133 of 100,000
(0.13%)** — the fixture's documented ~1/800 old arm
(`testing/relate-test-support/src/pokec.rs` reproduces exactly this
`old = below(800) == 0; age = 1 + below(50) + 50*old` draw).

- P1 point read of `age` by row (node id): **65.3 ns, 0 allocs**.
- P2 pushed filter scan `age > 50` (filter + projection in the reader):
  119.9 µs, 463 allocs, 84 KB/scan — the scan machinery cost is two orders
  above the answer itself, the same "batch the expands" lesson as the seam
  A/B.

## Concurrency: the lock-free decoded cache vs locks and per-thread copies
(measured 2026-11-24, load 125.5 — the box was under sustained load, so the
absolute ns are inflated and noisy; **the ordering and the per-thread resident
price are the signal**; three runs agree on the direction)

`./target/release/relate-vortex-lab concurrency` (`ops/concurrency.rs`) races
N barrier-synchronized threads through four ways of sharing one decoded
`Vec<i64>`: attr0 of the 150k-row synthetic shape (1.2 MB, 3 chunks). Point
reads index one row; batch reads sum 64 consecutive rows. The cold-cache
column is the decode count under an 8-thread stampede.

| arm | point ns/op @1/2/4/8 | batch ns/op @1/2/4/8 | cold decodes (8 threads) | resident B |
|---|---|---|---|---|
| lock-free `OnceLock<Arc<Vec>>` | 5.0 / 1.7 / 1.0 / 0.9 | 46.9 / 18.0 / 18.0 / 17.8 | **1** | 1,200,616 |
| `RwLock<HashMap>` | 32.6 / 29.6 / 96.5 / 58.9 | 169.5 / 66.4 / 90.3 / 205.6 | 1 | 1,200,116 |
| `Mutex<HashMap>` | 35.7 / 27.1 / 24.7 / 46.3 | 81.5 / 123.5 / 64.5 / 90.1 | 1 | 1,200,180 |
| per-thread cache (× 8) | 2.2 / 2.3 / 1.4 / 1.8 | 24.3 / 34.8 / 19.1 / 21.0 | **8** | **9,600,384** |

- **The lock-free hit does not degrade with N.** `OnceLock::get` is one
  acquire load, so the shared column reads at single-thread speed — 1 ns
  point (0.9–5.0 across runs), 18 ns per 64-row batch (0.28 ns/row) — at 1,
  2, 4 and 8 threads. The earlier three-run spread (point 0.9–21.9 ns) is the
  shared box, not the arm.
- **Both locks cost a reader 25–100 ns and grow with N.** `RwLock` pays a
  shared-word CAS on every `read()` (58–289 ns point at 4–8 threads across
  runs); `Mutex` serializes (24–59 ns). This is the "Mutex arm that must lose"
  and the RwLock fallback priced: ~30–100x the lock-free hit, before any
  writer.
- **Only the per-thread arm matches the lock-free hit** (1.2–2.3 ns point,
  19–35 ns batch) — and it pays 8 × the resident column: **9.6 MB vs 1.2 MB**
  for the same 150k rows. It also decodes the column **8 times** on a cold
  stampede against the shared arms' **once**: the all-or-nothing finding
  (table 3) priced per worker.
- **Cold fill-once is exactly one decode for every shared arm.** A racing
  first touch of `OnceLock`, `RwLock` and `Mutex` (double-checked) all decode
  the column exactly once; readers that lose the race wait on the miss path,
  never re-decode. The hit path never touches a lock in the adopted arm.
- **Decision**: the engine-side `VortexStore` adopts the lock-free arm — per
  column a fill-once `OnceLock<Arc<Vec<Val>>>` (chunks, chunk lengths and
  per-chunk parts likewise) — so the store is `Send + Sync` and reads at
  single-thread speed from every executor thread. The reader runtime moves
  from `SingleThreadRuntime` (rc-local) to the fork's `CurrentThreadRuntime`
  (`Arc<SmolExecutor>`, `Send` by construction). It lives inside the shared
  `Graph` (`Graph::attach_prop_store`), so `cargo check -p relate-vortex-lab`
  is the `Send + Sync` gate (ADR-0677 moved the store out of the publish
  closure).

## Engine A/B: the default store vs the attached Vortex property store (ADR-0665)

The round-1 engine A/B, run on `ccbf4a32` (release, toolchain 1.99.1). "The
default store" is the engine's compact live records ([ADR-0033]) plus the
[ADR-0614] postings; "the Vortex store" is the same graph with the
experimental `VortexStore` attached (`Graph::attach_prop_store`, ADR-0677)
and `PropStore::point` routed to it for the
keys its columns carry (`has-column` fallback to the compact records for a
key it does not promote).

**What this A/B covers, and what it does not.** Property *point* reads go
through the seam that the query evaluator already uses for a bound value's
property (`eval::prop` → `node_prop_id_at` → `PropStore::point`), so property
projections and property evaluation read from the Vortex store. The
scan-predicate candidate gate (`ScanNode::Live`, which reads the live record's
one-slot props directly) and the label/equality index lookups still read the
compact records — those inserts are the `candidates funnel`/`stats ladder`
rungs the mapping table lists and are not wired in round 1. Both stores stay
resident (the compact records are never dropped), so this measures the
property-read slice, not the replace-the-store memory thesis.

Harnesses (ADR-0677): `relate-vortex-ab run` (the 30-query SF0.1 suite) and
`relate-vortex-ab pokec` (the pokec groups) in `testing/relate-vortex-lab`;
both attach the store after load through the suite's `attach` hook. The
default variant runs the plain `query-latency`/`pokec_profile` binaries.

### Query latency, LDBC SF0.1 (30 queries, both runs `ok`)

Both runs reuse the same read-only checkpoint store
`graph-benchmark-ldbc-sf0.1.rl` (327,588 nodes / 1,477,965 rels), so the graph
is identical and only the property-read path differs.

- default run id: `sf0.1-default@1795493186` · peak RSS **922,484,736 B (~880 MiB)**
- vortex run id: `sf0.1-vortex@1795493220` · peak RSS **1,282,064,384 B (~1.22 GiB)**
- box load at the runs: 50.4 / 44.5 / 50.5 (1/5/15 min), shared host with
  other builds running — the per-query p50 is noisy between runs, so read the
  direction, not the third digit.

| q | default p50 ms | vortex p50 ms | v/d | rows |
|---|---|---|---|---|
| 1 | 0.046 | 0.045 | 0.99 | 1 |
| 2 | 0.099 | 0.098 | 1.00 | 2 |
| 3 | 0.028 | 0.029 | 1.06 | 1 |
| 4 | 0.029 | 0.030 | 1.02 | 1 |
| 5 | 0.373 | 0.544 | 1.46 | 1 |
| 6 | 0.026 | 0.027 | 1.02 | 2 |
| 7 | 0.022 | 0.012 | 0.53 | 0 |
| 8 | 0.358 | 0.168 | 0.47 | 1 |
| 9 | 0.043 | 0.023 | 0.53 | 1 |
| 10 | 0.059 | 0.031 | 0.53 | 3 |
| 11 | 5.392 | 3.469 | 0.64 | 1 |
| 12 | 0.397 | 0.569 | 1.43 | 1 |
| 13 | 0.528 | 0.642 | 1.22 | 1 |
| 14 | 0.024 | 0.024 | 1.01 | 1 |
| 15 | 0.026 | 0.027 | 1.02 | 1 |
| 16 | 0.044 | 0.056 | 1.28 | 1 |
| 17 | 1.012 | 1.027 | 1.01 | 1 |
| 18 | 0.306 | 0.371 | 1.21 | 1 |
| 19 | 2.967 | 6.354 | 2.14 | 1 |
| 20 | 0.478 | 1.064 | 2.23 | 1 |
| 21 | 0.020 | 0.041 | 2.02 | 1 |
| 22 | 0.027 | 0.015 | 0.58 | 1 |
| 23 | 0.024 | 0.013 | 0.53 | 1 |
| 24 | 0.021 | 0.011 | 0.52 | 1 |
| 25 | 0.023 | 0.012 | 0.51 | 1 |
| 26 | 0.019 | 0.009 | 0.48 | 1 |
| 27 | 3.369 | 1.519 | 0.45 | 1 |
| 28 | 0.058 | 0.027 | 0.48 | 1 |
| 29 | 0.026 | 0.012 | 0.46 | 1 |
| 30 | 0.157 | 0.068 | 0.43 | 1 |

**Reading.** All 30 answers and row counts match, so the Vortex-backed reads
are correct. The p50 direction is mixed and flips across runs: the
property-heavy projections that decode through the per-chunk `repeated_probe`
arm (q19, q20, q21) run ~2x slower, while the queries whose projection pulls
whole/dictionary columns (q7–q10, q22–q30) run up to ~2x faster. At this
workload the property-read slice is a small fraction of the scan/traversal
cost, and the host load dominates run-to-run variance, so the engine A/B is
**neutral**, not a win — the honest round-1 outcome for a store whose
decoded-cache cost (memory tables 2–3: `repeated_probe` retains nothing) is
paid against a compact read that is already a pointer chase.

### Pokec MEDIUM fixture: 100,000 users / 1,768,164 friendships

The `testing/relate-test-support/src/pokec.rs` `MEDIUM` preset opens into both
variants; all 36 `pokec_profile` groups run against both. The fixture is
deterministic, so **every group's checksum and row count is identical** —
`pokec_profile` prints `checksum=` per group; all 36 matched
(the fixture's own correctness cross-check). Peak RSS of the load shape:
**2,433,843,200 B** default vs **2,432,925,696 B** vortex — dominated by the
1.77M-rel adjacency, not the (183 KB-compressed) property columns, so the
store adds no measurable resident here.

Per-group wall time (ms, single process per variant, one open each; box load
56.2 first, 50.4 for the re-run — noise, not a clean A/B):

| group | default ms | vortex ms | rows | checksum (both) |
|---|---|---|---|---|
| count | 0 | 0 | 2000000 | 5308871538000190 |
| scan_agg_global | 311 | 317 | 20 | 53088715570 |
| expansion_3 | 8521 | 12458 | 47189028 | 125260243827546756 |
| expansion_3_unrolled | 4832 | 9012 | 47189028 | 125260243827546756 |
| scan_expand_2hop_filter | 366 | 495 | 744868 | 1977204262387716 |
| shortest_pair | 19 | 26 | 128 | 339767782656 |
| shortest_path_fn | 18 | 21 | 128 | 339767782656 |
| shortest_expr | 19 | 22 | 128 | 339767782656 |
| all_shortest_paths | 25 | 27 | 2282 | 6057422429082 |
| shortest_postfix | 22 | 24 | 128 | 339767782656 |
| shortest_fan | 17735 | 18550 | 3199984 | 8494151989827984 |
| shortest_all_pairs | 110 | 366 | 2048 | 5436284455200 |
| memo_prop_read | 170 | 453 | 492290 | 1306752184721055 |
| memo_date_call | 402 | 1358 | 500570 | 1328730912888375 |
| memo_datetime_call | 492 | 1938 | 500570 | 1328730912888375 |
| memo_date_row | 888 | 2420 | 500130 | 1327562961150015 |
| memo_datetime_row | 1544 | 2283 | 500130 | 1327562961150015 |
| memo_labels | 587 | 730 | 1000000 | 2654435769000045 |
| memo_in_list | 431 | 279 | 220120 | 584294401472325 |
| memo_regex | 396 | 818 | 500130 | 1327562961150015 |
| memo_hub_star | 9269 | 5661 | 1090 | 2893334988211 |
| memo_site_where | 1000 | 506 | 600920 | 1595103542307525 |
| memo_site_project | 1561 | 713 | 30574080 | 81156931556267565 |
| memo_site_order | 1619 | 691 | 910 | 2415536549835 |
| in_param_ints | 657 | 212 | 499430 | 1325704856111715 |
| in_param_strings | 1938 | 1011 | 500130 | 1327562961150015 |
| memmemo_datetime_distinct | 2917 | 2759 | 900000 | 2388992192100003 |
| memmemo_datetime_control | 2781 | 2772 | 900000 | 2388992192100003 |
| memmemo_regex_distinct | 4828 | 7986 | 150039 | 398268888344994 |
| memmemo_in_ints_64k | 155 | 320 | 149829 | 397711456833504 |
| memmemo_in_strings_64k | 14920 | 32602 | 150039 | 398268888344994 |
| memmemo_in_strings_overcap | 18781 | 30922 | 150039 | 398268888344994 |
| memmemo_sites_combined | 50935 | 73108 | 550 | 1459939672951 |
| memmemo_with_chain | 3446 | 1450 | 300000 | 796330730700003 |
| memmemo_invariant_heavy | 3359 | 1381 | 4000000 | 10617743076000045 |
| memmemo_invariant_heavy_x4 | 3090 | 4326 | 8000000 | 21235486152000045 |

**Reading.** Identical checksums settle correctness; the wall times move both
ways (`expansion_3`/`memo_date_call`/`memo_datetime_call`/`memmemo_in_strings_*`
slower through the probe decode, `memo_site_*`/`memo_hub_star`/`in_param_*`/
`memmemo_with_chain` faster). Same verdict as the query suite: the property
slice is neutral here and the shared-box noise is larger than the signal.

### Before/after mode in the CI lane

`.github/workflows/vortex-lab.yml` gains the **Engine A/B (default vs vortex
store)** step: it runs `pokec_profile small` and a five-query subset
(`--qids 1,8,12,19,30`) through the plain binaries and through
`relate-vortex-ab`, writing each into the job summary. Both suites exit 1 on
an answer mismatch, so their exit is reported (not fatal) in this report-only
lane.

## Open

- `Ty::Temporal` column for date properties.
- Nullable-column re-measure for presence (filter section finding 6).
- Engine-side insertion of the `PropStore` seam is now wired at the point-read
  boundary (`PropStore::point` → the attached Vortex store, engine A/B above);
  the remaining rungs are the candidates funnel's scan tail
  (`lookup_nodes_upto_at`) and the `stats_short_circuit` ladder — the scan
  tail and the equality/label index serve still read the compact records (the
  A/B's "what this covers" note), so the scan-predicate gate and the index
  lookups are the next insertions. The trait + A/B live in the lab (`seam.rs`,
  `colstore.rs`, `ops/seam_bench.rs`); the mapping table above is the plan.
- A clean-box re-run of the engine A/B: this round ran on a shared host at
  load 44–73 with both stores resident, so the p50s are neutral within noise.
  The decoded-cache cost (memory tables 2–3) is the item to attack before the
  store can win the property-read slice.

### Upstream responses (2026-11-23, all four threads answered)

- **#11808 (limit early exit)** — answered by a contributor: `with_limit`
  *does* terminate the split loop early; the cost is **spawned-ahead IO
  that is not cancelled**. Mitigation today: `max_splits_in_flight` —
  **on main, not in 0.93**. Closed then reopened 2s later: it stays open
  as the tracking issue for lazy IO scheduling (the real fix). Measured
  on the fork: `max_splits_in_flight=1` takes the exists shape from
  103.5 µs / 751 allocs to **58.9 µs / 506 allocs** (43% faster, −33%
  allocs) — real savings, still ~57% of a full scan because IO read-ahead
  remains uncapped (the knob's docs say so explicitly). The capped seam
  expand improves to 113 µs / 906 allocs vs 206 µs / 1,145 uncapped (45%).
- **#8694 (Probe API)** — closed as shipped: the API exists; #9971 tracks
  the per-encoding migration (`scalar_at` → `probe_scalar`, state carried
  per encoding + child probes). Our 0.93 already carries
  `array.probe()` / `repeated_probe()` — the seam's decoded-cache design
  maps onto it directly (switch pending in the lab queue). A maintainer
  comment proposes downcasting probes to canonical types to avoid Scalar
  wrapping — exactly our ~9-allocs/string-value cost.
- **#9551 (membership masks)** — a contributor has prototyped the sorted
  `IN` wiring: `col IN (list)` today runs one full-column Eq per literal
  (O(list × column)); the prototype sorts the literal list once and uses
  `sorted_membership` when the column's `IsSorted` stat is known — the
  same design as our `member`, and double confirmation that persisted
  sortedness (our sidecar) is the enabling stat.
- **#8948 (indexed layout)** — active but "still figuring things out";
  arm-A stands until their shape lands.

**Lab now pinned to the relatedb fork** (`github.com/relatedb/vortex` at
main, `0.93.0-343`): the fork carries `max_splits_in_flight` (unreleased
upstream) and becomes the home for our patches (native aggregation). Zero
API drift across the 343 commits — the lab compiled unchanged; the newer
upstream code already improved the file-prune scan baseline (O4b:
292 → 162 allocs).

- **Morsel gather — designed, not landed.** #8694 (open, 2026-07-09):
  "Probe API: encoding-owned repeated random access with density-hinted
  canonicalization caching" — names the exact failure modes measured here
  (per-value probe on compressed arrays, small takes over one encoded
  column), proposes decode-once / per-block-cache policies (prior art:
  Velox `DecodedVector`, DuckDB `UnifiedVectorFormat`). #9551 (open):
  vectorized membership masks for sorted arrays — the batched key check.
  #8948 (open epic): first-class index layout (`vortex.indexed`). The
  2024-era row-selection pushdown (#1202, #1938) shipped as the
  `with_row_indices` measured above, including its sparse fallback.
  Use-case comments + update request posted on all three (2026-11-23).
- **Exists early-exit filed**: vortex-data/vortex#11808 (2026-11-23) —
  `with_limit(n)` truncates output without stopping decoding; no cheap
  exists over a scan unless the file-level prune fires.
- **Stats layer mid-rework**: #4581 (file stats + zone maps computed
  twice), #11177 (retire legacy statistics APIs), #7939 (multi-resolution
  zone maps). Persisting `IsSorted` is AND-mergeable and fits their
  mergeable-stats rule — small after #11177. Store-side workaround for
  round 1: record per-column sortedness in our own metadata at seal time
  (the sorted-key arm is sorted by construction regardless).

### Store-side rule: shape our additions like their proposed APIs (adopted 2026-11-23)

Round-1 store additions are written against seams that mirror the upstream
proposals, so flipping to the real APIs when they land is a delegation
change, not a redesign:

- **Sidecar sortedness** flags mirror the `Stat` names (`IsSorted`,
  `IsStrictSorted`) behind one stats-shaped accessor — when the footer
  persists them, the accessor reads the file instead.
- **Morsel read path** goes through a single probe-shaped seam carrying a
  density hint (contiguous range / scattered ids / per-value) — the
  #8694 Probe API's shape; when it lands, the seam delegates to
  `ArrayRef::probe`.
- **Batched key check** is a sorted-values × sorted-members → mask
  function, matching #9551's proposed op.
- **Exists** is written against a limit-shaped terminal that assumes
  short-circuit — #11808 landing changes performance, not the operator.
