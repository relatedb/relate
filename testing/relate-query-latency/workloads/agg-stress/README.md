# agg-stress — the aggregation-stress corpus (LABS-733)

Five IN-HOUSE queries over the SAME pinned graph-benchmark-ldbc SF0.1 store the `graph-benchmark-ldbc`,
`pathdb` and `snb-bi` lanes run (`workloads/graph-benchmark-ldbc/`, ADR-0243 — no new
data), written as the **measuring net for the planned chunk-native
streaming fold** (LABS-685's follow-on). LABS-685's verdict: radix
partitioning alone buys ≤15% on shapes no shipped workload has; the real
target is a fold at the pipeline breaker that consumes chunks natively,
estimated 4-6x on high-cardinality folds and ~2x on low-cardinality
ones. No shipped workload's result stream is dominated by rows flowing
into an aggregation — the graph-benchmark-ldbc lane's queries fan out to application
shaped answers, the pathdb lane's RPQs return paths, snb-bi's analytics
are index- and join-bound. This workload exists so that fold
optimization lands as a lane-level timing delta instead of noise:
every entry is a bounded `knows` walk whose enumeration IS the fold's
input stream, and the five entries spread that stream across the group
cardinalities the estimate distinguishes.

## Provenance and license

- Source: IN-HOUSE (2026-11-02), written for this workload. No external
  query was translated or copied — the shapes (trail-count endpoint
  fold, composite-key fold, min/max per key, low-card control,
  collect-reach) are synthetic stress designs over the graph-benchmark-ldbc vocabulary.
- Data: the pinned graph-benchmark-ldbc store at SF0.1 (CC BY 4.0,
  `workloads/graph-benchmark-ldbc/data.sha256`) — 327,588 nodes / 1,477,965
  relationships, of which this workload touches `Person` (1,528 nodes)
  and `knows` (14,073 edges; average out-degree ~9.2, maximum 243).

## Why these queries — the stream and the fold

The workload's one ingredient is the bounded variable-length `knows`
walk. Its enumeration at SF0.1, measured by execution (release build,
2026-11-02):

| Bound | Walks enumerated | Measured cost (count fold) |
| --- | --- | --- |
| `*1..2` | 254,463 | ~0.9 s |
| `*1..3` | 2,624,450 | ~10.7 s |
| `*1..4` | 23,050,339 | ~6.7 min, 61 s of it system time — memory pressure |

`*1..4` is the measured reason the headline bound is `1..3`: the
workload's budget is seconds-to-tens-of-seconds per entry, and `1..4`
is minutes-scale under swapping. The bound is recorded here so a future
engine (the chunk-native fold itself, or the frontier kernel) can
re-measure and raise it deliberately.

Per query — the stream is the walk enumeration, the fold is what it
flows into (timings from the landing lane run, release, `:memory:`
store, 2026-11-02; `rows` is the recorded oracle's):

| Entry | Stream (rows into the fold) | Groups | Aggregates | Time | Rows |
| --- | --- | --- | --- | --- | --- |
| a01-endpoint-trail-count | 2,624,450 (`knows*1..3`, path variable) | 1,205 (endpoint ids) | `count(p)` | 10.3 s | 10 |
| a02-composite-key-fold | 2,624,450 (`knows*1..3`, path variable) | 391,109 ((src, dst) pairs) | `count`, `min`/`max`/`avg(length(p))` | 15.3 s | 10 |
| a03-min-max-per-key | 2,624,450 (`knows*1..3`, no path variable) | 1,199 (source ids) | `min`/`max(creationDate)`, `count` | 6.1 s | 10 |
| a04-low-card-control | 2,624,450 (`knows*1..3`, no path variable) | 10 (`ID % 10` buckets) | `count`, `count(DISTINCT)` | 5.8 s | 10 (complete) |
| a05-collect-reach | 254,463 (`knows*1..2`, no path variable) | 1,199 (source ids) | `size(collect(DISTINCT))`, `count` | 0.57 s | 10 |

What each entry is for:

- **a01 (headline)** — the trail-count endpoint fold: `count(p)` over
  every bounded walk, folded per destination. The path variable keeps
  the per-row path value in the stream — the shape a fold that never
  materializes the row benefits from most.
- **a02** — the high-cardinality composite case: the same stream folded
  into 391k groups with count and per-key min/max/avg. LABS-685's 4-6x
  estimate is for exactly this shape. Its `ORDER BY src, dst LIMIT 10`
  sorts the fold's output — 391k rows, an order of magnitude below its
  2.6M-row input, so the fold stays the dominant cost.
- **a03** — min/max per key over a big stream, keyed by source id: the
  mid-cardinality case, no path variable (the frontier declines it), so
  the stream is the enumeration without path values.
- **a04** — the control: the SAME stream and bound as a03, keyed on a
  ten-bucket attribute instead. The a03/a04 pair separates stream cost
  from group-count cost — today they measure 6.1 s vs 5.8 s, i.e. the
  fold's group handling is a small fraction of the stream cost; an
  optimization that only speeds up group handling cannot move a04 much
  more than that, one that speeds up row ingestion moves every entry.
- **a05** — the collect-heavy variant: a list-accumulating fold
  (`collect(DISTINCT ...)`, sized by `size()` so the recorded answer is
  deterministic where collect's own list order is not), over the
  smaller `1..2` stream. Memory stays bounded (1,199 per-source lists
  of at most 875 distinct ids).

Design note: the ticket's literal RETURNs (every endpoint's count,
every key's min/max) are shipped with a trailing `ORDER BY ... LIMIT
10` — the recorded oracle stays ten compact rows without touching the
stream the fold consumes (the LIMIT applies after the pipeline
breaker). The full group counts live in the table above, measured by
execution; a04's ten rows are its complete fold output.

## Oracle

Engine-self, recorded once per scale factor in `oracle-sf<SF>.json`:
every value is the answer this build returns over the pinned graph-benchmark-ldbc SF0.1
data (verified by execution 2026-11-02, release, reused store), NOT an
upstream answer — no upstream query of these shapes exists. The oracle
gates answer regressions in the fold itself: a fold rewrite that drops
or double-counts rows fails the run. Rows compare with the graph-benchmark-ldbc lane's
no-coercion rules (`answers::compare`); every recorded `avgHops` is an
exact binary fraction, so the float comparison is bit-stable.

Recorded at SF0.1 only: a `--sf 1` run fails loudly at its oracle load
(no `oracle-sf1.json` exists) — the same rule as snb-bi's params files.
Regeneration: re-freezing the graph-benchmark-ldbc data set regenerates this file (run
the lane, review each answer, re-record; the stream and group sizes in
the table above move with the data).

## Dialect check

Every shipped form was verified by execution over the graph-benchmark-ldbc SF0.1 store
(2026-11-02) before any file was marked `ok` — all supported, no engine
gap found:

| Form | Where | Verdict |
| --- | --- | --- |
| `count(p)` over a bounded `*1..N` range | a01, a02 | supported — the LABS-646 `count(p)` defect is fixed-length-only |
| `length(p)` in aggregates (`min`/`max`/`avg`) | a02 | supported |
| Arithmetic key expression (`a.ID % 10`) as a grouping key | a04 | supported |
| `count(DISTINCT ...)` inside a fold | a04 | supported |
| `size(collect(DISTINCT ...))` | a05 | supported — deterministic via `size()` |
| `ORDER BY` an aggregate alias (+ tiebreak) `LIMIT` | a01, a02, a03, a05 | supported |
| `min`/`max` over the raw ISO `creationDate` string | a03 | supported — uniform format, lexicographic = temporal order |

## Execution policy

Run-and-report (ADR-0289 memory-shape precedent): nothing here enters a
frozen gate compare set and no latency number gates a PR — the
workload IS the measurement, so its timings are recorded (here and in
every `report-corpus-agg-stress-sf<SF>.json`), never compared by a
gate. What gates is the ratchet: every entry is `ok`, must answer
within the timeout and match its oracle row exactly.

Timeout: the lane's own flat 60 s per entry (`Lane::default_timeout`),
~4x the slowest measured entry (a02, 15.3 s) — generous rather than
tight for the same reason, and never the graph-benchmark-ldbc-store lanes' 10 s SF0.1
default, which a01 and a02 are both past. Watchdog-cancelled; a
timed-out `ok` entry fails the run.

Peak RSS of the landing lane run: 10.86 GiB (the `:memory:` graph-benchmark-ldbc load
is ~1.9 GiB of it; the fold queries' materialized streams dominate the
rest — the exact shape the chunk-native streaming fold targets). The
gated lane test needs a machine with that much headroom; the CI corpus
stage does not run it (it runs the `graph-benchmark-ldbc` test only), it is a
local/agent invocation.

## How to run

```bash
cargo run --release -p relate-query-latency -- corpus agg-stress --sf 0.1
# the gated test (loads the graph-benchmark-ldbc SF0.1 graph, ~26 s, then the five entries):
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus agg_stress
```
