# ADR-0530: The playground moves to the relate-web repository

Status: Proposed

Claim: 8fc535ab5f90d424097242febb76cdd2bf8288ea

## Context

`examples/wasm-playground/` was the browser playground: a Vite + React app
that ran the WASM bundle in a worker, deployed at `/playground/` by
`.github/workflows/deploy-pages.yml`. ADR-0084 kept it as an example
consumer with no privileged status; `scripts/build-wasm.sh` mirrored each
bundle into its source tree (ADR-0100), and every Pages deploy installed
the Rust and wasm toolchain and built both the bundle and the app
(ADR-0330).

The website now lives in the relate-web repository, which already serves
the bundle to its home page console and owns the cookbook (ADR-0528). With
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
   (ADR-0051, ADR-0145), the `executeScript` and `renderResult` exports
   (ADR-0131), and `isWasmTrap` in `trap.js`.
5. ADR-0084, ADR-0100, ADR-0145, ADR-0278, and ADR-0330 stay as written;
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
  (ADR-0278).

## Consequences

- A WASM change no longer reaches the playground on the next deploy here;
  it reaches it when relate-web refreshes its copy, where its cookbook
  check and browser test run against it.
- The Pages deploy here needs no Rust, wasm, or Node toolchain. It still
  publishes `relatedb.ai`, now without `/playground/` or `/cookbook/`;
  which repository deploys the domain is a separate decision (ADR-0517
  §3).
- The `wasm` CI job and `make build-wasm` are unchanged.
