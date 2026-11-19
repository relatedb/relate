---
type: adr
area: ecosystem
claim: 8fc535ab5f90d424097242febb76cdd2bf8288ea
name: 0530-playground-moves-to-relate-web
status: proposed
tags:
  - docs
title: The playground moves to the relate-web repository
REFERENCES:
  - "[[adr/0051-wasm-memory-database]]"
  - "[[adr/0084-reinstate-wasm-distribution]]"
  - "[[adr/0100-wasm-size-optimized-build]]"
  - "[[adr/0131-wasm-script-execution]]"
  - "[[adr/0145-wasm-opfs-vfs]]"
  - "[[adr/0278-npm-distribution]]"
  - "[[adr/0330-release-chains-pages-deploy]]"
  - "[[adr/0517-rename-to-relate]]"
  - "[[adr/0528-cookbook-moves-to-relate-web]]"
---

# ADR-0530: The playground moves to the relate-web repository

## Context

`examples/wasm-playground/` was the browser playground: a Vite + React app
that ran the WASM bundle in a worker, deployed at `/playground/` by
`.github/workflows/deploy-pages.yml`. [ADR-0084](./0084-reinstate-wasm-distribution.md) kept it as an example
consumer with no privileged status; `scripts/build-wasm.sh` mirrored each
bundle into its source tree ([ADR-0100](./0100-wasm-size-optimized-build.md)), and every Pages deploy installed
the Rust and wasm toolchain and built both the bundle and the app
([ADR-0330](./0330-release-chains-pages-deploy.md)).

The website now lives in the relate-web repository, which already serves
the bundle to its home page console and owns the cookbook ([ADR-0528](./0528-cookbook-moves-to-relate-web.md)). With
the playground here, the site's engine came from two places, the
cookbook's hand-off into the playground crossed repositories, and the
deploy here rebuilt the engine for a single page.

## Decision

1. The relate-web repository owns the playground: its source, its build,
   and `/playground/`. This repository removes `examples/wasm-playground/`
   and keeps no in-tree browser app.
2. relate-web serves one copy of the bundle, `surfaces/relate-wasm/pkg/`
   built by `scripts/build-wasm.sh --mode release`, to its console, its
   cookbook, and the playground. It refreshes that copy by hand from a
   checkout of this repository.
3. `scripts/build-wasm.sh` stops mirroring the bundle into the
   playground's source tree. `deploy-pages.yml` no longer builds the
   bundle or the playground and publishes the `gh-pages` tree as it is.
4. The engine contract stays here and stays stable: the `RelateDb` class
 ([ADR-0051](./0051-wasm-memory-database.md), [ADR-0145](./0145-wasm-opfs-vfs.md)), the `executeScript` and `renderResult` exports
 ([ADR-0131](./0131-wasm-script-execution.md)), and `isWasmTrap` in `trap.js`.
5. [ADR-0084](./0084-reinstate-wasm-distribution.md), [ADR-0100](./0100-wasm-size-optimized-build.md), [ADR-0145](./0145-wasm-opfs-vfs.md), [ADR-0278](./0278-npm-distribution.md), and [ADR-0330](./0330-release-chains-pages-deploy.md) stay as written;
   each gains a Later references note pointing to this ADR.

## Alternatives considered

- **Keep the playground here and have relate-web link to it.** The site
  would still deploy from two repositories, and the cookbook hand-off
  would cross them. Rejected.
- **Keep a minimal example app here.** `docs/api/wasm/examples/` already
  holds the runnable examples AGENTS.md requires; a second app would
  need its own toolchain and would not be tested. Rejected.
- **Have relate-web build the bundle from this repository in CI.** Couples
  the two repositories' CI and needs the Rust toolchain there. Rejected
  until relate-web consumes the published `@relate/relate-wasm` package
 ([ADR-0278](./0278-npm-distribution.md)).

## Consequences

- A WASM change no longer reaches the playground on the next deploy here;
  it reaches it when relate-web refreshes its copy, where its cookbook
  check and browser test run against it.
- The Pages deploy here needs no Rust, wasm, or Node toolchain. It still
  publishes `relatedb.ai`, now without `/playground/` or `/cookbook/`;
 which repository deploys the domain is a separate decision ([ADR-0517](./0517-rename-to-relate.md)
  §3).
- The `wasm` CI job and `make build-wasm` are unchanged.
