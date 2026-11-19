---
type: adr
area: infra
date: 2026-10-13
name: 0243-query-latency-suite
status: accepted
tags:
  - latency-suite
title: Query-latency suite from the graph-benchmark-ldbc workload
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0130-cookbook-tested-recipes]]"
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0221-engine-reported-statement-elapsed]]"
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0244-in-process-query-sampling-profiler]]"
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
  - "[[adr/0246-defer-query-latency-plan-goldens]]"
  - "[[adr/0247-query-latency-sf0-1-literals]]"
---

# ADR-0243: Query-latency suite from the graph-benchmark-ldbc workload

> Renamed from [ADR-0226](./0226-ontology-mappings.md): the number collided with
> docs/adr/0226-* on main (both were taken by parallel PRs). The
> decision is unchanged; references were updated in the same change.

## Context

The repo has no read-latency regression signal on a realistic graph. The
frozen loadtests ([ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md), [ADR-0070](./0070-docker-baseline-loadtest-environment.md)) are synthetic mixed read/write
workloads measured as throughput and per-use-case p99; the TCK, CIP-eval,
cookbook and graph-examples suites ([ADR-0144](./0144-graph-examples-differential-suite.md)) check answers, not time.

The `benchmarks/graph-benchmark-ldbc` harness in the
companion examples repository runs the 30 Cypher queries of
[prrao87/graph-benchmark-ldbc](https://github.com/prrao87/graph-benchmark-ldbc)
at `c8eb87f` (MIT, Copyright (c) 2026 Prashanth Rao) over the LDBC SNB
`CsvBasic-StringDateFormatter` data set (CC BY 4.0, LDBC/GDC) through the
released Python wheel. Across releases it found changes that no in-repo
gate saw:

- q19, q22, q27 and q30 ran for seconds to minutes at SF1 on 0.11.8 and
  0.12.0 (q22 about 70-80 s) because of the order comma-joined MATCH parts
  run in (K98); 0.12.1 brought q22, q27 and q30 to milliseconds.
- q19 verbatim **regressed** from 7.0 s (0.11.8) to 10.8 s (0.12.1) at
  SF1, and q26 from 0.95 ms to 9.75 ms.

The measured size of the workload on 0.12.x (Apple M4 Max):

| | SF0.1 | SF1 |
| --- | --- | --- |
| archive | 19 MB | 241 MB |
| nodes / relationships | 327,588 / 1,477,965 | 3,181,724 / 17,256,038 |
| load | 25 s | 495 s |
| store open | 4 s | 105 s |
| peak RSS | 3.1 GB | 18 GB |
| upstream expected answers | none | all 30 |

Agents working on the engine need a reproducible, in-tree target to find
where a query's time goes ([ADR-0244](./0244-in-process-query-sampling-profiler.md)) and a gate that stops the next
regression ([ADR-0245](./0245-query-latency-baseline-environment-and-gate.md)). This ADR decides the suite itself: what is
imported, how data arrives and is loaded, how queries are run and judged,
and at which levels it runs.

## Decision

1. **New crate `testing/cypherlite-query-latency`** (R4: `testing/*` may
   depend on any workspace package). The first workload is `gbl`, under
   `workloads/gbl/`:
   - `queries/q01.cypher` … `q30.cypher`: upstream's Neo4j query text,
     verbatim, each after a `// qN: <docstring>` line that is not sent.
   - `expected-sf1.json`: upstream's 30 answers and order sensitivity.
   - `expected-sf0.1.json`: the 30 answers at SF0.1, produced **once** by
     a reference Neo4j lane (Neo4j in Docker,
     upstream's schema, the same load), with a `_source` header recording
     the version, image digest and date. The
     engine under test is never its own oracle.
   - the schema (node and relationship files, labels, types, property
     names and their types) as a table in `src/schema.rs`, which the
     loader expands into its `UNWIND` statements;
   - `indexes.cypher` (upstream's index profile:
     `ID` on all eight labels before the data, eight lookup indexes
     after), `text-indexes.cypher` (the optional TEXT profile, selected
     with `--indexes text`).
   - `NOTICE` (upstream's MIT text, what was taken from which file, and
     the CC BY 4.0 attribution for the data).

   Only the verbatim query text is run. The `tuned/`
   rewrites stay in the examples repository: a slow verbatim query is the signal.

2. **Fair use and data.** Nothing in the repo is named an "LDBC
   Benchmark" or reports "LDBC Benchmark Results" (LDBC fair-use policy);
   the suite is "query latency on the graph-benchmark-ldbc workload". The
   data is fetched, never vendored: `query-latency fetch --sf 0.1|1`
   downloads from `datasets.ldbcouncil.org/snb-interactive-v1`, fails
   closed against the two pinned sha256s in `workloads/gbl/data.sha256`,
   and extracts with pure-Rust `ruzstd` and `tar` (`ureq` and `sha2` are
   already workspace dependencies). The cache is
   `target/query-latency/data/` unless `CYPHERLITE_QUERY_LATENCY_DATA`
   names another directory, so a `.cache/ldbc/` layout fetched elsewhere can be
   reused.

3. **Loader: batched `UNWIND` with parameters.** The loader reads each
   `|`-separated file with the `csv` crate and runs
   `UNWIND $rows AS r CREATE …` in batches through
   `cypherlite::Database`, with upstream's schema (one label per node
   file, `id` as INTEGER `ID`, empty cells left out, ISO date strings
   kept as strings, `length`/`classYear`/`workFrom` as INTEGER,
   relationship endpoints matched by `ID`). It fails unless every label
   and type count equals its file's rows and the totals equal the
   published ones. The default target is `:memory:`, loaded fresh each
   run; `--store PATH` builds a durable store once and reuses it (for
   SF1). Load time is recorded as a side metric, not gated.

4. **Runner.** Per query, in-process on one connection:
   - one untimed warmup, then timed rounds until at least 5 rounds and at
     least `--min-time` (default 1 s) have passed;
   - every run goes through `Connection::query_with_params_cancellable`
     with a per-query timeout (default 60 s at SF0.1, 300 s at SF1); a
     timeout is a recorded failure, not a hang;
   - each round records wall time and the engine's own
 `QueryResult::elapsed` ([ADR-0221](./0221-engine-reported-statement-elapsed.md)); the report keeps min, p50, mean,
     p99, max and the round count for both;
   - the last round's rows are checked against the expected answers by
     upstream's rules (list equality where order-sensitive, else multiset
     equality of sorted `(column, value)` tuples, column names must match)
     with no type coercion (`Int(1)` never equals `Bool(true)` or
     `Float(1.0)`).

5. **Plan goldens.** `EXPLAIN` of each query at SF0.1 is pinned in
   `workloads/gbl/plans/sf0.1/qNN.plan`. A changed plan fails the test;
   `QUERY_LATENCY_UPDATE=1` rewrites the goldens for review, as
 `COOKBOOK_UPDATE=1` does for the cookbook ([ADR-0130](./0130-cookbook-tested-recipes.md)). This is the
   noise-free check for planner cliffs of the K98 kind.

6. **Levels.**
   - **Per PR (CI):** SF0.1. Answers, plan goldens, and the latency and
 memory compare of [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md).
   - **SF1:** local only (`query-latency run --sf 1`, on a machine with
     at least 32 GB RAM). Answers are checked against upstream's;
     latency is reported, not gated. No CI workflow runs SF1.

7. **Entry points.** One binary, `query-latency`, with `fetch`, `run`
 and `compare` (and `profile`, [ADR-0244](./0244-in-process-query-sampling-profiler.md)); a test target gated on
   `CYPHERLITE_QUERY_LATENCY=1` (as `CYPHERLITE_GRAPH_EXAMPLES=1` gates
 [ADR-0144](./0144-graph-examples-differential-suite.md)'s) runs fetch, load, answers and plans; `make query-latency`
   wraps the run. The report is
   `target/query-latency/report-<workload>-sf<sf>.json`, stamped with the
 [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) `environment` class. The usage and flag table live in
   `docs/datasets.md` (AGENTS.md §3).

## Alternatives considered

- **Port the examples repository's Python/pytest harness** — rejected: it times
  through the wheel, so PyO3 conversion and Python row handling sit inside
  every measurement and every sampled stack, and CI would need the wheel
  build and a Python environment. The gate tooling here is Rust.
- **Extend `cypherlite-datasets`** — rejected: different provenance (MIT
  queries, CC BY data, its own NOTICE) and dependencies (fetch, zstd,
  tar); a separate crate keeps the attribution scoped to what it covers.
- **Fold it into the graph-examples differential suite ([ADR-0144](./0144-graph-examples-differential-suite.md))** —
  rejected: that suite diffs answers against a live Neo4j oracle in
  Docker; this one measures latency against frozen answers and must run
  without Docker-in-Docker or a server.
- **Vendor the SF0.1 data** (19 MB archive, 97 MB of CSV) — rejected:
  it grows every clone's history for data a pinned, cached download gives
  just as reproducibly.
- **`LOAD CSV`, as the examples harness loads** — rejected: its 8 MiB
  file cap forces chunking every file into parts, and the load path is
  not what this suite measures. `LOAD CSV` throughput belongs to the bulk
  loadtests.
- **SF1 in PR CI** — rejected: 18 GB RSS, an 8-minute load and a
  105-second open exceed the standard runners and the PR time budget.
- **SF1 as an opt-in `workflow_dispatch` job on a large GitHub runner**
  — rejected (maintainer decision): it needs a paid runner class for a
  report nobody gates on; SF1 stays a local run.
- **Record SF0.1 answers from CypherLite itself** (after SF1 matches
  upstream) — rejected: the engine would be its own oracle at the scale
  the PR gate runs.
- **Time the `tuned/` rewrites too** — rejected: they hide exactly the
  planner behaviour the suite exists to catch.

## Consequences

- A new crate, `NOTICE`, workload files, binary, make targets, CI job and
  `docs/datasets.md` section.
- New dev-only dependencies `ruzstd` and `tar` in `testing/` only; no
  engine crate changes (R6 not triggered).
- `expected-sf0.1.json` comes from one reference Neo4j-lane run
  (`./run.sh neo4j answers --sf 0.1`,
  2026-10-14; every query run twice with identical answers, and every
  `LIMIT` answer checked free of ties); regenerating it is a deliberate,
  recorded step. CypherLite 0.12.0's SF0.1 answers matched all 30.
- The query literals were chosen for SF1, so at SF0.1 nine queries
  (q1-q7, q9, q10) return no rows, three counts are 0 (q13, q16, q20) and
  seven existence checks are `false`: 19 of 30 SF0.1 answers say "nothing
  found". They still run the full search, so their latency is meaningful,
  but as answer checks they are weak: a bug that drops rows passes them.
  The plan goldens and the SF1 answers carry more of the correctness
  weight; SF0.1-specific literals would change upstream's workload and
  are left to a later ADR.
- A CI job per PR: dataset restored from `actions/cache` (keyed on
  `data.sha256`), about 25 s of load plus about a minute of queries at
  SF0.1, on top of the build.
- Cross-engine comparisons (Neo4j, Kuzu, upstream's table) and the
  `tuned/` rewrites stay outside this repository; this suite reports
  CypherLite only.
- [ADR-0244](./0244-in-process-query-sampling-profiler.md) builds the sampling profiler on this runner; [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md) decides
  the latency and memory gate and its environment.

## Later references

- [ADR-0246](./0246-defer-query-latency-plan-goldens.md): item 5 (plan goldens) is withdrawn. `EXPLAIN` renders the
  logical plan clause by clause, without the part order, start node or
  index choice that K98 cliffs come from, so a golden of it cannot catch
 them; the [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md) latency compare carries that job. The plan goldens
  named under item 6 and item 7, and in Consequences, do not exist.
- [ADR-0247](./0247-query-latency-sf0-1-literals.md): SF0.1 runs 14 queries with recorded literal substitutions
  (`workloads/gbl/literals-sf0.1.json`), so the 19 "nothing found" SF0.1
  answers in Consequences are now 5, upstream's negative checks (q7, q24,
  q25, q26, q29), which answer "nothing" at SF1 too. SF1 stays verbatim.
