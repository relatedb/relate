# Workload mapping for the vortex property store (REL8-1982 scope 3)

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
