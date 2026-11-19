---
type: adr
area: surface
date: 2026-09-19
deciders: CypherLite / MemoryDB merge
name: 0065-withdraw-wasm-swift-surfaces
status: accepted
tags:
  - clients
  - wasm
title: Withdraw the WASM and Swift surfaces
REFERENCES:
  - "[[adr/0051-wasm-memory-database]]"
  - "[[adr/0052-graphstore-withdrawn]]"
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0084-reinstate-wasm-distribution]]"
  - "[[adr/0433-reinstate-swift-surface]]"
SUPERSEDES:
  - "[[adr/0051-wasm-memory-database]]"
---

# 0065. Withdraw the WASM and Swift surfaces

## Context

[ADR-0052](./0052-graphstore-withdrawn.md) established the withdrawal pattern for this repo: a surface that is
no longer a supported product path is removed from the tree, not left to rot
in-tree as dead code. This ADR applies that pattern to the two surfaces left
over from the CypherLite/MemoryDB merge.

After the merge, the supported product surfaces are `cypherlite::Database`
(both the Rust crate and the C ABI), the language clients that wrap the C ABI
(JS/Java/Go; [ADR-0057](./0057-embedded-c-abi-and-bolt.md)), the CLI, and Bolt as remote transport
([ADR-0057](./0057-embedded-c-abi-and-bolt.md)). Two former surfaces remain in the tree but are no longer on that
list:

- **WASM**: `crates/cypherlite-wasm` (the `Database` `:memory:` browser crate
 of [ADR-0051](./0051-wasm-memory-database.md)), the Docusaurus playground and homepage live terminal
  (`docs/src/pages/playground.tsx`, `docs/src/components/CypherTerminal.tsx`,
  `docs/src/lib/cypherlite-wasm.ts`, `docs/src/lib/terminalSession.ts`), the
  checked-in wasm-pack bundle under `docs/static/wasm/`, the wasm docs
  (`docs/docs/guides/wasm-embedding.md`,
  `docs/docs/reference/wasm-persistence.md`), and the Makefile
  `build-wasm` / `test-wasm-browser` targets.
- **Swift**: the repository-root `Package.swift`, `bindings/swift`,
  `examples/ios-snotel`, the XCFramework / Swift release scripts
  (`scripts/build-xcframework.sh`, `scripts/prepare-swift-release.sh`,
  `scripts/check-swift-*.sh`,
  `scripts/create-deterministic-xcframework-archive.sh`,
  `scripts/canonicalize-xcframework-info-plist.py`) and their script-test
  suites in `scripts/tests/`, the Makefile `build-xcframework` / `build-swift`
  / `test-swift` / `build-ios-example` / `test-ios-example` targets, and the
  Swift/iOS docs (`docs/docs/guides/swift-ios.md`, `bindings/swift/README.md`;
  `docs/PUBLISHING.md` survives, rewritten).

The merge commit 134667e ("drop WASM-target/WASM-browser/Swift CI jobs and the
wasm/ffi-apple release artifacts") already removed the gates: the `wasm`,
`wasm-browser-test`, and `swift` jobs from `.github/workflows/ci.yml`, and the
`wasm` (wasm-pack tarball) and `ffi-apple` (Apple XCFramework) jobs from
`.github/workflows/release.yml`. It deliberately left the scaffolding in-tree
as "local tooling". Nothing in the merge gate now builds the wasm32 target,
runs the headless-Chrome smoke, builds an XCFramework, runs `swift test`, or
builds the iOS example. The only remaining CI touch is the native
`cargo test -p cypherlite-wasm` entry in the core `test` job, which exercises
the crate off-target.

That state is the problem: the repo advertises surfaces by what it builds and
ships ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)'s "do not add a client language until the C ABI for that
feature is documented" is the same discipline), and a surface with no gate
drifts silently. Leaving it in-tree implies support the merge gate does not
provide.

## Decision

Withdraw both surfaces from the repository.

**WASM is withdrawn.** `crates/cypherlite-wasm`, the docs playground and
homepage live terminal, the checked-in `docs/static/wasm/` bundle, the wasm
docs, the Makefile wasm targets, and the native `cargo test -p
cypherlite-wasm` entry are deleted. No release artifact and no CI job builds
the wasm32 target after this change.

**Swift is withdrawn.** `Package.swift`, `bindings/swift`, `examples/ios-snotel`,
the XCFramework/Swift scripts and their script tests, the Makefile
XCFramework/Swift/iOS targets, and the Swift/iOS docs are deleted. No release
artifact and no CI job builds an XCFramework or runs `swift test` after this
change.

**`cypherlite-ffi` remains.** The C ABI is unchanged and stays the product
multi-language boundary for the JS/Java/Go clients, per [ADR-0057](./0057-embedded-c-abi-and-bolt.md).
Withdrawing the Swift *packaging* does not touch the C ABI itself.

This follows the withdrawal pattern of [ADR-0052](./0052-graphstore-withdrawn.md): the surface is removed, not
disabled; reviving it requires a new ADR.

## Alternatives considered

- **Keep both as untested local tooling (the status quo after merge commit
  134667e)** — rejected: CI no longer exercises either surface, so the crates,
  manifests, scripts, and docs drift against the engine with no signal; that is
 exactly the rot [ADR-0052](./0052-graphstore-withdrawn.md) rejected for GraphStore, and "works locally" is not a
  supported surface.
- **Remove Swift only and keep the WASM playground** — rejected: the playground
  has no CI gate either (the wasm-pack build and headless-Chrome smoke were
  dropped in 134667e), so it rots the same way; splitting the two surfaces also
  leaves a docs playground that can silently disagree with the engine, which is
  worse than no playground.
- **Keep the WASM crate but drop only the docs playground/bundle** — rejected:
  the crate is the thing the wasm32 target and browser smoke tested; keeping the
  crate while deleting its only consumer and its gates preserves the rot without
  the product, so it is strictly worse than full withdrawal.

## Consequences

- The supported-surface invariant becomes true again: a surface exists in-tree
 only if CI builds it and release ships it. `cypherlite-ffi` ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)) is the
  sole multi-language boundary; the JS/Java/Go clients are unaffected.
- The docs site loses the interactive playground and homepage live terminal.
  Users who want an embedded browser database have no supported path until a
  future ADR restores one.
- CI and release stay exactly as 134667e set them; the working tree loses the
  scaffolding that commit deliberately left behind, and the `cargo test -p
  cypherlite-wasm` entry, `build-wasm` / `test-wasm-browser`, and the
  XCFramework/Swift Makefile targets are removed with their code.
- **Reinstatement path**: WASM or Swift can return under a new ADR that commits
  to a CI gate from day one — a wasm32 build plus browser smoke, or a Swift
 package built against `cypherlite-ffi`. [ADR-0051](./0051-wasm-memory-database.md)'s memory-only WASM contract
  would have to be restated then; it governs a crate that no longer exists.
- This supersedes the WASM-scaffolding half of [ADR-0051](./0051-wasm-memory-database.md)'s consequences (its
  decision contract is not contradicted, only its in-tree presence) — see its
  *Later references*.

## Later references

- [ADR-0084](./0084-reinstate-wasm-distribution.md) reinstates the WASM surface
  as a distributed `wasm-pack` package (`cypherlite-wasm` crate plus a `wasm`
  CI job) under the reinstatement clause above. `examples/wasm-playground/`
  is one example consumer. The Swift withdrawal in this ADR stands.
- [ADR-0433](./0433-reinstate-swift-surface.md) reinstates the Swift surface
  (a SwiftPM package over the tabular C ABI, a static XCFramework, and a
  macOS `swift` CI job) under the reinstatement clause above.
