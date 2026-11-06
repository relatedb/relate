# One corpus-profile build per CI run

Implementation notes for ADR-0252, the same style of record as
[`build-caches.md`](build-caches.md) is for ADR-0251 §5,
[`ci-lanes.md`](ci-lanes.md) is for §2–§4, and
[`cadences.md`](cadences.md) is for §6.

> **Historical record (2026-11-05).** The `corpus` job's `relate-tck` and
> `relate-cip-eval` steps retired with the in-repo language harness
> (ADR-0520); the corpora and their reporting live in the spec harness
> (ADR-0482). The `cookbook-native` step and the `cookbook` change-detection
> output retired when the cookbook moved to the relate-web repository
> (ADR-0528). The one-compile merge described here still applies to the job's
> remaining gate (query-latency).

## Problem

The corpus-profile compile (~24 s per crate-family locally, ~327 s in CI)
was paid four times per CI run: the `corpus-build` job compiled it, and then
the `relate-tck`, `cip-eval`, and `cookbook-native` jobs — despite
restoring the shared `corpus` rust-cache key that `corpus-build` had just
written — each rebuilt it anyway. Combined: ~1370 s of compute per run for
work whose tests execute in seconds (the full 3892-scenario TCK runs in
~2.5 s).

Why the restore did not help: every consumer job runs `actions/checkout`
fresh, so every source file's mtime is newer than the restored target
directory's fingerprints. Cargo treats the restored artifacts as stale and
recompiles. The ADR-0212 pre-compile job therefore populated a cache that
only ever paid off across *runs* (same `Cargo.lock` hash, warm start), never
across *jobs within a run*.

## Decision (ADR-0252)

Merge the four corpus-family jobs into one `corpus` job: one compile, then
the three gates run as steps against the same target directory on the same
runner. No artifact upload, no cross-job cache restore, no fingerprint to
miss.

> **Later addition (ADR-0251 §6, [cadences.md](cadences.md)):** the `corpus`
> job gained a fourth gate step — the LDBC SF0.1 answer gate
> (`relate-query-latency --test graph_benchmark_ldbc`) — as the continuous LDBC slice. It
> rides the same single corpus-profile compile; the full latency run is
> nightly in `background-cadences.yml`.

```text
corpus (needs: changes; runs when code == true or cookbook == true)
  1. Build corpus test binaries   cargo test --profile corpus --no-run
                                   -p relate-tck -p relate-cip-eval
                                   -p relate-cookbook
                                   -p relate-query-latency
  2. openCypher TCK               cargo test --profile corpus -p relate-tck
     (code changes only; 3892 pass + 0 documented skips of 3892 — unchanged)
  3. Run cip-eval                 cargo test --profile corpus -p relate-cip-eval ...
     (code changes only; baseline-regression gate unchanged)
  4. Recipes reproduce ...        make cookbook-native
     (also on docs/cookbook-only changes)
```

The gates themselves are byte-for-byte the same commands as before; only
their job boundaries moved. `docs/tck/compliance.md` is untouched.

## Alternatives weighed

An artifact handoff (option (a) in ADR-0252: `corpus-build` uploads the
target dir, consumers download it) was rejected: it has the same
fingerprint risk as the rust-cache handoff it replaces — downloaded
artifacts carry either stale mtimes (same invalidation) or archive-time
mtimes, and any profile/feature/target-dir divergence between producer and
consumer silently turns the download into a rebuild, which is exactly the
failure being fixed. A job merge has no fingerprint to get wrong.

## What the merge costs

- **Failure granularity**: one failing step fails the `corpus` job; the
  step name still identifies which gate failed. The three gates are
  seconds each once built, so re-running the job to see all three is cheap.
- **Parallelism**: the three corpus gates no longer run on three runners
  concurrently. Their combined run time is ~1–2 min, so wall-clock impact
  is negligible against the ~327 s compile that dominated each lane.
- **Check names**: the required-checks list moves from `Corpus build`,
  `TCK (openCypher)`, `CIP evaluation suite`, `Cookbook recipes (native)`
  to the single `Corpus (TCK, CIP eval, cookbook native, LDBC SF0.1
  answers)`.

## Cache keying stays coherent (ADR-0212)

The merged job keeps `shared-key: corpus` on `Swatinem/rust-cache@v2`, so a
repeat run over an unchanged `Cargo.lock` restores warm instead of
compiling from scratch. The key's semantics actually improve: it now has
exactly one writer per run (no last-saver-wins churn between four jobs
with slightly different artifact sets), and the write-once-per-key ordering
constraint that forced `needs: corpus-build` is gone — `corpus` only needs
`changes`.

> **Later addition (ADR-0331):**
> the "restores warm" sentence above only became true once the composite
> preamble set up **sccache** (content-addressed compile cache) for the
> `corpus` job. Before it, a fresh checkout's source mtimes invalidated the
> restored target dir and the job recompiled the workspace chain anyway —
> the same mechanism that killed the original cross-job handoff. sccache
> serves the recompiles from source content, so the cross-run restore hits;
> the job merge stays the right shape for within-run sharing.

## The recipe-only path is fixed

`docs/cookbook/*` edits are docs-only changes, so `code` stays false. The
old wiring made `cookbook-native` depend on `corpus-build` (which was
skipped on docs-only changes), so the native recipe gate silently skipped
on exactly the changes it exists to gate. The `changes` job now emits a
`cookbook` output for `docs/cookbook/*`, and the `corpus` job runs when
`code == true || cookbook == true`, with the TCK/CIP steps gated to
`code == true` and the recipe step always running. The recipe gate now
actually runs on recipe-only edits, as ADR-0130 requires.

## Before / after (expected)

| Metric | Before (4 jobs) | After (1 job) |
| --- | --- | --- |
| Corpus-profile compiles per run | 4 (build job + 3 rebuilding consumers) | 1 (or 0 on a warm cache hit) |
| Corpus compute per run | ~1370 s | ~400–450 s (build ~327 s + ~1–2 min of gates) |
| Corpus wall time | ~660–720 s (needs-chained build, then the slowest consumer's rebuild) | ~400–450 s (the build plus the three gates, serial) |

Local verification of the commands: `cargo test --profile corpus --no-run
-p relate-tck -p relate-cip-eval -p relate-cookbook
-p relate-query-latency` builds all four test targets in one
invocation, and each gate's run command is then a
no-compile exec of the binaries just built.
