---
type: adr
area: infra
claim: 9bfa8230a7acb15ea5b205056c60224c7beb27a5
name: 0443-rust-toolchain-msrv-1-88
status: accepted
status_note: 2026-10-29
tags:
  - workspace
title: Rust toolchain stays on MSRV 1.88
REFERENCES:
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
---

# ADR-0443: Rust toolchain stays on MSRV 1.88

## Context

MSRV 1.88 is pinned across the workspace: `rust-version = "1.88"` in the
root `Cargo.toml`, `rust-toolchain.toml` (channel `1.88.0`), the CI pin
(`dtolnay/rust-toolchain@1.88.0` in `.github/workflows/ci.yml`), the
loadtest baseline Dockerfiles (`rust:1.88-bookworm`, [ADR-0070](./0070-docker-baseline-loadtest-environment.md)), `Makefile`,
and the AGENTS.md Toolchain section.

1.88 is also baked into the baseline freeze environment: every envelope
under `baselines/` was frozen with a `rust:1.88-bookworm` compile stage and
carries that environment class; the CI `loadtest-baselines` job hard-fails
on class mismatch (`INCOMPATIBLE`, [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md), [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md)).

On 2026-10-29 the question was raised whether to move to the most recent
stable Rust (1.99.1, 2026-10-13).

## Decision

Stay on MSRV 1.88. The toolchain floor is a compatibility contract with
CypherLite's embedded consumers — the Python wheel, the C ABI, the WASM
build, the Swift xcframework — not a developer-toolchain choice. Dev
machines may run a newer stable locally; gate parity runs through
`rustup run 1.88` (AGENTS.md Toolchain). Bump only when a needed
language/stdlib feature or a security requirement forces it; each bump is
its own ADR with a **Later references** note on this one.

## Alternatives considered

- **Bump MSRV to latest stable (1.99.1)** — rejected: breaks downstream
  consumers on older distro toolchains with no compensating need (no gated
  feature or dependency requires > 1.88), and changes the baseline
  environment class — every frozen `baselines/` envelope would hard-fail
  `INCOMPATIBLE` until re-frozen through the
 `create-loadtest-baselines.yml` dispatch ([ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)), invalidating the
  current merge-gate compares.
- **Dual-track: keep the 1.88 floor and add a latest-stable CI lane** —
  rejected: doubles the lint/test surface for no current benefit; CI on
  1.88 already catches the failure mode that matters (accidental use of
  newer stdlib/API shipping to consumers).

## Consequences

- No downstream break; frozen baselines stay comparable.
- Language/stdlib features above 1.88 are unavailable until a bump ADR
  lands.
- A future bump is a coordinated change: workspace `rust-version`,
  `rust-toolchain.toml`, the `ci.yml` pin, the loadtest Dockerfiles,
  `Makefile`, the AGENTS.md Toolchain section, and a baseline re-freeze
  via the dispatch workflow — plus a **Later references** note here.
