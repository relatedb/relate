# Load perf: the REL8-211 ground truth on a quiet host

The measurement behind [REL8-211](https://linear.app/neo4j/issue/REL8-211/bulk-load-is-bound-by-per-item-createinstall-not-serialization-8s-for) and
[REL8-1356](https://linear.app/neo4j/issue/REL8-1356/perf-bulk-load-api-columnartyped-arrays-internal-id-relationships-load):
how long one `UNWIND ... CREATE` statement takes to build 1M nodes and
1M relationships, and how much CPU and memory it spends doing it. The
workload is the ticket's ground truth verbatim — every row pays the
per-item create/install path (interning, overlay, adjacency, label index,
stats), which is the mechanism both tickets are about.

```
UNWIND range(1,1000000) as id
create (p:Person {name:"Tomas "+id, id:id, age:id%100})-[:KNOWS]->(p);
```

## Running it

- Locally: `make load-perf` (builds the release CLI, then
  `scripts/load-perf.sh target/release/relate` — the defaults, including
  the out-dir, are the script's own). `LOAD_PERF_RANGE` shrinks the
  workload, `LOAD_PERF_ROUNDS` sets the best-of-N; both ride the
  environment, which `make` passes through.
- In CI: the `load-perf` workflow ([ci-lanes.md](ci-lanes.md)) runs the
  same script on the loadtest runner class for every ready PR, on every
  push to `main`, and on demand (`workflow_dispatch` on any ref that
  carries the workflow file — `main` and branches based on it; a PR head
  predating the workflow cannot be dispatched and gets its numbers at
  ready-for-review, on the PR's merge ref).

Each round runs on a fresh store and records **execute** (the engine's
own `--timings` number — the same metric the tickets' local measurements
quote), **wall / user / sys** and **peak RSS** from time(1) (GNU `-v` on
Linux, BSD `-l` on a dev Mac), and a **load stamp before and after every
run**. The report lands in `target/load-perf/REPORT.md` (plus the
machine-readable `report.txt`); the CI lane mirrors it into the job
summary and uploads it as an artifact.

## Reading the numbers

- **Reporting, never gating.** No envelope, no bands; a number cannot
  redden a build (the sqlite-gate rule, the ADR-0482 §4 precedent). Only
  a run the script cannot read back — a failed query, a missing timings
  line — fails.
- **Run-to-run on one host class.** Absolute times are host-class
  properties. CI rows compare with CI rows; a dev-box row (the load
  stamps say whether it was quiet) compares with nothing but its
  siblings. This is the whole reason the lane exists: the 2026-11-23
  local re-measure never found a load window under 4 in 4.75 hours on a
  box shared with sibling builds, and a user-CPU reading taken above the
  bar is provisional at best — contention inflates user CPU as much as
  wall.
- **The medians line** is best-of-N; with three rounds it is the middle
  row of each metric.

The related-but-distinct timing suites: the query-latency ratchet
([query-latency.md](query-latency.md), ADR-0245) gates read-side p50s
against a frozen envelope; the loadtest baselines
(ADR-0070/ADR-0071) gate dataset loadtest throughput. This lane reports
the raw create-path number the bulk-load tickets argue about.
