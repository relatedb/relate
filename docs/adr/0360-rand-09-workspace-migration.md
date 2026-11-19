---
type: adr
area: infra
claim: d80cd0ea279b608006a8ca47137def0a71c68c5b
name: 0360-rand-09-workspace-migration
status: proposed
tags:
  - workspace
title: Workspace rand 0.9 migration
REFERENCES:
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0085-wasm32-build-split]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0268-proptest-property-testing]]"
SUPERSEDES:
  - "[[adr/0268-proptest-property-testing]]"
---

# ADR-0360: Workspace rand 0.9 migration

## Context

The workspace pinned `rand = "0.8"` in the root `Cargo.toml`. It is used by
`cypherlite-executor` (shortest-route parity generation and the random-value
functions), `cypherlite-datasets` (fraud and identity dataset/loadtest
generators), `cypherlite-auth` (token bytes) and the optional CLI `telemetry`
feature (UUID v4 bytes).

A full rand 0.9 migration was attempted in 2026-09 and reverted; LABS-136
tracks that attempt. Two consequences blocked it at the time:

- The frozen fraud/identity loadtest envelopes shifted, because rand 0.9's
  range sampling produces different dataset content from the same seed. The
  reverted attempt measured identity `logical_used_start_bytes` at 12,145,027
  on rand 0.8 and 12,118,136 on rand 0.9 — −26,891 B, −0.22% — and fraud
  shifted likewise. Those figures are historical: the 2026-10-14 re-freeze
  (PR #290) superseded them, so they describe the reverted attempt, not the
  envelope this change measures against.
- `wasm32-unknown-unknown` broke: rand 0.9 pulls getrandom 0.3 through
  rand_core's `os_rng` path, while the wasm target had only wired getrandom
 0.2's `js` backend ([ADR-0085](./0085-wasm32-build-split.md)).

Deferring no longer avoids a second RNG major: proptest 1.11, adopted as a
dev-dependency by [ADR-0268](./0268-proptest-property-testing.md), already pulls rand 0.9.5 into `Cargo.lock`. The
workspace therefore carried both families, and the two test paths could not
share an RNG. The migration is also an API modernization: rand 0.9 renames
`thread_rng()` to `rng()`, `gen_range` to `random_range`, `gen` to
`random`, and `gen_bool` to `random_bool`. The remaining surface the
workspace uses — `rand::random()`, `StdRng`, `SeedableRng::seed_from_u64`,
`RngCore::fill_bytes` and `IteratorRandom::choose_multiple` — is already
name-compatible with 0.9 and needs no call-site change.

## Decision

1. **One workspace rand major.** The root `Cargo.toml` declares
   `rand = "0.9"` (resolved to 0.9.5). `cypherlite-executor`,
   `cypherlite-datasets`, `cypherlite-auth` and the optional CLI `telemetry`
   feature keep consuming `rand = { workspace = true }`; no per-crate
   version is pinned and no compatibility shim or wrapper is added.

2. **The rename surface is applied at every call site.** `thread_rng()` →
   `rng()`, `gen_range` → `random_range`, `gen` → `random`, `gen_bool` →
   `random_bool`. The already-compatible spellings listed above stay as
   they are: `rand::random()`, `StdRng`, `SeedableRng::seed_from_u64`,
   `RngCore::fill_bytes` and `IteratorRandom::choose_multiple`.

3. **wasm32 wiring is target-scoped in `cypherlite-storage`.** rand 0.9's
   getrandom 0.3 dependency needs the `wasm_js` backend on
   `wasm32-unknown-unknown`, beside the existing getrandom 0.2 `js` entry.
   The storage crate's `[target.'cfg(target_arch = "wasm32")'.dependencies]`
 carries both (the [ADR-0085](./0085-wasm32-build-split.md) pattern, so every wasm consumer of storage
   inherits the fix):

   The target-scoped entry resolves to getrandom 0.3.4 (the manifest's
   `0.3.4` requirement; `Cargo.lock` pins 0.3.4):

   ```toml
   getrandom = { version = "0.2", features = ["js"] }
   getrandom03 = { package = "getrandom", version = "0.3.4", features = ["wasm_js"] }
   ```

   As of getrandom 0.3.4, the Cargo `wasm_js` feature alone selects that
   backend. The `--cfg getrandom_backend="wasm_js"` spelling is therefore
   unnecessary and must not be set: it would override the feature-selected
   backend, not act as a no-op. No CI workflow changes were needed: the
   existing wasm job's `cargo check` builds pick the target-scoped dependency
   up.

4. **The frozen envelopes are re-frozen by CI, not hand-edited.** The RNG
   swap changes generated dataset content and therefore the logical-bytes
   figures recorded in the frozen fraud/identity baselines. Re-freeze is
 CI-owned per [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md): dispatch
   `.github/workflows/create-loadtest-baselines.yml` from the PR branch; the
   workflow opens its envelope PR with `--base "$GITHUB_REF_NAME"`, so it
   bases back onto that branch. The `loadtest-baselines` job is skipped on
   draft PRs (the `changes` job sets `full_ci=false`) and runs once the PR
   is marked ready for review. Its committed `Compare against baselines` step
   is currently commented out in `.github/workflows/ci.yml`, so CI does not
   gate on logical bytes today; the re-freeze is still required for
   `make gate` and for whenever that compare step is re-enabled. The envelope
   diff is the expected content change, not a regression; no band is widened.

 Measured on this branch with the [ADR-0070](./0070-docker-baseline-loadtest-environment.md) Docker baseline environment on
   aarch64 local hardware: `environment.class`
   `cypherlite-loadtest-baseline/v1`, 2 vCPU / 2 GB, `errors == 0`. Only
   logical bytes are compared. This is not a class-matched freeze run — its
   architecture and runner differ from the CI envelope — so treat the figures
   as a content comparison, not as freeze-comparable numbers. The figures
   below are the post-refactor re-measurement: the dataset generators were
   refactored to canonical pickers, structural exclusion and
   `choose_multiple`, which changed the seeded RNG draw sequence, so the
   pre-refactor measurement no longer describes this branch.

   The before-values come from two different re-freezes:
   `logical_used_start_bytes` is the 2026-10-14 re-freeze (PR #290) and is
   unchanged by the 2026-10-17 re-freeze (PR #328); `logical_used_bytes` is
   the 2026-10-17 re-freeze (PR #328).

   - fraud: `logical_used_start_bytes` 10,224,607 → 10,362,719 (+138,112 B);
     `logical_used_bytes` 16,914,079 → 16,828,667 (−85,412 B).
   - identity: `logical_used_start_bytes` 2,252,236 → 2,251,071 (−1,165 B);
     `logical_used_bytes` 7,697,926 → 7,729,688 (+31,762 B).

   Those are the current-envelope deltas, not the historical 2026-09 attempt's
   numbers.

5. **The residual dual version is accepted and bounded.** rand 0.8.8 stays
   in `Cargo.lock` only through `neo4rs 0.8`, on two paths:
   `neo4rs → backoff 0.4` and
   `neo4rs → chrono-tz 0.8 → chrono-tz-build → phf_codegen → phf_generator
   → rand 0.8.8`. `cypherlite-storage` keeps its
   direct getrandom 0.2 dependency and adds the aliased getrandom 0.3 entry
   above. proptest 1.11 already pulled rand 0.9.5 in, so this migration
   deduplicates the **workspace side**: the workspace, executor, datasets,
   auth and telemetry share one rand major, and the storage test build no
   longer links a second one.

## Alternatives considered

- **Stay on rand 0.8 indefinitely** — rejected: proptest 1.11 already forces
  rand 0.9.5 into the dependency graph, so the workspace carried two RNG
  majors and could not dedupe; the earlier deferral was for the baseline and
  wasm mechanics, both of which this change settles, and the API
  modernization is due.
- **Use `RUSTFLAGS=--cfg getrandom_backend="wasm_js"` instead of the Cargo
  feature** — rejected: at getrandom 0.3.4 the Cargo feature alone selects the
  `wasm_js` backend, and setting the cfg would override the feature-selected
  backend rather than duplicate it; the feature is the supported mechanism.
- **Re-freeze the baselines from a host-native run, or hand-edit the
 logical-bytes figures** — rejected: [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) makes committed envelopes
  CI-owned, and `make gate` hard-fails on an environment-class mismatch;
  host numbers are not comparable and hand-editing would hide the real
  content change.

## Consequences

- The workspace has one rand major (0.9.5); call sites use the 0.9 names.
  No behavior change is intended beyond the RNG stream itself.
- The wasm32 build keeps routing to `crypto.getRandomValues` through both
  getrandom majors; the wiring is target-scoped, so native builds are
  unaffected and no CI file changed.
- Seeded datasets remain reproducible within rand 0.9, but are not
  byte-identical to rand 0.8 output — the measured post-refactor
  current-envelope deltas above (fraud `logical_used_start_bytes` +138,112 B,
  identity −1,165 B) are that content change. The prior baseline is PR #328's
  envelope; this change's own re-freeze is still pending — a CI-owned dispatch
 in the PR phase ([ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)). Because the committed CI compare step is
  commented out, CI does not fail on logical bytes today, but `make gate`
  and a re-enabled compare require the new envelopes.
- rand 0.8.8 remains in the lockfile only via `neo4rs 0.8`, on both the
  `backoff` and `chrono-tz → chrono-tz-build → phf_codegen → phf_generator`
  paths, for the native-only Aura reference; `cypherlite-storage` keeps
  getrandom 0.2 (native and wasm `js`) beside the aliased getrandom 0.3.4
  (`wasm_js`). Neither is a workspace rand major.
- [ADR-0268](./0268-proptest-property-testing.md) §2's `rand 0.8` workspace pin is superseded for the workspace;
  its proptest adoption stands, and the test build now unifies with the
  workspace's rand 0.9 instead of linking a second major.
- rand 0.9 removed `ThreadRng`'s automatic reseed-on-fork (rand 0.8 reseeded
  through `pthread_atfork`). A pre-fork embedder — Python under
  `gunicorn --preload`, for example — must reseed in the child with
  `rand::rng().reseed()`. The shipped binaries here do not fork, so this is
  a consequence for embedders, not a product regression.
- [ADR-0103](./0103-engine-surfaces-testing-layout.md)'s R6 does not trigger: rand is an existing dependency and this is
  a major-version bump of an existing dependency, not a new heavy dependency.
- A future rand 0.10 migration is a new decision and a new ADR.
- References: [ADR-0070](./0070-docker-baseline-loadtest-environment.md), [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md), [ADR-0085](./0085-wasm32-build-split.md), [ADR-0268](./0268-proptest-property-testing.md) (the reverted
  2026-09 attempt).
