# 0100. Size-optimized WASM build pipeline

- **Status**: Accepted
- **Date**: 2026-09-25
- **Deciders**: CypherLite maintainers

## Context

ADR-0084 reinstated the WASM distribution via `wasm-pack build --release
--target web`, wired through `scripts/build-wasm.sh` and gated in CI. That
pipeline used the default workspace `release` profile (no explicit size
tuning) and wasm-pack's bundled `wasm-opt` pass, producing a 7.07 MB raw
`cypherlite_bg.wasm` (2.05 MB gzip, 1.34 MB brotli).

The browser is the transport for this artifact; every page load pays the
transfer and decode cost. wasm-pack cannot select a custom cargo profile
(`--release`, `--dev`, and `--profiling` are the only options), so any
size-tuned profile is unreachable through it. The workspace release
profile also feeds native builds where we do not want `opt-level = "z"`
or full LTO.

## Decision

Replace the wasm-pack invocation in `scripts/build-wasm.sh` with an
explicit two-step pipeline that uses a wasm-only release profile:

1. `cargo build -p cypherlite-wasm --lib --profile release-wasm --target
   wasm32-unknown-unknown` — a new profile in the workspace `Cargo.toml`
   that inherits `release` and adds `opt-level = "z"`, `lto = "fat"`,
   `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`. It is
   only referenced by the wasm build entry, so native builds are
   unaffected.
2. `wasm-bindgen --target web` on the resulting `.wasm`, matched to the
   `wasm-bindgen` crate version in `Cargo.lock` (currently 0.2.128).
3. `wasm-opt -Oz` on the bindgen output.

`scripts/build-wasm.sh` remains the single entry point. It takes a
`--mode` flag:

- `--mode release` (default) runs the full size-tuned pipeline above.
  Used by CI, `make build-wasm`, the playground's `build:wasm:release`
  npm script, and the Pages deploy workflow — the workflow runs it
  explicitly before `npm run build`, so shipped playground bundles
  always contain the small wasm. `npm run build` itself does not run
  cargo, so local Vite-only iterations stay fast.
- `--mode dev` uses the workspace `release` profile and runs
  `wasm-opt -O2` instead of `-Oz`. Both the cargo step and the
  wasm-opt step are substantially faster (measured ~20 s end-to-end
  vs ~50 s for release on this machine). The output is roughly 1.5×
  the release size — smaller than the pre-ADR-0100 baseline because
  `-O2` still strips dead code. The playground's `build:wasm` script
  defaults to this mode for iterative work.

Required tools are `wasm-bindgen-cli` (matching the crate version) and
`binaryen` (for `wasm-opt`, used in both modes); the script checks both
and prints install hints if they are missing. The `wasm` job in
`.github/workflows/ci.yml` installs both. `wasm-pack test` still runs
for the browser smoke gate.

Measured impact on `cypherlite_bg.wasm` (this branch):

| Metric | Before | After | Δ    |
| ------ | -----: | ----: | ---: |
| Raw    | 7.07 MB | 4.26 MB | −40% |
| gzip   | 2.05 MB | 1.40 MB | −32% |
| brotli | 1.34 MB | 0.99 MB | −26% |

## Alternatives considered

- **Retune the workspace `[profile.release]` directly.** Rejected:
  `opt-level = "z"` + `lto = "fat"` + `codegen-units = 1` slows every
  native build (CLI, FFI, loadtests) with no size benefit for those
  surfaces, and would show up in baseline loadtest envelopes.
- **Stay on `wasm-pack` and set `[profile.release]` overrides for
  `cypherlite-wasm` only via `[profile.release.package.cypherlite-wasm]`.**
  Rejected: package-scoped overrides do not apply to dependencies, so the
  bulk of the code (`cypherlite-executor`, `cypherlite-front`, …) would
  keep the native profile, giving nearly none of the observed savings.
- **`wasm-pack --profiling` or `--dev`.** Rejected: they optimize away
  from release, not toward smaller size.
- **Pull in `wee_alloc` or drop `regex` / `chrono-tz` / `phf`.** Rejected
  for this change: it is a Rust source change, not a compilation-step
  change, and belongs in a follow-up ADR if the wire size still needs to
  drop.

## Consequences

- The wasm build now requires `wasm-bindgen-cli` and `binaryen` on the
  host. CI installs them; local devs get an actionable error from
  `scripts/build-wasm.sh` if either is missing.
- The `release-wasm` profile is a public workspace profile. Any crate
  can be built with it, but it is only intended for the wasm entry;
  documenting that here is the contract.
- Full LTO + `codegen-units = 1` makes the wasm build slower (measured
  ~35 s on this machine vs ~15 s for the old default profile). CI wasm
  job time increases by roughly that amount.
- ADR-0084's build-entry clause still holds — one shared entry point,
  called by both `make build-wasm` and the playground.

## Later references

- ADR-0530 moves the playground to the relate-web repository: its
  `build:wasm` scripts and the Pages deploy's bundle build are removed.
  The two modes and the single entry point are unchanged.
