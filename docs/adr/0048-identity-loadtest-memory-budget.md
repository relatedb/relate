---
type: adr
area: infra
date: 2026-09-16
deciders: CypherLite / MemoryDB merge
name: 0048-identity-loadtest-memory-budget
status: accepted
tags:
  - loadtest
title: Separate identity loadtest memory budget
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0060-identity-loadtest-baseline-refreeze]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
---

# 0048. Separate identity loadtest memory budget

## Context

[ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) requires frozen loadtest summaries to have zero errors. The original
fraud and identity baselines shared a 20 MiB logical memory limit. That limit is
valid for the fraud workload, which finishes near 17 MiB, but not for the
identity workload.

At the frozen identity knobs (`scale=0.10`, 20,000 measured operations, 50
workers, and `write_fraction=0.44`), the source baseline reached 20,970,786
logical bytes and reported 2,484 capacity errors. The merged runtime reached
20,971,398 bytes and reported the same number of capacity errors. The 612-byte
difference is not a material accounting regression; the baseline was frozen at
the cap and could not satisfy its own zero-error contract.

A 32 MiB probe completed with zero errors but used 31.47 MiB, leaving too little
headroom for a repeatable performance gate. The identity workload is a
throughput and latency benchmark, not a capacity-limit test.

The filesystem-backed 50-worker identity runs were also not repeatable:
unchanged merged-runtime runs varied from 1,496 to 1,779 ops/s, while p99
stalls moved among unrelated sub-millisecond read use cases. Parser and planner
work averaged only 0.037 ms, ruling that out as the throughput cause. Fraud
already exercises the durable WAL with 50 workers.

With eight workers and an in-memory WAL, fresh source and merged identity runs
produced 2,553 and 2,589 ops/s with zero errors. The remaining ratio-only p99
failures were 16–36 microseconds on baselines below 0.2 ms: ordinary clock and
scheduler jitter, not meaningful query regressions.

## Decision

Keep the fraud loadtest at `MEMORY_LIMIT=20mib`. Give the identity loadtest a
separate `MEMORY_LIMIT_IDENTITY=40mib` default in the merge harness. Run the
identity gate with `WORKERS_IDENTITY=8` and `URI_IDENTITY=:memory:`; fraud
retains the 50-worker filesystem/WAL coverage.

Regenerate only the summarized identity baseline from a fresh, zero-error
source run at those corrected knobs. Keep the 115% p99 ratio and add a 250,000
ns absolute jitter allowance: p99 fails above the greater of 115% of baseline
or baseline plus 250,000 ns. Current errors must still be zero, throughput must
be at least 90% of baseline, and logical bytes must be at most 110%.

## Alternatives considered

- Treat matching capacity failures as acceptable — rejected because [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)
  explicitly requires zero errors and capacity failures omit requested writes.
- Increase the shared `MEMORY_LIMIT` — rejected because fraud fits its existing
  budget and changing both workloads would erase a useful fraud memory bound.
- Retain 50 filesystem-backed identity workers — rejected because repeated
  unchanged runs crossed the throughput threshold and moved p99 stalls among
  unrelated read queries; fraud already covers this WAL/concurrency profile.
- Use 32 MiB for identity — rejected because the successful probe consumed
  98.3% of the cap and would remain vulnerable to ordinary concurrent outcome
  variation.
- Apply only the 115% ratio to sub-millisecond p99 values — rejected because
  tens of microseconds of ordinary jitter repeatedly failed otherwise matching
  source/current runs.
- Change logical memory accounting — rejected because source and current usage
  at saturation differed by only 612 bytes and the accounting invariants remain
 covered by [ADR-0042](./0042-graph-memory-limit.md) tests.

## Consequences

- `make identity-loadtest` and source identity baseline generation use a 40 MiB
  logical cap, eight workers, and an in-memory URI unless their identity-specific
  variables are overridden.
- `make fraud-loadtest` continues to use `MEMORY_LIMIT`, defaulting to 20 MiB.
- The replacement identity baseline contains no operation array and represents
 a complete, zero-error source workload suitable for [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) comparison.

## Later references

- [ADR-0060](./0060-identity-loadtest-baseline-refreeze.md): the identity baseline regenerated under this ADR was re-frozen on
  2026-09-18 after the gate host demonstrably could not reproduce it in
  daytime conditions (a position-controlled A/B showed no code regression).
  The 8-worker / `:memory:` / 40 MiB knobs and the 250,000 ns p99 jitter
  allowance are unchanged.
- [ADR-0070](./0070-docker-baseline-loadtest-environment.md): these knobs now run inside the Docker baseline environment
  (two CPUs of CFS bandwidth, 2 GB). `WORKERS_IDENTITY` drops from 8 to 4
  and the p99 jitter allowance rises to 5 ms there, because eight writers
  saturate two CPUs and queueing alone then moves p99 by 2-4x. The
  40 MiB logical cap and the `:memory:` URI decided here are unchanged.
