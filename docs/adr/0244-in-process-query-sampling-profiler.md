---
type: adr
area: infra
date: 2026-10-13
name: 0244-in-process-query-sampling-profiler
status: accepted
tags:
  - latency-suite
title: In-process sampling profiler for the query-latency suite
REFERENCES:
  - "[[adr/0212-tck-release-in-ci]]"
  - "[[adr/0227-ontology-document-format]]"
  - "[[adr/0243-query-latency-suite]]"
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
---

# ADR-0244: In-process sampling profiler for the query-latency suite

> Renamed from [ADR-0227](./0227-ontology-document-format.md): the number collided with
> docs/adr/0227-* on main (both were taken by parallel PRs). The
> decision is unchanged; references were updated in the same change.

## Context

[ADR-0243](./0243-query-latency-suite.md) gives the repo a per-query latency number. To act on a slow or
regressed query, an engineer or agent then needs to know **where** the
time goes. Today:

- `PROFILE` reports only the statement's total time and row count
  (`cypherlite/cypherlite/src/explain.rs`); there is no per-operator
  timing. Adding it is an engine change and out of scope here.
- `scripts/perf/` profiles the fraud and identity loadtests with Linux
  `perf` only: it needs `perf_event_paranoid` access, does not run on
  macOS, where most development happens, and does not cover single
  queries.
- The `release` and `corpus` ([ADR-0212](./0212-tck-release-in-ci.md)) profiles carry no debug info, so
  any sampler shows unsymbolized or inlined-away frames.

Agents read text. A profile they can use is a ranked list of frames and
crates in a file, produced by one command, with no sudo and no
platform-specific tooling.

## Decision

1. **A `profile` subcommand of `query-latency`** ([ADR-0243](./0243-query-latency-suite.md)):
   `query-latency profile --sf 0.1 --qid 19 [--seconds 10]
   [--frequency 997] [--indexes upstream|text] [--sampler pprof|none]`.
   It loads the workload, runs the query once untimed, then loops it for
   `--seconds` (at least one iteration) under the sampler. `--sampler
   none` runs the same loop with no in-process sampler, so an external
   one (`samply`, `perf`) can record the process instead.

2. **The sampler is `pprof-rs`** (`pprof` 0.15, feature `flamegraph`),
   in-process, timer-signal based, sampling every thread, including the
   connection's query worker thread. It is a dependency of
   `testing/cypherlite-query-latency` only, behind a crate feature
   `sampling` (default on, compiled on `cfg(unix)` only; on other
   targets the subcommand reports that sampling is unavailable).

3. **Outputs**, under
   `target/query-latency/profile/<workload>-sf<sf>-q<NN>/`:
   - `top.txt`: the file agents read. The 40 heaviest frames by self
     time and by inclusive time, then inclusive share by workspace crate
     (`cypherlite-planner`, `cypherlite-executor`, …), with `std`/`core`/
     `alloc` and the allocator reported as their own groups. The grouping
     follows `scripts/perf/03-bottleneck-summary.sh`.
   - `folded.txt`: collapsed stacks, for diffing two runs or re-rendering.
   - `flame.svg`: a flame graph for humans.
   - `summary.json`: query, iterations, mean engine elapsed, sample
     count, frequency, git commit and dirty flag.

4. **A `profiling` cargo profile** in the workspace `Cargo.toml`:
   inherits `release`, `debug = "line-tables-only"`, `strip = "none"`.
   `make query-latency-profile QID=19 [SF=0.1] [SECONDS=10]` builds with
 it and runs the subcommand on the host (not in the [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md) Docker
   environment: a profile is a diagnosis, not a frozen measurement).

5. **No engine instrumentation.** Nothing in `cypherlite/*` changes; the
   sampler observes the release-optimized engine as shipped.

6. **The agent playbook** is `docs/testing/query-latency.md`: run the
   suite, pick the query, profile, read `top.txt`, form one hypothesis,
   change one thing, re-run `run --qids N` and `profile` and compare.
   AGENTS.md gets a short "How to investigate a slow query" pointer to
   it.

## Alternatives considered

- **Per-operator timing in `PROFILE`** — out of scope (maintainer
  decision): it is an engine and public-surface change needing its own
  ADR. It would complement sampling (operator-level attribution with
  rows) and does not replace it (it cannot see parsing, planning,
  allocation or hash-table cost inside an operator).
- **`samply` as the default** — rejected as the default, kept as the
  documented `--sampler none` alternative: a separate install, and its
  output is Firefox Profiler JSON, which needs post-processing before an
  agent can rank frames.
- **Linux `perf` only** (extend `scripts/perf/`) — rejected: no macOS,
  and container or CI use needs `perf_event_paranoid` changes.
- **`tracing` spans in the engine** — rejected: it instruments engine
  crates, costs time on the measured path, and shows only what someone
  thought to annotate.
- **`criterion` benches** — rejected: good statistics, but no sampling,
  and each bench would pay the 25 s SF0.1 load per process.
- **Debug info in the `release` profile** — rejected: it bloats every
  release binary and wheel to serve a diagnostic workflow.

## Consequences

- A new dependency tree (`pprof` and its `inferno`, `backtrace`,
  `findshlibs`, `nix`, `symbolic-demangle`, …) in one `testing/` crate;
  engine crates and released artifacts are unaffected.
- A `profiling` build adds a third optimized target directory next to
  `release` and `corpus` on machines that use it.
- Signal-based sampling has small overhead (about 1-3% at ~1 kHz) and
  attributes time to frames as optimized code inlines them; `top.txt`
  is a pointer to where to look, not a per-line cost model.
- Windows developers profile with an external tool or WSL.
