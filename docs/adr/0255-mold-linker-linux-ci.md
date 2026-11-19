---
type: adr
area: infra
name: 0255-mold-linker-linux-ci
status: accepted
tags:
  - ci
title: mold links every Linux CI build; CARGO_INCREMENTAL and the dev debug-info trim are workflow-level env
REFERENCES:
  - "[[adr/0251-test-architecture]]"
  - "[[adr/0252-corpus-family-single-job]]"
  - "[[adr/0253-test-profile-opt-level]]"
---

# ADR-0255: mold links every Linux CI build; CARGO_INCREMENTAL and the dev debug-info trim are workflow-level env

## Context

The CI runtime follow-up round (after [ADR-0251](./0251-test-architecture.md)/[ADR-0252](./0252-corpus-family-single-job.md)/[ADR-0253](./0253-test-profile-opt-level.md)) profiled the
remaining lane time. Two structural costs stood out:

1. **Linking.** The workspace's debug-profile test/example binaries are
   large and numerous (the `test` job alone links ~17 crates' worth of lib
   and integration binaries, each tens of MB even with
   `line-tables-only`). GNU ld links them one at a time inside each rustc
   invocation and is single-threaded for most of its work. This is exactly
   the workload mold was written for (many large, debug-info-heavy
   binaries), and mold is a drop-in `-fuse-ld=mold` switch on the
   gcc/clang driver Ubuntu runners ship.
2. **Per-job env drift.** `CARGO_INCREMENTAL=0` and
   `CARGO_PROFILE_DEV_DEBUG=line-tables-only` were set only on the `test`
   job (via `GITHUB_ENV` steps), so every other lane paid incremental
   compilation's multi-GB on-disk cache churn (`target/debug/incremental`)
   with zero warm-rebuild benefit — CI builds are one-shot — and some lanes
   still linked full debug info.

The mold flag env var (a CARGO_TARGET_*_RUSTFLAGS, also a CARGO_* var) is part of the Swatinem/rust-cache environment hash (it hashes
all `CARGO*`/`CC`/`CFLAGS`/`CXX`/`RUST*` env vars into the key), so the
linker flag must be identical across all lanes that share a cache key or
the `dev`/`release`/`corpus` sharing of [ADR-0251](./0251-test-architecture.md) §5 silently splits.

Not in scope: local builds (`.cargo/config.toml` would leak CI choices
onto developer machines — deliberately not used), macOS/Windows lanes
(mold's macOS port is not a drop-in for ld64 here, and the lanes are
smoke-scale), and the Docker image build (musl static links inside
Dockerfile, a from-scratch guarantee lane).

## Decision

1. Every ci.yml lane that compiles Rust on Linux installs mold via
   `sudo apt-get update -qq && sudo apt-get install -y mold` (mold is a
   system package in the runner image's apt repos — it is NOT a Rust
   crate, so neither `taiki-e/install-action` nor cargo-binstall can
   install it; a cargo-binary action was tried first and fails
   `install-action does not support mold`) and sets
   `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"`
   through `$GITHUB_ENV` (target-scoped, NOT the blanket RUSTFLAGS: a global
   RUSTFLAGS leaks the mold flag into the wasm32 target build, whose rust-lld
   linker rejects `-fuse-ld=mold` — the npm-smoke lane failed on exactly that) in a
   `runner.os == 'Linux'`-guarded step, so the same job definition stays
   correct if a lane's matrix grows a non-Linux member. All Linux lanes
   share the identical flag string, keeping the rust-cache env hash
   aligned.
2. ci.yml gains a workflow-level `env:` block with
   `CARGO_INCREMENTAL: "0"` and `CARGO_PROFILE_DEV_DEBUG: line-tables-only`;
   the per-job copies (the `test` job's two `GITHUB_ENV` steps and the
   three `dev`-lane env blocks) are removed. Comments on the shared-key
   lanes now point at the workflow-level block as the single source.
3. The `test` job gains a diagnostic-only `cargo build --timings` step
   whose HTML report uploads as an artifact — a free measurement of the
   serial compile tail that informs the sharding question
   (`docs/testing/runtime.md`), not a policy or gate.
4. The scheduled browser-smoke lane's from-source
   `cargo install wasm-pack --version 0.13.1 --locked` (~2–4 min of
   compile) is replaced by a prebuilt install via
   `taiki-e/install-action` at the same pinned version, and
   `scripts/install-wasm-bindgen.sh` learns the same prebuilt-first path
   (falling back to the `cargo install --locked` build when no prebuilt
   tarball exists for the host) — the npm-smoke and cookbook lanes were
   each spending ~2 min compiling wasm-bindgen-cli per run.

## Alternatives considered

- **`.cargo/config.toml` with a `[target.x86_64-unknown-linux-gnu]`
  linker table.** Rejected: it is checked in, so it would also rewrite
  every local Linux build's linker — a CI-only choice must not leak onto
  developer machines. `$GITHUB_ENV` scoping keeps it CI-local.
- **lld (`-fuse-ld=lld`, ships with rustup).** Rejected: mold is measurably
  faster than lld on exactly this workload (many large debug binaries) and
  the install cost is seconds; lld remains the fallback if mold ever
  breaks a lane.
- **sccache or a linker-wrapper script.** Rejected: sccache caches object
  files, not the link step that dominates here, and adds a cache server to
  operate; a wrapper script is another moving part for the same effect
  `-fuse-ld=mold` gives natively.
- **Leaving incremental on for cache warmth across jobs.** Rejected:
  rust-cache does not restore `target/debug/incremental` usefully across
  the mtime boundary even with normalization, and the on-disk incremental
  state is what tipped lanes over the free-tier disk ceiling (ci.yml
  `test` job history).

## Consequences

- All Linux CI link steps use mold; expected link-time reduction is
  large (mold's design point) and is recorded per-lane in
  `docs/testing/runtime.md` after the first runs.
- The rust-cache env hash now includes the host-target-scoped mold flag on every Linux
  lane: the first run after this lands is a cold compile for every
  shared-key family, then warm again.
- One workflow-level env block is the single source for the one-shot-build
  env contract; a new lane inherits it instead of copying two GITHUB_ENV
  steps.
- Local builds are untouched: no repo-resident cargo config changes.
- The build-timings artifact gives the serial-bottleneck data for any
  future test-job sharding decision without another instrumented run.
