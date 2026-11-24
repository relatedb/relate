---
type: adr
area: infra
claim: efeafc88c9cb0427740e620cdc974aca2afb6251
name: 0671-rust-toolchain-msrv-1-99
status: accepted
status_note: 2026-11-24
tags:
  - ci
  - release
  - workspace
title: Rust toolchain moves to MSRV 1.99, pinned 1.99.1
SUPERSEDES:
  - "[[adr/0443-rust-toolchain-msrv-1-88]]"
REFERENCES:
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
  - "[[adr/0255-mold-linker-linux-ci]]"
---

# 0671: Rust toolchain moves to MSRV 1.99, pinned 1.99.1

## Context

[ADR-0443](./0443-rust-toolchain-msrv-1-88.md) kept MSRV 1.88 as a
compatibility contract with the embedded consumers — the Python wheel,
the C ABI, the WASM package, the Swift xcframework. That contract binds
only source-builders, and every one of those consumers ships a prebuilt
artifact from this repository's release pipeline (wheel via maturin, FFI
`.so`/`.a` tarballs, the npm bundle, the xcframework); the repository is
private with no external source-builders, so the floor's remaining reach
is our own lanes.

Since 1.88 the pressure inverted:

- The columnar property-store evaluation (Vortex) is blocked behind a
  lab-only `RUSTUP_TOOLCHAIN=1.95` lift in its report-only CI lane:
  vortex 0.93.0 declares `rust-version = 1.95`, which 1.88 cannot build.
- std carries what we shim: `File::try_lock` (1.89) covers the fs4 file
  locking role in `relate-storage` and `relate-feedback`, and
  `Atomic*::try_update` (1.95; `fetch_update` deprecated in 1.99)
  replaces the deprecation-guarded macro in the executor.
- 1.99.1 (2026-10-13) is the newest release line with a patch release
  and sits inside the current wasm-bindgen support window: wasm-bindgen
  0.2.130 (2026-10-30) predates rustc 1.101.0 (2026-11-05), the current
  stable, by six days — building the WASM surface on a rustc newer than
  the bindgen release is the known breakage shape.

## Decision

Bump the floor to 1.99 and the pin to 1.99.1 across every site the
coordinated-change checklist in [ADR-0443](./0443-rust-toolchain-msrv-1-88.md)
names: workspace `rust-version`, `rust-toolchain.toml`, the
`dtolnay/rust-toolchain` @rev pins in every workflow and
`setup-rust-ci`, the Sonic image tags and their Dockerfiles, the product
`Dockerfile` (alpine, digest-pinned), the baseline build image
(`Dockerfile.baseline`, `Makefile`, `scripts/baseline-env.sh` —
digest-pinned — and the target cache id), and the AGENTS.md Toolchain
section.

The wasm toolchain moves with it, one release behind the rustc curve:
the wasm-bindgen family to 0.2.130 (crate and CLI stay lock-matched by
`scripts/install-wasm-bindgen.sh`, which reads the version out of
`Cargo.lock`, prebuilt-first per [ADR-0255](./0255-mold-linker-linux-ci.md)),
wasm-pack 0.13.1 → 0.15.1, binaryen unchanged at 133 (current). The
new-lint findings 1.99 fires (`is_multiple_of`,
`cloned_ref_to_slice_refs`, the `fetch_update` deprecation) are fixed in
the same change, not allowed.

Every frozen `baselines/` envelope re-freezes through the
`create-loadtest-baselines.yml` dispatch: the environment class changes
(`rust:1.99-bookworm`), and the `loadtest-baselines` gate hard-fails
`INCOMPATIBLE` on class mismatch ([ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md),
[ADR-0245](./0245-query-latency-baseline-environment-and-gate.md)) — the
red window on main between the bump merge and the freeze merge is
accepted, with the freeze dispatched from the bump branch and its PR
merged immediately after.

## Alternatives considered

- **Stay on 1.88** ([ADR-0443](./0443-rust-toolchain-msrv-1-88.md)) —
  rejected: the Vortex evaluation cannot leave its exception lane; the
  std shims (fs4, the deprecated `fetch_update`) stay; lint coverage
  stagnates four minors behind while local Homebrew toolchains run
  ahead of the pin.
- **Bump to the newest stable (1.101.0)** — rejected: wasm-bindgen
  0.2.130 predates it by six days; the WASM surface builds on a rustc
  the bindgen release has not seen. 1.99.1 is the newest line the whole
  toolchain (wasm-bindgen 0.2.130, binaryen 133, wasm-pack 0.15.1)
  demonstrably covers.
- **Bump the floor to exactly 1.95** (Vortex's own floor) — rejected:
  no headroom for the next vortex minor (it releases roughly monthly);
  the internal cost of the bump is identical at 1.99.1, which buys four
  more minors of margin.

## Consequences

- Every consumer still consumes prebuilt artifacts; nothing external
  compiles the crate. The binary runtime floors (Linux glibc/ISA, Apple
  minimums, the wheel's manylinux tag) are measured and declared by the
  release pipeline (`scripts/check-glibc-floor.sh` stamps the `.deb`
  `Depends`), not assumed — a rustc-driven floor raise surfaces there,
  not silently.
- The frozen baselines re-freeze with the new environment class; perf
  numbers move with the new codegen, and the re-validation ticket
  records the toolchain version beside its CPU accounting.
- `fs4 = "=1.1.0"` (storage lease locking, `relate-feedback`) is
  droppable via std `File::try_lock` — a follow-up change, not this one.
- LLVM 23.1.1 arrives with 1.99.1: the macOS `strip = "symbols"` dyld
  question (rust-lang/rust#157750, fixed upstream in LLVM 23) becomes
  re-evaluable; the release-profile recipe is unchanged in this change.
- The Vortex lab's `RUSTUP_TOOLCHAIN=1.95` lift becomes redundant once
  that lane lands on main — it can ride the standard pin.
- Local gate parity is `rustup run 1.99.1 cargo clippy …`.
- A future bump is the same coordinated change plus a **Later
  references** note on this ADR.
