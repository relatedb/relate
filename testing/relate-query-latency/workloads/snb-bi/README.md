# snb-bi — LDBC SNB BI analytics corpus (LABS-646 follow-up, ADR-0479)

The 16 analytics queries of the LDBC Social Network Benchmark's Business
Intelligence workload, mechanically translated to the graph-benchmark-ldbc store's
vocabulary and running over the same pinned graph-benchmark-ldbc LDBC data as the `graph-benchmark-ldbc`
lane (`workloads/graph-benchmark-ldbc/`, ADR-0243 — no new data).

## Provenance and license

- Source: [ldbc/ldbc_snb_bi](https://github.com/ldbc/ldbc_snb_bi) @
  `47dd38b40844ecdb0e42e5a610c369535304786d` ("Add Umbra binary",
  2026-04-06), **Apache-2.0** (repo-root `LICENSE.txt`, verified in a
  local clone).
- What was taken: ONLY the query files (`neo4j/queries/bi-1..bi-20`) and
  the `:params` default values in their headers. What was never taken:
  their `benchmark.py`/`queries.py`/paramgen machinery and the published
  parameter archives (their entity ids exist in no graph-benchmark-ldbc store — different
  datagen run; params regenerate per store, see below). These files are
  OUR Cypher translations to the graph-benchmark-ldbc vocabulary; each file's header names
  the upstream repo, pin and upstream file.
- Skipped, permanently out of dialect: bi-10 (APOC), bi-15/19/20 (GDS
  procedures) — never translated, not a coverage gap.

## Translation

The vocabulary mapping (Message → `Comment|Post`, uppercase rel types →
graph-benchmark-ldbc relationship types, City/Country → `Place`, string `creationDate`
wrapped in `datetime()`, `id` → `ID`) lives in ONE place:
`params-sf0.1.json` `_source.mapping`. Two corrections the landing run
forced (each recorded in its file's header):

- **bi-16** — graph-benchmark-ldbc `creationDate` is a datetime-shaped ISO string, so
  `date()` of it needs the mapping's `datetime()` wrap first:
  `date(datetime(message.creationDate))` (the raw `date()` call fails on
  the `+0000`-offset string; the param is a datetime value, `date()` of
  which converts directly — upstream's param is a date).
- **bi-17** — upstream spells the two `HAS_MEMBER` patterns with both
  arrowheads (`<-[:HAS_MEMBER]->`); the intended edge is forum → person
  (upstream bi-4 and bi-17's own `NOT` clause spell it that way, and
  graph-benchmark-ldbc's edge is `forum-[:hasMember]->person`), so the translation is
  directional — the arrow-flipped form matched nothing over the graph-benchmark-ldbc
  store.

## Parameters (no seed policy)

Upstream's `:params` defaults were chosen on SNB BI SF1 data; over the
graph-benchmark-ldbc store they are starting points. There is no seed resolution (nothing
anchors on a node id): every value in `params-sf0.1.json` is ONE Cypher
expression string, evaluated once per parameter at lane build
(`RETURN <expr>`) and bound as the statement's parameter — the engine's
native parameter binding (ADR-0479), never textual substitution (a bare
word is not an expression, so a string parameter carries its quotes). A
param whose query answers empty at SF0.1 is substituted with a
same-kind value that has rows (ADR-0247 precedent) and recorded here:

- **bi-16** — upstream `Meryl_Streep` 2012-09-16 / `Hank_Williams`
  2012-05-08 answers empty at SF0.1 (no person qualifies on both tag-day
  pairs); substituted `2_Become_1` 2010-08-18 / `Alexander_Pope`
  2010-10-05 (2 rows, the fastest qualifying pair found — the query's
  cost is structural, ~1.9 s across tag pairs of every volume) — same
  kinds (tag, day), found by enumerating the qualifying
  (tag, day, person) triples the inner `CALL` admits.
- Every other entry runs with upstream's own defaults unchanged
  (bi-13 France and bi-11 India answer 1 row each — non-empty, kept as
  upstream chose them).

Regeneration rule: params are store-specific. Re-freezing the data set
or adding a scale factor regenerates this file — check each query's row
count, substitute empty-answer params same-kind, record the
substitutions in this README. Recorded at SF0.1 only; `--sf 1` fails
loudly until an SF1 params file exists.

## Dialect check

The seven forms the 2026-10-31 spike had NOT exercised, each verified by
execution (parse + run over hand-built graphs and the graph-benchmark-ldbc SF0.1 store,
2026-11-02) before any file was marked `ok` — all supported, no engine
gap found:

| Form | Where | Verdict |
| --- | --- | --- |
| Label-expression alternation `n:Comment\|Post` in a node pattern | bi-01..bi-09, 11, 13, 14, 16-18 | supported — parse + execute |
| Label predicate in a RETURN projection (`message:Comment AS isComment`) | bi-01 | supported — answers `true`/`false` per label |
| Pattern comprehension with inner WHERE (`[(tag)<-[:...]-(m) WHERE ... \| m]`) | bi-08 | supported — sizes match the manual count |
| `duration({days: N})` map form + `datetime(...) + duration` arithmetic | bi-02, bi-17 | supported — `datetime("2012-06-01") + duration({days: 100})` = 2012-09-09 |
| `CALL { ... UNION ALL ... }` (UNION ALL inside a subquery) | bi-04 | supported — both branches contribute |
| `collect({...})[0]` indexing + `top.k` map access on the result | bi-14 | supported |
| `datetime(<string property>).year` / `.month` component access | bi-01, bi-13 | supported |

Also verified on the same run: temporal/list/string/integer parameter
binding (the lane's evaluation plan), and `date(<datetime value>)`
conversion (bi-16's param side). Perf note for the record, not a
blocker: the `datetime(...)` wraps parse a string per row per
comparison — the same per-row temporal-parse cost the sol experiment
already flagged (q8's `date()` family); revisit if a bi query ever
enters a timed lane.

## Execution policy

Run-and-report (ADR-0289 memory-shape precedent): nothing here enters a
frozen gate compare set and no latency number gates a PR. SNB BI has NO
oracle answers for graph-benchmark-ldbc stores — upstream's expected outputs are for
their own datagen run, whose entity ids exist in none of our data — so
what gates is the ratchet: an `ok` entry must answer within the timeout
with a sane shape (row count stable across runs), and the ok-set only
shrinks with a tracked reason, like the TCK allowlist. `kept` entries
parse and EXPLAIN only, never execute. Timeout: 10 s per entry at SF0.1,
watchdog-cancelled.

The `kept` class is one family: the unbounded
`replyOfComment|replyOfPost*0..` transitive walk per anchor (bi-03,
bi-04, bi-09, bi-12, bi-17) — each cancelled at 60 s in the landing
probe, far past the 10 s bound, and bi-17's no-watchdog probe was killed
on memory — the walk-budget / memory-blow-up acceptance cases
(LABS-660), future-engine tickets already tracked there. They become
`ok` only by running and passing, never by reclassification.

Initial ok-set, recorded 2026-11-02 against the engine at this PR
(`query-latency corpus snb-bi --sf 0.1`, graph-benchmark-ldbc SF0.1, release,
reused store): 11 `ok` of 11 answered, 5 `kept` planned, peak RSS
1.90 GB, slowest `ok` entry bi-18 at 2.0 s (bi-16 1.7 s, bi-01 1.0 s;
the rest under 0.35 s), rows 1-100 per entry — bi-01 12, bi-02 100,
bi-05 38, bi-06 100, bi-07 100, bi-08 22, bi-11 1, bi-13 1, bi-14 5,
bi-16 2, bi-18 20.

## How to run

```bash
cargo run --release -p relate-query-latency -- corpus snb-bi --sf 0.1
# the gated test (loads the graph-benchmark-ldbc SF0.1 graph):
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus snb_bi
```
