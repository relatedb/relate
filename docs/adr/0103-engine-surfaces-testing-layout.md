# 0103. Engine / surfaces / testing workspace layout and boundary rules

- **Status**: Accepted
- **Date**: 2026-09-25
- **Deciders**: CypherLite maintainers
- Later references: ADR-0434 adds the fourth top-level group `extensions/`
  for the first-party extension crates (the importer children and the wire
  kit) and with it amends R5 (members live directly under `cypherlite/`,
  `surfaces/`, `testing/` or `extensions/`), R3 (a surface's product-path
  reach into the group is the wire kit alone) and adds R7 (the children's
  own directions).

## Context

Linear LABS-103 ("Separate Cypherlite engine into a minimal core") asks for
the engine to stay small, lean, stable and fixed for the use cases that need
it, with a separate layer on top for surfaces, integrations and UX. The
components stay in the same repository, but with clearer separation rules, so
that use-case-specific bundles (for example an embedded-only engine build)
and a full bundle can be assembled from the same tree.

Today every workspace member lives flat under `crates/`
(`members = ["crates/*"]`). Nothing in the layout tells a contributor which
crates are the engine and which are adapters, CLIs, bindings or test
harnesses, and nothing stops an engine crate from picking up a dependency on
an output formatter, a network client, or a test harness. The boundary exists
only by convention.

The architecture proposal document for LABS-103 grouped the crates into an
"EasyGraph surface", a "CypherLite core engine" and "Test & eval". We keep
that three-way grouping but name the upper layer `surfaces/` instead of
introducing a new brand: externally everything is sold as CypherLite (PR
FAQ), and a second product name for the upper layer would compete with it.

The physical move of crates is done in a later change; this ADR records the
target layout and the rules that the move and all later changes must obey.

## Decision

Keep one Cargo workspace and one `Cargo.lock`, and place every workspace
member directly under exactly one of three top-level folders. Package names
do not change.

### Layout

- **`cypherlite/`** — the engine: `cypherlite`, `cypherlite-model`,
  `cypherlite-parser`, `cypherlite-error`, `cypherlite-core`,
  `cypherlite-catalog`, `cypherlite-ir`, `cypherlite-front`,
  `cypherlite-planner`, `cypherlite-executor`, `cypherlite-storage`,
  `cypherlite-splitter`.
- **`surfaces/`** — everything that adapts `cypherlite::Database` to a user,
  host or integration: `cypherlite-cli`, `cypherlite-bolt`,
  `cypherlite-python`, `cypherlite-ffi`, `cypherlite-wasm`,
  `cypherlite-formatter`, `cypherlite-load`, `cypherlite-arrow`.
- **`testing/`** — conformance, evaluation, datasets and
  tooling: `cypherlite-tck`, `cypherlite-cip-eval`, `cypherlite-datasets`,
  `cypherlite-test-support`, `xtask`.

Each crate sits at depth two (`<group>/<crate>/`), e.g.
`cypherlite/cypherlite-storage/` and `surfaces/cypherlite-cli/`.

`cypherlite-ffi` and `cypherlite-wasm` are surfaces: like
`cypherlite-python`, they adapt `cypherlite::Database` to a host (the C ABI
that the JS/Java/Go clients wrap, and the browser). They are not part of the
engine.

### Boundary rules

Rules are evaluated against the resolved workspace dependency graph
(`cargo metadata`), by dependency kind:

- **R1** — Normal and build dependencies of `cypherlite/*` may only target
  `cypherlite/*` packages.
- **R2** — Dev dependencies of `cypherlite/*` may target `cypherlite/*` or
  `testing/cypherlite-test-support`.
- **R3** — Normal and build dependencies of `surfaces/*` may target
  `cypherlite/*` or `surfaces/*`; dev dependencies of `surfaces/*` may
  additionally target `testing/cypherlite-test-support`.
- **R4** — `testing/*` may depend on any workspace package.
- **R5** — Every workspace member lives directly under one of `cypherlite/`,
  `surfaces/` or `testing/`.
- **R6** — Adding a new heavy external dependency (large transitive tree,
  native/C++ build, networking or async runtime, and the like) to any
  `cypherlite/*` crate requires its own ADR.

### Enforcement

R1–R5 are checked mechanically by `cargo xtask check-boundaries`, a small
`testing/xtask` binary that depends only on `std` and `serde_json`, reads
`cargo metadata --format-version 1`, classifies each workspace member by its
manifest path, and fails with a list of offending edges or misplaced members.
It runs in CI next to fmt and clippy. R6 is a review rule backed by
AGENTS.md §1 and is not machine-checked.

### Rationale

- The folders are organisation; the dependency graph is the real boundary.
  A single workspace and lockfile keep one resolver, one `cargo test`, one
  set of versions and no cross-workspace path juggling.
- Unchanged package names keep `-p` flags, CI jobs, published crate names and
  the `cypherlite_ffi` library name (consumed by the language clients)
  working.
- Depth two keeps existing `../../testdata` relative paths (TCK runner,
  CIP-eval, gherkin tests) resolving without edits.
- A dedicated engine folder with R1 gives use-case-specific bundles a clean
  starting point: an engine-only build pulls in nothing from surfaces or
  testing, and a full bundle simply adds surface crates on top.

References: LABS-103 and the LABS-103 architecture proposal document.

## Alternatives considered

- **Keep `crates/` and rely on naming conventions only.** Every crate is
  already named `cypherlite-*`, so no naming scheme can distinguish engine
  from surface without renaming packages, and a convention with no check
  erodes silently. Rejected because it gives neither a visible layout nor an
  enforced boundary.
- **Separate Cargo workspaces per group.** This would make the boundary
  physical, but it means multiple lockfiles that can drift, duplicated
  workspace metadata, cross-workspace path dependencies, and no single
  `cargo test`/`cargo clippy --workspace` gate. Rejected because the cost in
  tooling and version drift outweighs a boundary that a metadata check
  already enforces.
- **Enforce the rules with `cargo-deny` bans.** `cargo-deny` bans are aimed
  at external crates and express allow/deny per crate name, not "packages in
  folder A may only depend on packages in folder A" by dependency kind; it
  would also add a tool install to CI and local gates. Rejected because a
  ~100-line `std`+`serde_json` xtask expresses R1–R5 directly and adds no
  external tool.
- **Name the upper layer `easygraph/`.** This follows the proposal document,
  but it introduces a second brand in the tree while everything is sold as
  CypherLite. Rejected because a competing name confuses users and
  contributors about what the product is called.
- **Name the upper layer `adapters/`.** Accurate for ffi/wasm/python/bolt but
  a poor fit for the CLI, formatter and loader, which are user-facing
  experiences rather than adapters. Rejected because it describes only part
  of the group.
- **Name the upper layer `apps/`.** Fits the CLI and Bolt server binaries but
  not libraries such as `cypherlite-formatter`, `cypherlite-arrow` or the
  bindings. Rejected because most of the group are not applications.
- **Put `cypherlite-ffi` and `cypherlite-wasm` inside the engine group.**
  They are low-level and ship with the engine in many bundles, but they only
  adapt `cypherlite::Database` to a host ABI exactly as `cypherlite-python`
  does, and they carry host-specific dependencies (`wasm-bindgen`, C ABI
  glue) the engine should not need. Rejected because it would make the
  engine depend on host concerns and blur R1.

## Consequences

- A later change moves every crate from `crates/` into the three folders,
  updates `[workspace] members`/`default-members`, `[workspace.dependencies]`
  paths, CI workflows, Makefile targets, scripts, docs links and AGENTS.md's
  crate layout, and adds `testing/xtask` plus the CI step. Package names and
  published artifacts are unchanged.
- A client implementing embedded Relate that Docker-builds this repository
  consumes it as a named build context, and `.dockerignore` currently
  assumes all Rust sources live in `crates/*` (including the
  `crates/*/python` exclude). It must be updated in the move to include
  `cypherlite/`, `surfaces/` and `testing/` (or at least the members that
  client builds), and that client's build must be verified against the
  new paths.
- In-flight branches that touch files under `crates/` (for example
  `oskar/query-resource-governor`, which also reserves ADR-0100–0102) must be
  rebased across the move. Git rename detection handles most content edits,
  but new files added under `crates/`, `Cargo.toml` member/path edits and
  hard-coded `crates/...` paths in CI, scripts and docs will conflict or land
  in the wrong place and need manual fix-up. Landing the move when few
  branches are open, as a pure-move change, keeps this cost down.
- Engine crates can no longer grow dependencies on formatters, loaders,
  bindings or test harnesses; such needs must be met by moving code into a
  surface or into `cypherlite-test-support`. Heavy new external dependencies
  in the engine need an ADR (R6).
- A manifest review at the time of writing found no workspace edge that
  violates R1–R4, so the move is expected to need no code restructuring;
  `cargo xtask check-boundaries` is the authority once it lands.
- Use-case-specific bundles and the full bundle can be defined in terms of
  the groups: engine-only builds depend on `cypherlite/*` alone; surface
  bundles add the needed `surfaces/*` crates.
