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

## Open

- The engine-side A/B (CompactProps + postings) — needs the container
  harness or the `research`-feature read path; `CompactProps` is `pub(crate)`.
- `Ty::Temporal` column for date properties.
- Nullable-column re-measure for presence (filter section finding 6).
- Engine-side insertion of the (now designed and lab-validated) `PropStore`
  seam: at `node_prop_id_at`, the candidates funnel's scan tail
  (`lookup_nodes_upto_at`), and the `stats_short_circuit` ladder — the
  trait + A/B live in the lab (`seam.rs`, `colstore.rs`,
  `ops/seam_bench.rs`); the mapping table above is the wiring plan.

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
