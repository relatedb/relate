# 0278. npm distribution

- **Status**: Accepted
- **Renumbered**: from 0154 (shared-number cleanup)
- **Date**: 2026-10-01
- **Deciders**: CypherLite maintainers (Linear LABS-19, parent LABS-64)

## Context

LABS-19 (parent LABS-64, GS London v0.1 release) asks for a seamless
`npm install` story for the JavaScript surfaces. Today neither surface is
installable from npm:

- The Node client (`clients/javascript`, koffi over `libcypherlite_ffi`)
  resolves the native library only from the `CYPHERLITE_LIB` environment
  variable or the repo `target/` directory, so consuming it outside a
  checked-out, locally built repo is not possible.
- The wasm bundle (`surfaces/cypherlite-wasm`, built by
  `scripts/build-wasm.sh` with `wasm-bindgen --target web`) has no
  `package.json` at all; only the in-tree mirror the playground consumes
  exists.

The unscoped name `cypherlite` on npm is taken by an unrelated project
(v2.1.0) — the same situation as PyPI (ADR-0054). The scoped names
`@neo4j-labs/cypherlite` and `@neo4j-labs/cypherlite-wasm` are verified
available.

`release.yml` already builds FFI artifacts for five targets (linux
x64/arm64, darwin x64/arm64, windows x64) and creates the release tags, and
trusted publishing to PyPI is the established distribution pattern for
this repo (`docs/PUBLISHING.md`).

## Decision

Publish two scoped npm packages:

- **`@neo4j-labs/cypherlite`** — the Node embedded client (koffi over the
  C ABI).
- **`@neo4j-labs/cypherlite-wasm`** — the browser wasm bundle.

The embedded client ships no native code itself. Five platform packages
carry the prebuilt library, one per release target:

- `@neo4j-labs/cypherlite-linux-x64`
- `@neo4j-labs/cypherlite-linux-arm64`
- `@neo4j-labs/cypherlite-darwin-x64`
- `@neo4j-labs/cypherlite-darwin-arm64`
- `@neo4j-labs/cypherlite-win32-x64`

The platform packages are wired as `optionalDependencies` of
`@neo4j-labs/cypherlite` with npm `os`/`cpu` fields — the esbuild/swc
pattern — so npm installs exactly the one matching package and skips the
rest, with no install scripts anywhere in the graph.

The loader resolves the native library in order: `CYPHERLITE_LIB` first,
then the platform package, then the repo `target/` directories for
development.

All seven package versions are lockstep with the release tag via
`scripts/release-bump.sh`. Publishing happens in `release.yml` after the
tag job using npm trusted publishing (OIDC, `id-token: write`, no
`NODE_AUTH_TOKEN`). The human prerequisites — creating the `@neo4j-labs`
npm org, the first publish, and configuring trusted publishing on
npmjs.com — are runbook items in `docs/PUBLISHING.md`, not automation.

## Alternatives considered

- **One combined package shipping both the native libs and the wasm
  bundle.** Rejected: it forces the ~4.3 MB wasm blob onto Node users and
  the platform dylibs onto browser users; the two surfaces have disjoint
  runtimes and disjoint audiences.
- **Postinstall download of the FFI tarball from GitHub Releases.**
  Rejected: it breaks under `--ignore-scripts` and offline mirrors, needs
  network access at install time, and is not seamless — exactly the
  failure modes the `optionalDependencies` pattern avoids.
- **`NODE_AUTH_TOKEN` secret in CI.** Rejected: a long-lived revocable
  secret in CI when npm trusted publishing (OIDC) is available and matches
  the repo's PyPI plan (`docs/PUBLISHING.md`).
- **napi-rs rewrite of the JS client.** Rejected: the koffi C-ABI client
  exists and works; a rewrite is out of scope for the install story.
- **Independent per-package versioning.** Rejected: version drift between
  client and engine is a support burden; lockstep with the release tag
  matches `release-bump.sh`'s existing sweep.

## Consequences

- `npm install @neo4j-labs/cypherlite` works with no Rust toolchain and no
  install scripts on the five release targets; other platforms keep
  working via `CYPHERLITE_LIB`.
- Seven packages publish per release, all fail-closed until the human
  npmjs.com setup in `docs/PUBLISHING.md` is done.
- The playground keeps consuming the in-tree wasm mirror, not the npm
  package.
- The loadtest CLIs (`bin/`) stay repo-internal and are excluded from the
  published tarball.

## Later references

- ADR-0530 moves the playground to the relate-web repository, which
  serves a copy of `surfaces/relate-wasm/pkg/`; no in-tree mirror remains.
