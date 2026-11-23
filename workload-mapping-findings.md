# Workload mapping for the vortex property store (REL8-1982 scope 3)

## Operator kernels (the bench's unit of measurement, 2026-11-23)

Derived from the whole corpus: 30 LDBC + fraud-qpp (f01-f06) + pathdb
(p001-p012) + snb-bi (bi-01..13) + equi-join (e01-e03) + ukrailway (r02-r03).
Each kernel is what the bench implements and reports (bytes read/decoded,
allocs, ns) — the operator, not the query, is the unit.

| # | Operator | Exercised by | Vortex mechanism | Optimize for |
|---|---|---|---|---|
| O1 | Point property read by node-id | every projection; pathdb seeds; fraud ring return | node-major store, row index == node-id, one column's segment slice | decode ONE cell, not the chunk (scalar-at-row); mmap segment cache |
| O2 | Multi-column point read, same rows | q3 (firstName+lastName+org.name), q9, fraud `[n.id]` | struct take: one row-id list, k column gathers | one gather plan over k columns, shared row mask, no per-column search |
| O3 | Equality seek by business key | 16 LDBC queries, fraud `{id:}`, pathdb `$seed`, bi-07 `$tag` | arm A (key, node_id) sorted index, IsStrictSorted stat; or arm B dictionary | seek-not-scan; dict lookup O(log dict) |
| O4 | Range filter -> count/exists | q4, q8, q20 + the 8 `COUNT(..)>0` existence queries (q21-q28) | MEASURED (filter.rs): zone-map prune exists at FILE level via `can_prune` (22.7 us, decode-free); per-split prune needs `LayoutReader::pruning_evaluation` (36 us/5 splits) — no public "which splits prune" call (API gap). **No exists early-exit**: `.with_limit(1)` costs 98% of the full count (2.71 vs 2.77 ms) — the limit truncates output, not decoding. Cheap negatives come only from the file-level prune. No popcount/mask terminal either, so counts materialize the projected column at survivors |
| O5 | Conjunctive filter | q24 `l.name=X AND t.name=Y` shape (16/30) | MEASURED (filter.rs): pruning IS per-column (struct reader splits And/Or into per-column masks and ANDs them) and **pruned zones are the UNION of the conjuncts' pruned sets** (a zone survives iff EVERY conjunct keeps it) — measured 3/5 ∪ 1/5 → 4/5 on the monotone store (corrected 2026-11-23; the earlier "never more than the most selective conjunct" was the wrong direction, derived from flat-store 0/5-everything numbers). The real win: one scan beats executor-side sequential re-filter by **1.9x time and 8x memory traffic** (8.14 ms/515 KB vs 15.6 ms/4.19 MB), because the sequential path materializes survivor ids and gathers them |
| O6 | Group-by + aggregate + top-k | q11, q17, q19, bi-07, bi-02, bi-13 | dictionary = the group universe; Min/Max/Sum chunk stats (Sum confirmed mergeable via merge_sum; RowCount scalar fn exists) | group over dict codes (cardinality = dict size, not row count); stats-served partials; top-k over dictionary; **stats-only aggregates**: COUNT = len - NullCount, SUM = sum of chunk Sums, AVG = sum Sums / sum (len - NullCount) with zero decode — exact when unfiltered OR when min/max proves a chunk all-matching (partial-match chunks still decode); bench float NaN awareness (NaNCount exists) and integer Sum overflow width |
| O7 | Join on property key | equi-join e01-e03 (`a.birthday = b.birthday`), q9 | key-sorted index on both sides | sort-merge join instead of hash — no build side, no hash table |
| O8 | Intra-row property compare | fraud f05 `ti.date < tj.date` | two aligned gathers from the node-major store | batched two-column compare kernel, not per-row point reads |
| O9 | Property reads mid-traversal | fraud per-hop `{test: true}`, pathdb expansions, ukrailway | O1 in traversal order | locality: node-major rows put id-adjacent nodes row-adjacent; measure whether traversal order benefits |
| O10 | Batched key check from a morsel (1024 keys at once) + batched predicate multi-check | every operator that today loops per-row: q20's per-row `lookup_range_seek` closures, `node_prop_resolved` per row; the ADR-0555 morsel boundary is the natural pushdown seam | the batch is itself a vortex array: keys-in -> is_in-style kernel against the key column / dictionary; rows-in -> take-gather exactly those rows from compressed columns, compare over the gathered slice, return a 1024-bit mask | amortized decode (one kernel call, not 1024); two seek strategies to bench: 1024 binary seeks (order-preserving) vs sort-batch + one merge pass (cheaper on compressed data but needs a scatter to restore input order); output = mask or node-id vector aligned to the morsel's row order |
| O11 | Row-position-carrying operators (user 2026-11-23): keep the store row position of a node's attributes from the lookup onward, use position sets for later filter/merge/compare | `n.age < m.age` (aligned two-column compare at two bound row sets), `n.id = m.id` (row equality), `m.age > min(n.age)` (aggregate over a row set as a compare bound), conjunctive filters as position-set algebra | identity: in the node-major store row position == node-id, so the executor's existing node-id binding IS the row handle — no new handle type; gathers/compares/intersections at positions, values decoded only where an operator's output needs them; stats fast path when a position set is chunk-aligned (Min/Max served free) | measure: compare-at-positions vs materialize-then-compare; the identity breaks if rows are ever reordered (O9 locality tension — bench both); position stability under MVCC overlay/seals = the round-2 translation problem, stakes raised; cross-label compares require ONE global row space (single node-major table, sparse per-label columns) — pinned design decision |

Cross-cutting wins the store enables (what "optimized" means here):
1. **Late materialization** — properties decoded only for surviving rows;
   attacks the 927-allocs/stmt materialization on the projection side.
2. **Stats-served short-circuits** — exists/count/aggregate answered from
   chunk stats alone when pruning is total.
3. **Seek-not-scan** — sorted/dict keys turn filters into binary seeks.
Out of scope for the store: CONTAINS (text index), DISTINCT over un-pruned
rows (q12/q13 count-distinct still reads survivors).

Stats pushdown framing (user 2026-11-23): every PROMOTED column gets
min/max/counts/pruning — the residual column's lack of per-key stats is the
exception, not the story. The promotion threshold is the dial for how much
of the property space is prunable; the bench measures where it should sit
(promote-more = more columns but per-key pruning everywhere) and the
seal-time per-key aggregates over the residual are an optional probe, not a
prerequisite.

## Second sweep (2026-11-23): forwards from query languages + backwards from vortex

Confirmed 0.93 Stat set: IsConstant, IsSorted, IsStrictSorted, Max, Min,
Sum, NullCount, UncompressedSizeInBytes, NaNCount (NO TrueCount; bytebool
gives 1-bit booleans instead). UncompressedSizeInBytes = store-side
per-chunk size accounting, cross-check for the container-harness smaps.

New kernels:

| # | Operator | Exercised by | Mechanism |
|---|---|---|---|
| O12 | STARTS WITH prefix range | string-key queries | prefix = range predicate on a sorted string column: binary seek; ENDS WITH only via optional reversed-sort variant |
| O13 | countDistinct | q12, q13, bi-07 | dictionary IS the distinct set: unfiltered = dict size (no decode); filtered = per-code popcount after mask |
| O14 | Histogram / quantiles | pokec age/gender analyses; percentileCont/Disc (an ADR-0555 breaker today) | (a) dict+runend frequency, (b) equi-width bins from chunk Min/Max, (c) approx quantiles from per-chunk min/max; exact quantile = position seek on IsStrictSorted |
| O15 | ORDER BY / top-k / SKIP-LIMIT | q11/q17/q19, bi-07 | order free on the sorted key column (IsSorted); per-chunk top-k merge otherwise; scan selection for LIMIT |
| O16 | Presence / IS NULL / label prune | lenient property presence; fraud per-hop; label scans | NullCount prunes all-null chunks; sparse encoding = presence-bits for rare keys; label-presence per chunk = our own metadata |
| O17 | Anti-join (position-set complement) | bi-07 NOT pattern | complement over the dense global row space, composes with O11 algebra |
| O18 | IN-list as query operator | seed lists, parameter sets | same is_in kernel as O10a, operator-level shape |
| O19 | Traversal set state: seen/frontier as row-sets (QPP, shortest path, varlen) | every expansion loop: new = next - seen, seen += new, frontier filter = O10b over a row-set; shortest-path frontiers; DISTINCT-node path semantics (per-path sets stay executor-side) | MEASURED (sets.rs): HashSet<u64> **18.9 B/node** at 1M visited (the spec's "48+ B" was wrong — hashbrown is 18.9, and 11.8 at 100k from power-of-two rounding), HashSet<u32> 10.5, dense bitset 0.875 B/node at 7-bit spacing / **0.125 B/node at 1 bit/node** — 21.6x smaller. Frontier algebra (100 levels, 10k new/level): bitset 42-49 us/level, **0 allocs**, vs HashSet 151-171 us, 13 allocs, 262 KB/level — 3.4-3.7x faster, alloc-free. Order: arrival-order parallel Vec +9 us/level (+29%), sort-at-end 58 us, ascending bitset materialization 86 us (space scan) — keep the Vec. frontier ∩ predicate: bitset probe 6.6 us vs HashSet 65-76 us (10x). Crossover: bitset wins memory below ~1% density of the id space, time crossover ~12-15M bits for a 10k frontier; LDBC/Pokec reachability (~14% density) is deep in the win region |
| O20 | Distinct over multi-column projections | q5 DISTINCT firstName+lastName, snb-bi distincts | packed dictionary code pairs (u32+u32) — hash/compare codes, never decode strings; generalizes to k dict-encoded columns |

**METHODOLOGY CORRECTION (2026-11-23, from the filter kernel):** the
synthetic shapes in `store.rs` use `attrN = (i*(N+3)) % 1000`, which makes
every 8k-40k-row zone span the full value range — **zone-map pruning can
never fire on them**. All range-pruning numbers must come from a monotone or
clustered column (`id<75000` prunes 3/5 splits; `attr0<500` prunes 0/5).
`store.rs` needs a monotone shape before the next campaign. Also measured:
splits are 8k-40k rows (the prune unit, coarser than a morsel); a string
range predicate IS expressible (`gt_eq/lt_eq` on a Utf8 column with
`upper = prefix + "\u{10FFFF}"`) and prunes 4/5 splits, but is a loss
(10.7 ms) against decoding + in-memory compare (0.71 ms) or a binary search
(624 ns); and the presence finding was a lab write-shape artifact, not a
vortex limit (corrected 2026-11-23): nullable promoted columns express
absent-as-null directly (null = absent, no sentinel values), costing a
validity bitmap where nulls exist and giving `NullCount`-based IS NULL /
IS NOT NULL pruning — to be re-measured on a nullable table.

Cross-cutting vortex features newly mapped: page-level row-group skipping on
the mmap path (untouched segments never fault: per-query RSS == segments
touched — a measured bench effect, not an assumption); encodings ALP
(floats), FSST (strings — hottest keys), FastLanes, Pco, RunEnd, Sequence
(defaulted column ~0 bytes), Sparse (mostly-null), datetime_parts (q8
temporal range); roaring in the dep tree — whether 0.93 exposes roaring
arrays for masks/position-sets = bench-time check.

Low priority: window frames as position ranges (via O11), chunk-level
sampling for approx queries, dictionary as bloom pre-probe. Out: CONTAINS/
regex/fulltext (text index), vector (own index), spatial (plain range),
collect() (needs rows), adjacency-side (separate store).

Static classification of the 30 graph-benchmark-ldbc SF0.1 queries
(`testing/relate-query-latency/workloads/graph-benchmark-ldbc/queries/`),
2026-11-22. Measured time shares pending the profiled runs.

## Property-access patterns (static, from query text)

| Pattern | Queries | Count | Vortex angle |
|---|---|---|---|
| Point lookup by unique `ID` literal | q03 (Post.ID), q21 (Post.ID + name), q26 (Person.ID) | 3 | sorted unique-key column: binary seek + zone-map prune — the postings-retirement case |
| Equality on non-unique property | q01, q02, q06, q07, q09, q13, q14, q15, q17, q18, q22, q23, q24, q25, q27, q28 | 16 | equality postings (ADR-0614) today; dictionary/RLE vortex column + chunk stats |
| Range predicate | q04 (Comment.length > 100), q08 (Person.birthday date >), q20 (length > 1000) | 3 | chunk-statistics min/max prune |
| `CONTAINS` substring | q02, q05, q16, q19, q20, q29 | 6 | NOT vortex-servable (no zone-map prune for substrings) — trigram/text index stays |
| Pure structural (no property predicate) | q30 | 1 | — |

(Queries can sit in two rows: q02 equality+contains, q20 range+contains,
q21 point+equality, q26 point+equality.)

## Hot properties (by predicate + projection frequency)

- `name`: Place, Tag, Organisation, Tagclass — the single hottest key
- `firstName` + `lastName`: Person (usually paired)
- `ID`: Person, Post, Comment, Forum — unique business key
- `content`: Post, Comment (CONTAINS — text-index territory, not columnar)
- `length`: Comment (range); `birthday`: Person (date range);
  `title`: Forum (contains); `browserUsed`: Post (contains); `type`: Organisation

## What this predicts for the benches

- The seek bench (sorted unique `ID` column) covers 3/30 queries directly,
  but every query ends in property projection, so the columnar read path
  is on all 30.
- The equality filter bench should use `name`-like keys (high-cardinality
  dictionary-encoded strings) — that is 16/30 queries' entry predicate.
- The range bench should use an integer column (`length`) — 3/30.
- CONTAINS patterns (6/30) set a boundary on the claim: a vortex property
  store does not replace the text index; the ADR must scope adopt to
  equality/range/point patterns.

## Measured (d98aee8f, SF0.1, 2026-11-22 21:17-21:28, box load 16-64 — noisy)

### Suite p50 (engine-elapsed, store reused, --min-time 5)

Fast band (p50 <= 0.03 ms, 100k rounds — fixed-cost dominated): q14 0.012,
q15 0.013, q23 0.012, q24 0.011, q25 0.012, q26 0.010, q29 0.013, q22 0.015,
q4 0.017, q21 0.021, q6 0.026, q28 0.027, q3 0.028, q16 0.028.
Slow band: q19 2.135, q11 1.786, q27 1.339, q5 0.666, q8 0.389, q17 0.337,
q20 0.321, q13 0.170, q12 0.132, q18 0.120, q2 0.101, q30 0.064, q10 0.046,
q1 0.045, q9 0.041, q7 0.024.

All 30 `ok` (answers checked). Absolute p50s are noise-affected (load spiked
to 64 mid-run); ordering and the fast-band/fixed-cost split are the signal.

### plan_bench (EXPLAIN loop, per-statement)

Aggregate (30 queries): parse 35.2 us, plan 4.8 us, execute 9.5 us;
**927.6 allocations and ~143 KiB requested per statement**.

Per representative query (allocs/stmt | parse/plan/execute us):

| Query | Pattern | allocs/stmt | parse | plan | execute |
|---|---|---|---|---|---|
| q24 | equality (Person+Place+Tag) | 1002 | 100.8 | 15.2 | 34.6 |
| q03 | point by Post.ID | 838 | 95.9 | 21.9 | 30.8 |
| q04 | range (Comment.length) | 914 | 127.5 | 13.5 | 71.9 |
| q20 | CONTAINS scan (Comment) | 701 | 103.7 | 10.0 | 32.1 |

### Profile finding: pprof on fast queries measures thread lifecycle, not queries

`profile` on q24/q03/q04 put 99.5% of self time in three libsystem frames
(`_pthread_key_delete` 70%, `_pthread_attr_getguardsize`,
`_pthread_attr_setcpupercent_np`) — **not** a symbolication artifact: the
folded stacks are coherent thread start/exit paths
(`__pthread_exit; thread_start; Box::<F>::call_once; relate::query_worker::spawn`).
At 11-14 us engine time per statement, the sampled activity is the engine's
per-statement query-worker thread spawning and exiting. Reproduced on both
the 1.100 and the rustup 1.88 profiling builds, so it is not the toolchain.

Only the slow query gave real frames — q20 (222 us/stmt): `stub`
(relate-executor/src/store.rs) 0.6%, `drop_glue<[Value]>` 0.5%,
`contains<&str>` 0.4%, `lookup_range_seek` closure 0.4%, `node_prop_resolved`
(relate-names) 0.3%, `prop_read.rs` 0.3%, `label_probe::node_has_label_id_at`
0.2%, `apply_cmp` 0.1%, `bind_node_stub` 0.1%.

**Consequence for the vortex benches:** wall-clock profiling is the wrong
instrument for the fast band; per-statement allocation counts (plan_bench)
and engine-elapsed p50 are the load-robust measures. Property-access cost is
visible in wall time only for the slow queries; for the rest it is a share of
the ~700-1000 allocations per statement, not of wall time.

### Load path

SF0.1 load: 327,588 nodes / 1,477,965 relationships in 25.4 s (~77 us per
element) on d98aee8f, with `--store` writing the reusable artifact. Raw
samply capture at /tmp/vortex-load-q30.json (unsymbolized — atos
symbolication of a full load capture is a ~40 min background job; the load's
*memory* attribution already exists in the REL8-1949 learnings comment).
