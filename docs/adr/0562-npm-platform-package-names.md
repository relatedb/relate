---
type: adr
area: infra
claim: 6fcd03d06e4e58b59dc3c39138e8b28c6aacc1e0
date: 2026-11-10
deciders: the maintainers
name: 0562-npm-platform-package-names
status: proposed
tags:
  - release
title: npm platform package names carry the relate- prefix
REFERENCES:
  - "[[adr/0278-npm-distribution]]"
  - "[[adr/0517-rename-to-relate]]"
  - "[[adr/0560-crates-io-and-pypi-distribution]]"
  - "[[adr/0616-npm-platform-file-links-repo-pins-at-pack]]"
SUPERSEDES:
  - "[[adr/0278-npm-distribution]]"
---

# 0562. npm platform package names carry the relate- prefix

## Context

[ADR-0278](./0278-npm-distribution.md) (renamed in scope by [ADR-0517](./0517-rename-to-relate.md)) shaped the npm distribution as a
pure-JS main package plus five platform packages carrying the prebuilt
native library, wired as `optionalDependencies`. The platform packages
were named `@relate/<os>-<arch>` — `@relate/darwin-arm64` and so on —
while the code packages are `@relate/relate` and `@relate/relate-wasm`.

Nothing has been published yet ([ADR-0560](./0560-crates-io-and-pypi-distribution.md)'s bootstrap is pending), so the
names can still change for free. On the registry page the bare
`@relate/darwin-arm64` form reads as a top-level product next to
`@relate/relate`, and the scope accumulates one visually unrelated entry
per platform forever.

## Decision

Name the five platform packages `@relate/relate-<os>-<arch>`:
`@relate/relate-darwin-arm64`, `@relate/relate-darwin-x64`,
`@relate/relate-linux-arm64`, `@relate/relate-linux-x64`,
`@relate/relate-win32-x64`. The main package (`@relate/relate`) and the
wasm bundle (`@relate/relate-wasm`) keep their names. The Node loader
derives the platform package from the platform key as
`@relate/relate-${key}`; everything else (pack jobs, smoke gate, publish
workflow) follows the package manifests.

## Alternatives considered

- **Keep `@relate/<os>-<arch>`.** Rejected: five short, product-looking
  names squat the scope's top level and read as siblings of the main
  package rather than its implementation detail.
- **Nest under a sub-scope (`@relate/relate.platforms/...`).** Rejected:
  npm has no sub-scopes; only the flat prefix is available.

## Consequences

- The scope reads as three products — `relate`, `relate-wasm`, and the
  `relate-*` platform set — instead of seven unrelated names.
- No migration: decided before the first publish; [ADR-0278](./0278-npm-distribution.md)'s package
  list is superseded in this one aspect (see its Later references).
- Tarball names become `relate-relate-<os>-<arch>-<version>.tgz`; the
  smoke gate and the publish workflow's main-vs-platform selection key
  off the version segment, which is unchanged.

## Later references

- [ADR-0616](./0616-npm-platform-file-links-repo-pins-at-pack.md) changes how the five names resolve: `file:` links in the
  repository manifest, registry pins injected into the packed manifest
  at the release version. The names themselves are unchanged.
