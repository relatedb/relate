# Investigating a slow query

The query-latency suite (`testing/relate-query-latency`, ADR-0243)
times the 30 graph-benchmark-ldbc queries; its `profile` command (ADR-0244)
shows where one query's time goes. This page is the loop for using them.
Flags and outputs are specified in
[`docs/datasets.md`](../datasets.md#query-latency-graph-benchmark-ldbc-workload-adr-0226).

## The loop

1. **Measure.** Time the query on its own, long enough for a stable p50:

   ```bash
   cargo run --release -p relate-query-latency -- run --qids 19 --min-time 5
   ```

   The table prints engine-elapsed p50, mean and p99 and the query's
   `db hits`; the full report is
   `target/query-latency/report-graph-benchmark-ldbc-sf0.1.json`.

   `db hits` is the store reads of one untimed PROFILE execution (the plan
   tree's `db_hits`, summed). It is deterministic for a fixed graph and
   plan, so it needs no quiet machine: a change that moves wall time but
   not `db hits` is CPU work, and one that moves `db hits` changed the
   plan or the walk. The compare gate (`scripts/compare-query-latency.py`)
   holds it in every environment class: report `db_hits` must stay within
   max(1.05 x envelope, envelope + 64) per query (env knobs
   `QUERY_LATENCY_DB_HITS_RATIO`, `QUERY_LATENCY_DB_HITS_FLOOR`); a
   decrease passes, so re-freeze the envelope to ratchet an improvement in.
   An envelope frozen before the field existed carries no `db_hits` and the
   band is skipped until the next freeze.

2. **Profile.**

   ```bash
   make query-latency-profile QID=19
   ```

   This builds with the `profiling` cargo profile (release code with line
   tables), loads SF0.1, checks the answer, then samples the query in a
   loop for 10 s. Read `target/query-latency/profile/graph-benchmark-ldbc-sf0.1-q19/top.txt`.

3. **Read `top.txt`** top to bottom:
   - *Self time by group*: which code is on the CPU. A large `allocator`
     share means allocation in a hot loop (clones, temporary vectors or
     maps); `std/core/alloc` is mostly inlined collection and hashing code
     called from the engine; a crates.io crate (`dashmap`, `indexmap`,
     `hashbrown`) points at the data structure the engine leans on.
   - *Inclusive time by engine crate*: which engine layer the time sits
     under. `relate` (the API) is on every sample; look below it.
   - *Top functions by self time*: the hot leaves, each with its source
     file. Start reading the code there.
   - *Top functions by inclusive time*: the call path that owns the time
     (frames on 99% or more of samples, the thread entry and dispatch, are
     left out). A path that is 90%+ of the profile tells you which
     operator or phase to look at.

4. **Form one hypothesis and change one thing.** Check it against the
   source file `top.txt` names.

5. **Re-measure and re-profile** with the same commands, on the same
   machine and build. Compare the p50 from step 1 and the group shares in
   `top.txt`. `run` also re-checks the answer.

6. **Before the PR,** run the whole SF0.1 suite (`run` with no `--qids`, and
   `RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p
   relate-query-latency --test graph_benchmark_ldbc`). For a change whose effect grows
   with data size, check SF1 locally too (`--sf 1`; about 10 minutes and
   12 GB).

## Faster iterations

Each `profile` or `run` loads SF0.1 into memory, about 26 s. To skip that
while iterating, build a store once and reuse it:

```bash
cargo run --profile profiling -p relate-query-latency -- profile --qid 19 \
  --store target/query-latency/graph-benchmark-ldbc-sf0.1.rl
```

The first run loads into the store and marks it complete
(`graph-benchmark-ldbc-sf0.1.rl.query-latency.json`); later runs open it read-only in a few
seconds. Delete both whenever the change touches the loader, storage or
indexes: a store built by an older commit does not reflect them.

## plan_bench — PLAN-phase micro-harness

`relate-query-latency` also ships `plan_bench`, a dev tool (not CI-gated)
for the plan phase alone: it loops `EXPLAIN` over every `.cypher` query in
a directory against the suite's prebuilt store and reports the
per-statement phase split (parse / plan / execute, from the always-on
phase timing, ADR-0491) plus exact heap-operation counts from a counting
global allocator:

```bash
cargo build --release -p relate-query-latency --bin plan_bench
target/release/plan_bench \
  target/query-latency/graph-benchmark-ldbc-sf0.1.cl \
  testing/relate-query-latency/workloads/graph-benchmark-ldbc/queries 15
```

Usage: `plan_bench <store.cl> <queries-dir> [seconds]` (seconds defaults
to 15, after one warm-up pass). The store is the same build artifact the
`profile` loop reuses above; time the tool in a quiet window — its
per-statement figures are wall-clock medians and CPU contention shows up
in them directly.

## handoff_bench — statement-handoff micro-harness

`relate-query-latency` also ships `handoff_bench`, a dev tool (not
CI-gated) for the per-statement cost of the query-worker handoff an
unasserted connection pays (ADR-0259): it loops the sqlite-gate PR
workload's 8-statement brief mix (verbatim) on an evidence-shaped
store sized to the gate's smallest band, in two interleaved modes —
**(a)** an unasserted
connection on the main thread (what the Python wheel, the FFI surface
and the CLI drive) and **(b)** a connection on a thread spawned with the
engine's exported `QUERY_WORKER_STACK_BYTES` budget after
`assume_worker_stack_on_caller_thread()` (ADR-0504) — and reports the
per-statement p50 of each plus their signed gap, per phase:

- **cold** — one mix pass on each of `--cold-stores` fresh stores per
  mode (every pass first-execution; the per-connection worker spawn is
  primed away with one untimed worker-routed statement, so the ~0.5 ms
  one-time spawn cost cannot swamp the µs-scale handoff term);
- **warm** — `--rounds` passes per mode after `--warmup` untimed ones.

```bash
cargo build --release -p relate-query-latency --bin handoff_bench
target/release/handoff_bench --rounds 1500 --cold-stores 40
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--rounds` | 1500 | timed mix passes per mode in the warm phase |
| `--warmup` | 200 | untimed mix passes per mode before the warm phase |
| `--cold-stores` | 40 | fresh stores per mode in the cold phase |

Reading the report: a ~0 gap is either both-inline (the ADR-0663
cache-hit route serving the warm hit) or both-worker (the handoff paid
by **both** modes and invisible to the diff — the var-length
`neighbourhood` statement, every multi-hop plan past the escalation
bound, every write); a ~5-7 µs gap is a statement only the asserted
mode serves inline. The mix's known classes are printed with the
report. Time it in a quiet window — the figures are wall-clock medians
at microsecond scale.

## External samplers

`--sampler none` runs the same checked loop without the in-process
sampler, so another profiler can record the process:

```bash
cargo build --profile profiling -p relate-query-latency
# macOS or Linux, no sudo (the load is recorded too; select the loop in the UI)
samply record target/profiling/query-latency profile --qid 19 --sampler none --seconds 20
# Linux
perf record --call-graph dwarf -- target/profiling/query-latency profile --qid 19 --sampler none --seconds 20
```

## Caveats

- Profile the `profiling` build, never a debug build: debug code spends
  its time in places release code does not.
- Shares under about 1% are noise at the default 10 s and 997 Hz. Lengthen
  `--seconds` for more samples.
- The `allocator` share moves between runs on macOS: two back-to-back q19
  profiles of the same build, at the same p50 (676 and 681 ms), gave 20%
  and 35%, almost all in `dealloc`. Profile twice or more before acting
  on an allocator difference, and let the p50 decide whether a change
  helped.
- Frames are attributed by their source file when the line tables have
  one, else by symbol name. Inlined frames often have bare names
  (`match_node`); `top.txt` shows them with their source file.
- On Linux, pprof-rs drops samples inside libc, libgcc, pthread and the
  vDSO (its unwinder is not signal-safe there), so glibc `malloc` time is
  missing; the Rust allocator entry points (`allocator`) still show.
- The flame graph (`flame.svg`) is pprof-rs's own rendering, with the
  thread name as the root frame.
- The 30 queries all carry a predicate or a relationship pattern; none is
  the single-label property shape the index-backed aggregate shortcut
  recognizes ([ADR-0611](../adr/0611-index-backed-aggregates.md)), so the
  shortcut never fires in this suite and its timings do not cover it.

## Example: q19 at SF0.1

On 0.12.1 (Apple M4 Max), q19 takes about 0.7 s at SF0.1 and 9.5 s at SF1;
every other query at SF0.1 is under 6 ms. Its profile:

- 99% of samples sit under `match_one_path` → `match_node` →
  `expand_rel` → `walk` → `for_each_typed` in `relate-executor`, and
  `match_node` alone is about 36% of self time: the time is pattern
  expansion, not planning or result handling.
- `allocator` is about 15% of self time, `dashmap` about 10% (node
  lookups by id), `indexmap` about 7%, with SipHash (`write_u8<Sip13Rounds>`)
  visible: per-row work inside the expansion.

That is consistent with the expansion starting from every `Comment`
instead of the tags that match `CONTAINS "Copernicus"` (K98's ordering).
It is a lead for the next step, not a confirmed cause.
