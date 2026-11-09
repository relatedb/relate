# 0084. Reinstate the WASM distribution

- **Status**: Accepted
- **Date**: 2026-09-24
- **Deciders**: CypherLite maintainers

## Context

ADR-0065 withdrew the WASM surface because it had no CI gate after merge
commit 134667e. Its reinstatement clause is explicit: WASM can return
"under a new ADR that commits to a CI gate from day one — a wasm32 build
plus browser smoke". This ADR is that new ADR.

The product goal is to distribute a browser-embeddable
`cypherlite::Database` opened at `:memory:` as a `wasm-pack`-generated
package (`--target web`), so a page can import CypherLite and run Cypher
without a server. The memory-only contract from ADR-0051 still applies.
`examples/wasm-playground/` is one consumer of that package; it is not the product.

## Decision

Reinstate `crates/cypherlite-wasm` as a supported distribution surface,
gated in `.github/workflows/ci.yml`:

1. **Package.** `crates/cypherlite-wasm` builds with
   `wasm-pack build --target web`. The output package is the distributed
   artifact.
2. **CI gate.** A `wasm` job runs
   `cargo check -p cypherlite-storage --target wasm32-unknown-unknown`,
   `cargo check -p cypherlite-wasm --target wasm32-unknown-unknown`, and
   `wasm-pack build --release --target web`. Failure blocks the merge.
3. **Build entry.** The shared entry is `scripts/build-wasm.sh` (a thin
   `wasm-pack` invocation) and `make build-wasm`. `examples/wasm-playground/`
   and CI both call the same entry.
4. **Examples.** `examples/wasm-playground/` is an example consumer. It has no
   privileged status and no separate CI gate beyond the `wasm` job
   producing a package it can `import`.
5. **Docs.** The public surface has `docs/api/wasm/{spec.md,description.md,examples/}`
   per AGENTS.md §2. The wasm32 build split it depends on is recorded in
   ADR-0085.

## Alternatives considered

- **Leave the surface out of the tree** — rejected: distribution is the
  product goal and the branch already has a working package plus an
  example consumer. ADR-0065's objection was the missing gate; this ADR
  adds it.
- **Skip the shared build entry; call `wasm-pack` from the npm script
  only** — rejected: CI and repo-root builds would need to duplicate the
  incantation. One entry point per build step is the repo norm.

## Consequences

- The supported-surface invariant from ADR-0065 holds: the crate exists
  in-tree only because CI builds it. Deleting the `wasm` job supersedes
  this ADR.
- ADR-0051's memory-only browser contract is restated in force. Any
  future file- or IndexedDB-backed browser surface needs its own ADR.
- Publishing the package to npm, and any release-artifact packaging, are
  separate decisions for a follow-up ADR.

## Later references

- ADR-0100 replaces the `wasm-pack build --release` command inside
  `scripts/build-wasm.sh` with a `cargo build --profile release-wasm` +
  `wasm-bindgen` + `wasm-opt -Oz` pipeline. The build-entry, CI-gate,
  and shared-entry clauses of this ADR still hold.
- ADR-0145 supersedes the memory-only clause for one worker path:
  `CypherliteDb.open("opfs://<name>")`. The default constructor is still
  `:memory:`.
- ADR-0530 moves the playground to the relate-web repository:
  `examples/wasm-playground/` is removed, so the examples clause has no
  in-tree app and `scripts/build-wasm.sh` has no playground caller. The
  shared build entry is unchanged.
