---
type: adr
area: infra
claim: f1f6fefcf9946d7780656c560ba65105cf814ba9
name: 0476-quarantine-research-kernels
status: proposed
tags:
  - workspace
title: Quarantine the research kernels behind a non-default `research` cargo feature
REFERENCES:
  - "[[adr/0468-soa-batch-substrate]]"
  - "[[adr/0469-allreduce-inline-pruning]]"
  - "[[adr/0471-one-var-length-traversal-engine]]"
  - "[[adr/0473-contracted-traversal-kernels]]"
  - "[[adr/0474-compound-contains-seed-start-cost]]"
  - "[[adr/0475-gpm-early-conjuncts-and-riders]]"
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
---

# ADR-0476: Quarantine the research kernels behind a non-default `research` cargo feature

## Context

The execution-architecture feature ([ADR-0468](./0468-soa-batch-substrate.md)/[ADR-0469](./0469-allreduce-inline-pruning.md)/[ADR-0471](./0471-one-var-length-traversal-engine.md)/[ADR-0473](./0473-contracted-traversal-kernels.md)/[ADR-0474](./0474-compound-contains-seed-start-cost.md)/[ADR-0475](./0475-gpm-early-conjuncts-and-riders.md),
LABS-642/643/644/645/660/661) shipped two research-only kernel modules in
the product executor crate: `cypherlite-executor/src/graph/sol.rs`
(speed-of-light kernels for gbl q8/q19/q30) and
`cypherlite-executor/src/graph/qpp.rs` (PathDB QPP frontier kernels).
Both were marked `#[doc(hidden)]` — but doc-hiding is not quarantine:

- Both modules compiled into **every** build of the engine: default
  `cargo build`, all four CLI bundles (`full`/`agent`/`minimal`/`serve`),
  and the WASM build.
- Both were re-exported unconditionally on the public API of two crates:
  `pub use graph::{sol, qpp}` in `cypherlite-executor`'s lib.rs and
  `pub use cypherlite_executor::{sol, qpp}` in the `cypherlite` facade.
- The facade grew `Database::sol_graph()`, which hands out the `neo4j`
  namespace's live `Graph` by reference and **bypasses the read guard**
  every other `Database` accessor takes.
- Both harness examples in `testing/cypherlite-query-latency` self-
  described as "UNCOMMITTED, research-only — delete to revert" while
  being committed and compiled by every `--all-targets` pass.

The kernels are measurement tools, not engine behavior: hand-written
plain-Rust loops over the typed adjacency that quantify the gap between
the clause-at-a-time interpreter and direct access (LABS-644's frontier
follow-up builds on these numbers). They must stay runnable — and they
must read the graph's private internals (version-chain liveness, typed
adjacency chunks, interner ids), which is why they live in `graph/`
beside the structures they measure — but none of that may ship on the
default public API or in any product build.

The stellar pre-push round (blockers 1–6) flagged exactly this: research
code wired unconditionally into the product crate's public API.

## Decision

Gate all research-kernel code behind a **non-default `research` cargo
feature**:

- `cypherlite-executor` defines `research = []`; `pub mod sol;`,
  `pub mod qpp;` (in `graph/mod.rs`) and both `#[doc(hidden)]`
  re-exports in its lib.rs carry `#[cfg(feature = "research")]`.
- The `cypherlite` facade forwards it (`research =
  ["cypherlite-executor/research"]`); the two re-exports and
  `Database::sol_graph()` carry the same gate, so nothing research-only
  exists on the default public API of either crate.
- `testing/cypherlite-query-latency` forwards it and marks both harness
  examples `required-features = ["research"]`, so a plain
  `cargo run --example sol` reports the missing feature instead of
  failing to compile; `--features research` runs them unchanged.
- The feature is off in every bundle feature list (`full`, `agent`,
  `minimal`, `serve`), off in the WASM build, and never part of
  `default` anywhere.

The kernels keep their in-module private `Graph` access — that is the
point of their placement in `graph/` — and the header comments now state
the quarantine ("research-only, compiled only under the `research`
feature, not engine behavior") instead of the stale "UNCOMMITTED, delete
to revert" wording (LABS-672's wording half).

The long-term home for the primitive access these kernels hand-roll
(label scan with inline interned-prop predicates, bidirectional typed
adjacency, interner access, version-filtered live read) is the embedded
graph-native SPI ticketed as **LABS-455**: when that SPI lands, the
kernels become ordinary external consumers of a public, read-guarded
surface and the quarantine feature can be retired or folded into the
SPI's own dogfooding.

## Alternatives considered

- **Move the kernels to a `testing/*` crate.** Rejected: the kernels
  read `Graph`'s private internals (live version chains, typed adjacency
  chunks, interner ids) — a separate crate would need those fields made
  `pub`, widening the exact surface this quarantine exists to keep
  private.

- **Keep `#[doc(hidden)]` as the quarantine.** Rejected: doc-hiding is
  documentation, not a compile gate — the modules still compiled into
  every build, every bundle, and the WASM artifact, and still shipped on
  the public API of both crates, with `sol_graph()` bypassing the read
  guard every other accessor takes.

- **Delete the kernels outright.** Rejected: they are the measurement
  tools the frontier-NFA productionization (LABS-644 follow-up) builds
  on — the gap numbers and the fidelity notes (walk-vs-trail divergence,
  acceptance-on-arrival semantics) are the input to that ticket, and the
  LABS-455 SPI requirements were digested from them.

## Consequences

- Default builds of `cypherlite` and `cypherlite-executor` contain no
  `sol`/`qpp` symbols; the default public API is unchanged by the
  research program.
- Running the harnesses now requires the explicit opt-in
  (`cargo run --release -p cypherlite-query-latency --features research
  --example sol`), which the example headers document.
- Every gate must stay green in both configurations: the default
  workspace clippy/fmt/tests and the `--features research` build (the
  gated code must not rot while disabled).
- `sol_graph()`'s read-guard bypass remains while quarantined: it is
  research-only, compiled only under the feature, and disappears with
  the quarantine when the LABS-455 SPI replaces it — the guard question
  is settled by the SPI's snapshot-scoped read contract, not by
  retrofitting a guard onto a research accessor.
- The `research` feature must never appear in a bundle feature list, in
  `default`, or in the WASM build; a future ADR (the LABS-455 SPI)
  supersedes this quarantine.

## Later references

[ADR-0478](./0478-frontier-nfa-qpp-kernel.md) resolves the `qpp` half of this quarantine: the prototype is
replaced by the production frontier kernel (compiled from the
`QuantifiedPathPattern` IR, running over the `ExecGraph` overlay), and
the research module is deleted outright — not reduced to parity tests.
Its oracle value is preserved two ways: the constructor-derived expected
shapes stay pinned as literals in the production kernel's suite
(`exec/kernels/nfa_tests.rs`, which never executed the prototype — its
`Pattern` fields were module-private, so the literals were hand-read),
and behavior is cross-checked by [ADR-0478](./0478-frontier-nfa-qpp-kernel.md)'s statement-level differential
suite plus the PathDB/LDBC QPP corpus (LABS-646). The `qpp` re-exports
(`graph/mod.rs`, both lib.rs files) and the `qpp` harness example in
`cypherlite-query-latency` are removed with it; the `research` feature
itself survives for the `sol` half, which stays quarantined until the
LABS-455 SPI.
