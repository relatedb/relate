---
type: adr
area: infra
name: 0251-test-architecture
status: accepted
tags:
  - ci
  - testing
title: Fast test architecture — memory-first tiers, OS-delta lanes, build-once, background cadences
REFERENCES:
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0243-query-latency-suite]]"
  - "[[adr/0244-in-process-query-sampling-profiler]]"
  - "[[adr/0252-corpus-family-single-job]]"
  - "[[adr/0254-cargo-nextest-test-execution]]"
---

# ADR-0251: Fast test architecture — memory-first tiers, OS-delta lanes, build-once, background cadences

## Context

The test suite has grown with the engine. Today a typical CI run and a local
`cargo test` pass are dominated by work that does not need to be where it is:

- Storage-coupled tests (temporary directories on a real filesystem) run in
  the default suites even when the behavior under test (parser, planner,
  executor semantics) never touches disk; the `:memory:` VFS exists and is
  exercised for persistence roundtrips, but is not the default for tests
  that merely need a `Database`.
- The OS matrix runs the full suite on macOS and Windows lanes even though
  the vast majority of scenarios are OS-independent; open PR #293
  (`triha/os-smoke matrix split`) has already started carving an OS-smoke
  delta out of the duplicated lanes.
- Smoke lanes meant to answer "does the artifact install and run at all"
  rebuild the workspace from scratch in each lane, and the Docker lanes
  rebuild in-container; local CLI e2e re-resolves the full dependency graph
  on every invocation.
- Heavy gates — the full TCK corpus (3892/3892, `docs/tck/compliance.md`),
 loadtest baselines ([ADR-0070](./0070-docker-baseline-loadtest-environment.md), [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)), the wasm site build, LDBC query
 latency ([ADR-0243](./0243-query-latency-suite.md)), multi-OS sweeps — all run on the same cadence as the
  unit suite, so every PR waits on work whose answers change on the scale of
  hours, not minutes.
- Locally, the AGENTS.md parallel-gates rule ("run independent gates as
  concurrent background shells … a per-invocation `--target-dir` or
  sequential retry resolves it") already codifies the fix for shared-target
  contention, but CI and the make targets still serialize gates that could
  run concurrently.

The goal is a test architecture where every continuous suite and job answers
in ≤ 60 s, coverage is not reduced, and slow-but-necessary verification moves
to background cadences that file tickets on failure. This is Linear LABS-310
(authored and approved by Michael).

## Decision

The CypherLite test architecture is memory-first, delta-deduplicated on the
OS matrix, build-once per SHA, and cadence-split for heavy gates. Coverage
never shrinks: every test moved between tiers keeps running, in the tier
that matches its actual dependency.

### 1. Memory-first test tiers

Three tiers, chosen by what the test actually needs:

- **Default tier — pure unit.** Tests with no store dependency at all
  (parser, planner, catalog, model, error, splitter, formatter units) run
  against in-memory data only, no `Database`, no VFS.
- **Memory tier — everything that does not need a filesystem.** Integration
  and contract tests that need a `Database` run against the `:memory:` VFS.
  This is the default for any new test that opens a store.
- **Disk tier — the storage suite plus genuinely OS/path/durability-dependent
  tests.** The persistence layer (WAL, checkpoint, snapshot codecs, Native
  VFS roundtrips) and tests whose subject is real path handling, fsync
  semantics, file locking, or durability stay on the real filesystem. The
  disk tier is the only tier allowed to create temporary directories.

Tier membership is per test, not per crate: `cypherlite-storage` holds the
bulk of the disk tier, but a path-handling test in the CLI or a
fsync-dependent test elsewhere is disk tier, and a pure roundtrip test
inside `cypherlite-storage` may be memory tier. Every moved test keeps its
coverage; moves are migrations, not deletions.

### 2. OS-matrix dedup

The full suite runs on Linux only in continuous CI. macOS and Windows lanes
run **the OS-dependent delta**: the disk tier's OS-sensitive subset
(path/permission/durability behaviors), platform-specific client builds
(JS/Java/Go wrappers where their toolchains behave differently), and the
platform smoke lanes (below). This work is **stacked on PR #293**
(`triha/os-smoke matrix split`): #293 lands first and establishes the
lane-split mechanism; this ADR's delta reduction builds on it. The stacking
is a merge-order dependency — #293 merges before the branch implementing
this ADR, and the delta selection is expressed with the same mechanism #293
introduces, not a parallel one.

### 3. Smoke lanes are true minimal e2e

A smoke lane answers exactly: does the artifact install, build, connect,
run one query, and exit? Each smoke lane is:

- **npm pack/install** — pack the JS client, install it into a clean
  project, run one query through the C ABI wrapper, exit.
- **Bolt + official driver** — start `cypherlite serve --bolt` (or
  `cypherlite-bolt`), connect with the official Neo4j driver for that
  language, run one query, disconnect, exit.
- **wasm** — build the browser bindings, load them, run one query in the
  browser harness, exit.

No smoke lane runs a second query, a second surface, or a dataset. Anything
deeper belongs to the memory tier (native) or a dedicated lane.

### 4. Machine-adaptive parallelism

- **Local:** full core and memory parallelism — the AGENTS.md
  parallel-gates rule applies (`--target-dir` per invocation; note the
  "cargo lock contention only hits a shared target dir" clause: distinct
  `--target-dir` values are the escape, sequential retry the fallback).
  Per-crate suites run as concurrent processes, each with a distinct
  `--target-dir`.
- **CI:** each job sizes its parallelism (`cargo` jobs, test threads) to
  the runner's vCPU/RAM rather than a hardcoded number. No gate that could
  run concurrently runs serially — any remaining serial sequence must be
  justified by an actual ordering dependency.
- **Fail-fast ordering:** the check that failed on the previous run (from
  GitHub check-run history) runs first, so a repeat failure surfaces in
  seconds instead of after the full matrix.

### 5. Build once per SHA, shared everywhere

A single build per SHA feeds all consumers:

- A **pinned shared target dir / prebuilt-binaries directory** holds the
  release binaries and test artifacts for the SHA; local CLI e2e and smoke
  lanes consume those binaries instead of re-resolving and rebuilding.
- **Docker lanes copy the prebuilt binary into the container** instead of
  rebuilding in-container. Full in-container builds (compiling the
  workspace inside the image, toolchain and all) move to a **nightly job**
  that verifies the from-scratch container build still works. This does not
 change [ADR-0070](./0070-docker-baseline-loadtest-environment.md): baseline loadtest *runs* still compile in
  `rust:1.88-bookworm` and execute in `debian:bookworm-slim` with the
  pinned 2 vCPU / 2 GB environment; only the duplicated, non-baseline
  in-container rebuilds are removed from the continuous path.

### 6. Background cadences

Continuous CI keeps a representative fast slice of every heavy gate; the
full versions move to timed cadences:

- **TCK:** a fast smoke slice (a fixed, representative scenario sample plus
  every scenario touched by the diff when that is cheap to select) runs
  continuously; the **full 3892-scenario corpus runs nightly**. The
  allowlist (`testing/cypherlite-tck/src/allowlist.txt`) is unchanged and
  the 3892/3892 gate stands (`docs/tck/compliance.md`) — only its cadence
  changes. A nightly TCK failure files a ticket instead of blocking every
  PR; the continuous slice still blocks PRs on language regressions.
- **Loadtest baselines:** nightly, in the pinned [ADR-0070](./0070-docker-baseline-loadtest-environment.md) environment,
 compared per [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md); failures file tickets.
- **wasm site build + multi-OS sweeps:** 6-hourly, with a continuous
  representative subset (the wasm cookbook parity slice, one non-Linux
  delta lane per OS).
- **LDBC query latency ([ADR-0243](./0243-query-latency-suite.md)/[ADR-0244](./0244-in-process-query-sampling-profiler.md)):** nightly full SF0.1 suite,
  continuous p50 spot-check of recently-touched query ids.

### 7. Non-goals

This ADR does not delete any coverage, does not change the TCK allowlist or
skip list, and does not modify the [ADR-0070](./0070-docker-baseline-loadtest-environment.md) baseline environment or the
[ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) CI-owned envelope workflow. Coverage moves between tiers and
cadences; nothing is dropped.

## Alternatives considered

- **Keep everything on the real filesystem (status quo):** rejected — it
  couples every PR's wall-clock to disk I/O the tests do not need, and it
  makes the disk tier's failures (which are the interesting ones) rare and
  diluted.
- **Memory tier only (drop the disk tier):** rejected — the persistence
  layer is a core product property (fsync'd WAL, Parquet checkpoints) and
  path/durability behavior genuinely differs per OS; removing the real-VFS
  suites would delete coverage and violate AGENTS.md ("WAL + snapshot
  roundtrip on Native and Memory VFS").
- **Full suite on every OS lane (status quo matrix):** rejected — macOS and
  Windows runners cost multiples of Linux ones and re-answer the same
  questions; the OS-dependent subset is small and is what those lanes
  uniquely verify. PR #293 already established this direction.
- **Forking a separate smoke repo/matrix outside the workspace:** rejected —
  smoke lanes drift without the same CI ownership; the lanes stay in this
  repository's workflows next to what they smoke-test.
- **Fixed parallelism everywhere (one `-j` number, one runner size):**
  rejected — it under-uses big local machines and over-subscribes small CI
  runners; parallelism adapts to the machine as a rule, not per-job tweaks.
- **Rebuild in each lane (status quo) or a shared remote cache only:**
  rejected — a remote cache still pays network and cache-miss cost on every
  lane; a SHA-pinned local prebuilt artifacts dir plus Docker COPY is
  deterministic and free after the one build.
- **Everything continuous (no background cadences):** rejected — the full
  TCK, loadtests, and wasm build take far past the ≤ 60 s continuous
  target and their answers change on hour scales; keeping them on every PR
  both burns runner time and slows every PR with no signal gain.
- **Everything background (no continuous slices):** rejected — a language
  regression would then surface a day later via a ticket, not on the PR
  that introduced it; the fast slices keep PR-blocking signal for exactly
  the classes of change PRs introduce.
- **Reduce coverage to hit the runtime target:** rejected — the runtime
  target is met by moving and parallelizing work, not deleting it; AGENTS.md
  ("No behavior without tests", TCK floor 3892/3892) forbids it, and §7
  records it as a non-goal.

## Consequences

- **Runtime target: every continuous suite and CI job completes in ≤ 60 s.**
  This is a design constraint for the implementation PRs: a lane that cannot
  meet it moves work to a background cadence or is split, never silently
  dropped.
- **Runtime dashboard seed.** The per-suite before/after table — suite,
  tier, before-runtime, after-runtime — is the seed for the runtime
  dashboard and is recorded in the test docs
  (`docs/testing/`) as the standing research kickoff for section 7 of
  LABS-310; the implementation PRs fill it from real runs, not estimates.
- **Fail-fast ordering depends on GitHub check-run history.** The
  previously-failed-check-first ordering reads GH check history; on a fresh
  branch with no history, or after a rename of a check, it degrades to
  the default order. Check ids are treated as stable identifiers in the
  workflows (renames must update the ordering mapping).
- **Background cadences file failures as tickets, not red PRs.** A nightly
  TCK or loadtest failure opens a tracked issue (Linear) with the failing
  run attached; the continuous slice remains the PR-blocking signal. The
  3892/3892 compliance claim in `docs/tck/compliance.md` remains true on
  every nightly run — a nightly failure is a next-day fix obligation, not
  a renegotiation of the floor.
- **Tier moves are the review surface.** Migrating a test from disk to
  memory tier can hide an accidental filesystem dependency; tier moves in
  review get explicit attention for what the test actually observes.
- **The stack order is binding:** PR #293 merges first; this ADR's
  implementation branches stack on it (`--base` of the #293 branch until
  it lands) per the AGENTS.md stacking rule.
- **[ADR-0070](./0070-docker-baseline-loadtest-environment.md)/[ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) are untouched.** Baseline loadtests keep their pinned
  environment, CI-owned envelopes, and compare rules; only their cadence
  (nightly) and the removal of unrelated in-container rebuilds change.

Later references: the §5 shared-build wiring (per-SHA prebuilt directory,
`CYPHERLITE_BIN` consumers, the Dockerfile `BINARY_SOURCE=prebuilt` path,
and the CI rust-cache shared keys) is documented in
`docs/testing/build-caches.md`. The §6 cadence wiring is documented in
`docs/testing/cadences.md`, with two reconciliations against the sketch
above: the full TCK corpus stays continuous ([ADR-0252](./0252-corpus-family-single-job.md)'s merged `corpus`
job made it seconds-cheap, so the nightly demotion never pays), and the
loadtest-baselines job stays continuous (it is the [ADR-0070](./0070-docker-baseline-loadtest-environment.md)/[ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)
CI-owned freeze gate, which only works on the merge path); the §6
allowlist/3892-gate guarantees and the "cadence failures file tickets"
contract are unchanged. The §5 `dev` shared-key family later gained a
`prebuild` warmer job that the dev-key lanes `needs:`-chain on; the
handoff design, the warm-run critical-path cost, and what stays out of
the chain are recorded in `docs/testing/build-caches.md` ("The `prebuild`
handoff"). The warmers were removed in LABS-310 round 3 (the
feature-unified warmer fingerprints never matched the lanes' per-crate
ones, so the warmer recompiled ~5m34s every run and only gated); the
lanes now self-warm with a cold-miss-only `save-if`, recorded in the
same build-caches.md section.

Later references (post-merge learnings, first green run 38705035809):
the merged state was measured and confirmed three follow-ups, which the
ci-runtime-next round ([ADR-0254](./0254-cargo-nextest-test-execution.md) onward) applies: (a) the free-disk step
must not run on the larger-runner lanes (150 GB SSD) — moving a job
there changed only runs-on and left the cleanup paying >1 minute for
zero benefit; (b) `cargo test -p A -p B …` executes test binaries
serially (only libtest threads parallelize within a binary), so the
parallelism section's next step is cargo-nextest with doctests and the
TCK single-binary corpus harness deliberately left on stock cargo test;
(c) the rust-cache shared keys were silently fragmented by env drift:
the key's environment hash covers every `CARGO*`/`RUST*` variable, and
the `test` job alone set `CARGO_INCREMENTAL=0`/`CARGO_PROFILE_DEV_DEBUG`
before the cache step, so it never shared the `dev` cache it anchored
(run 38706915770: test hashed `b2ab3321`, the other dev lanes
`b74045d1`). Verified against the action source: `add-job-id-key` is a
no-op when `shared-key` is set (the job id is appended only in the
no-shared-key branch), so the fix is identical workflow-level env on
every member lane, not key flags — mechanism recorded in
docs/testing/build-caches.md; (d) the docker image build dominated the
bolt-smoke lane (a full in-container musl build on a buildx cache
miss), so the full build moved to the nightly cadence and the
prebuilt `BINARY_SOURCE=prebuilt` path is the continuous answer for
image-dependent smokes. Follow-up noted for the next round (recorded on LABS-310): the
~15 hand-copied copies of the setup preamble (checkout + mtime
normalize + rust-cache + free-disk + toolchain) should become one
composite action (`.github/actions/setup-rust-ci`); the per-lane
hand-copying is the drift class that produced the free-disk-on-larger-
runner defect, and GitHub Actions has no cross-file include, so a
composite action is the mechanism.
