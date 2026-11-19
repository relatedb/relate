---
type: adr
area: infra
date: 2026-09-18
deciders: CypherLite / MemoryDB merge
name: 0060-identity-loadtest-baseline-refreeze
status: accepted
tags:
  - loadtest
title: Re-freeze the identity loadtest baseline for gate-host daytime conditions
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0048-identity-loadtest-memory-budget]]"
  - "[[adr/0061-ci-host-loadtest-baselines]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
SUPERSEDES:
  - "[[adr/0048-identity-loadtest-memory-budget]]"
---

# 0060. Re-freeze the identity loadtest baseline for gate-host daytime conditions

## Context

[ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) froze loadtest summaries under `baselines/` as a merge gate (ops/s ≥
90% of baseline, p99 ≤ 115% + 250,000 ns jitter allowance, logical bytes ≤
110%, errors == 0). [ADR-0048](./0048-identity-loadtest-memory-budget.md) re-froze the identity baseline at corrected knobs
(8 workers, `:memory:`, 40 MiB) after documenting that host conditions swing
identity throughput (unchanged 50-worker filesystem runs varied 1,496–1,779
ops/s).

On 2026-09-18 the identity throughput leg failed four consecutive fresh-HEAD
runs (1,486.10 / 1,500.49 / 1,528.35 / 1,557.75 ops/s vs the 1,579.37 floor
from the frozen 1,754.85), two of them on an otherwise-quiet machine (load
2–4 on 16 cores). The frozen baseline had been produced on this same host the
previous day under overnight-idle conditions, and the gate had passed that
same morning (1,743.1 ops/s, 0.993×) on an idle machine. The suspect window
between green and red contained five merge nodes (35–39), of which only node
37 (property-index key normalization) touches a hot path.

A position-controlled A/B experiment isolated code from conditions: five
interleaved pairs of `make identity-loadtest` at the pre-reopen state
`60fa578` (A) vs the gated HEAD `b6c984e` (B), same daytime machine
conditions, alternating order, with the final two pairs order-reversed:

- **Position effect** (first vs second in pair): medians 1,537 vs 1,519 —
  ratio 0.988, noise.
- **Code effect** (A vs B): medians 1,537 vs 1,521 ops/s — B/A 0.990, noise;
  bands overlap completely.
- Both commits sit in a ~1,370–1,600 ops/s daytime band; zero errors in
  every run; per-op latency parity vs the frozen baseline (mean 1.028, p50
  1.023, p99 1.058); logical-bytes ratio ~1.03, well under the 110% band.

The gate failure is therefore host-condition variance (daytime-active vs
overnight-idle), not a code regression: this host cannot reproduce the frozen
baseline during normal working hours regardless of the code under test.

## Decision

Re-freeze `baselines/identity-loadtest.json` from a zero-error HEAD run
representative of daytime gate-host conditions (1,521.03 ops/s; A/B run `b1`,
2026-09-18, HEAD `b6c984e`, [ADR-0048](./0048-identity-loadtest-memory-budget.md) knobs: 8 workers, `:memory:`, 40 MiB —
summarized with `scripts/summarize-loadtest.py`, ops arrays omitted as
before). All [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands (ops/s ≥ 90%, p99 ≤ 115% + 250,000 ns, logical
bytes ≤ 110%, errors == 0) and all [ADR-0048](./0048-identity-loadtest-memory-budget.md) knobs are unchanged. The fraud
and checkpoint baselines are untouched: fraud's failures were
load-correlated scattered p99 misses that passed on a quiet re-run, with no
systematic component.

## Alternatives considered

- **Treat as a product regression and rework node 37** — rejected: the
  position-controlled A/B shows no code effect (B/A 0.990, overlapping
  bands), per-op latency is at parity, and the throughput deficit sits in
  inter-op wall time, not query execution.
- **Keep the stale baseline; only run the gate on an idle machine** —
  rejected: a gate that can only pass overnight is a broken gate; developers
  run `make gate` in the daytime.
- **Widen the throughput floor (e.g. 0.80) against the stale baseline** —
  rejected: a looser band around a number the host cannot reproduce carries
  less signal than an honestly re-frozen baseline at the existing 0.90 floor.
- **Re-freeze the fraud baseline too** — rejected: fraud passes in quiet
  conditions; no evidence its baseline is stale.
- **Gate identity on per-op latency instead of wall-clock throughput** —
  rejected for now as a gate-contract change beyond this fix; recorded as the
  follow-up instrument if throughput flakes recur.

## Consequences

- The identity gate now guards drift against a baseline the gate host
  reproduces in daytime (observed band ~1,370–1,600 ops/s; new floor
  1,368.9). The band's observed bottom (1,373.6) sits ~0.3% above the new
  floor, so rare daytime flakes remain possible; the documented recourse is a
  quiet re-run, as already practiced. If flakes recur, the follow-up is a new
  ADR amending the throughput band or gating per-op latency instead — this
  ADR's A/B protocol (interleaved, position-controlled pairs) is the
  adjudication template.
- Baselines remain host- and condition-specific: moving the gate to a
  different host class repeats this measure-then-re-freeze pattern under a
  new ADR.
- [ADR-0048](./0048-identity-loadtest-memory-budget.md)'s regenerated identity baseline is superseded (see its *Later
  references*); its knobs, memory budget, and jitter allowance stand.
- `make gate` on the merge branch re-verified green against this baseline
  (gbuild node `40-reopen-verify`).

## Later references

- [ADR-0061](./0061-ci-host-loadtest-baselines.md): the "moving the gate to a different host class" case this ADR's
  Consequences predicted arrived immediately — CI `ubuntu-latest` runners
  cannot reproduce the local-host baselines (PR #13: fraud at 58% of the
  throughput floor, zero errors). CI now compares against its own
  `baselines/ci/` set seeded from runner artifacts; the local gate and this
  re-frozen baseline are unchanged.
- [ADR-0070](./0070-docker-baseline-loadtest-environment.md): host-class splits are replaced by a Docker baseline environment
  (2 vCPU / 2 GB). Identity (and fraud/checkpoint) are re-frozen from that
  environment; `baselines/ci/` is withdrawn.
