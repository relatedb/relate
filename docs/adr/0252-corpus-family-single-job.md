---
type: adr
area: infra
name: 0252-corpus-family-single-job
status: accepted
tags:
  - ci
title: The corpus family builds once in a single merged CI job
REFERENCES:
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0130-cookbook-tested-recipes]]"
  - "[[adr/0212-tck-release-in-ci]]"
  - "[[adr/0251-test-architecture]]"
SUPERSEDES:
  - "[[adr/0212-tck-release-in-ci]]"
---

# ADR-0252: The corpus family builds once in a single merged CI job

## Context

[ADR-0212](./0212-tck-release-in-ci.md) introduced a `corpus-build` job that compiles the corpus-profile
test binaries once (`cargo test --profile corpus --no-run`) and populates a
shared rust-cache key (`corpus`) so the three corpus consumers — TCK,
CIP-eval, cookbook-native — "restore warm instead of each paying the full
release compile". [ADR-0251](./0251-test-architecture.md) §5 keeps the goal: one build per SHA, shared
everywhere.

The audited reality: the ~327 s corpus-profile compile is still paid four
times per CI run (~1370 s combined), while the tests themselves run in
seconds (the full 3893-scenario TCK runs in ~2.5 s). The cross-job cache
handoff does not work for within-run sharing: each consumer job checks out
the sources fresh, every source mtime is newer than the restored target
directory's fingerprints, and cargo recompiles. The cache only pays off
across runs with an unchanged `Cargo.lock`, never across jobs within one
run. Implementation notes and the before/after table:
[`docs/testing/corpus-build.md`](../testing/corpus-build.md).

## Decision

Merge `corpus-build`, `cypherlite-tck`, `cip-eval`, and `cookbook-native`
into one `corpus` job. Step 1 builds the three crates' corpus-profile test
binaries; steps 2–4 run the TCK (3892 pass + 1 documented skip of 3893,
command unchanged), CIP-eval (baseline-regression gate, command unchanged),
and `make cookbook-native` against the same target directory on the same
runner. The gates' strength is unchanged — same commands, same corpus,
`docs/tck/compliance.md` untouched.

The merged job keeps `shared-key: corpus` so repeat runs over an unchanged
`Cargo.lock` restore warm; the key now has exactly one writer per run. It
needs only `changes`, plus a new `cookbook` output on `changes` so a
`docs/cookbook/*`-only edit still runs the native recipe gate ([ADR-0130](./0130-cookbook-tested-recipes.md)) —
previously it silently skipped, because its `corpus-build` need was
skipped on docs-only changes. The TCK and CIP steps stay gated on
`code == true`; the recipe step runs whenever the job runs.

## Alternatives considered

- **Artifact handoff from `corpus-build` (upload/download the target
  dir)** — rejected: same fingerprint risk as the rust-cache handoff it
  replaces. Downloaded artifacts carry archive-preserved or extract-time
  mtimes and any profile/feature/target-dir divergence between producer
  and consumer silently converts the download into a full rebuild — the
  exact failure being fixed. A job merge has no fingerprint to get wrong.
- **Keep four jobs, fix freshness (touch sources to older mtimes after
  checkout)** — rejected: mtime surgery on the checkout to fool cargo's
  freshness check is fragile and invisible to the next reader; it also
  keeps four runner boot + toolchain + checkout + restore setups (~2–3 min
  each) that a merged job pays once.
- **Keep the status quo** — rejected: ~1000 s of redundant compile per run
  is the largest single waste in CI, and the recipe-only-edit gate skip is
  a latent coverage hole.

## Consequences

- The corpus compile happens once per run (or zero times on a warm cache
  hit); expected corpus compute drops from ~1370 s to ~400–450 s, and
  corpus wall time from ~660–720 s to ~400–450 s.
- Failure granularity moves from job to step; the failing step name still
  identifies the gate.
- Required-check names change to the single `Corpus (TCK, CIP eval,
  cookbook native)`; branch protection is updated on landing.
- [ADR-0212](./0212-tck-release-in-ci.md)'s "one pre-compile job" mechanism is superseded; its profile
  split (corpora in `corpus`, unit suites in debug) is unchanged. The
 [ADR-0070](./0070-docker-baseline-loadtest-environment.md) loadtest baseline environment and the `loadtest-baselines` job
  are untouched.
