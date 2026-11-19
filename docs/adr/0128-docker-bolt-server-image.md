---
type: adr
area: infra
date: 2026-09-26
deciders: CypherLite maintainers (agreed in conversation)
name: 0128-docker-bolt-server-image
status: accepted
tags:
  - release
title: Docker image for the cypherlite-bolt server
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0109-bolt-tls]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0248-serve-host-flag]]"
  - "[[adr/0249-docker-unified-serve-image]]"
---

# 0128. Docker image for the cypherlite-bolt server

**Later references**: [ADR-0249](./0249-docker-unified-serve-image.md) retargets the image's runtime from the
bolt-only `cypherlite-bolt` to the unified `cypherlite serve` (Bolt +
HTTP + MCP, ports 7687/7474/7475, one credential); the scratch/chef/
UID 1000/`/data` volume/PID-1-signal decisions here stand unchanged.
[ADR-0248](./0248-serve-host-flag.md) adds the `serve --host` flag the container binds with.

## Context

CypherLite is a single binary; the remote surface is `cypherlite-bolt`
([ADR-0057](./0057-embedded-c-abi-and-bolt.md): Bolt as transport). Containers are the natural unit for running
the server next to agent sandboxes and CI, and the agentic cookbook work
kept re-deriving "run a server with some seeded data" by hand. The
`.dockerignore` already exists (an external project builds this repo as a
named cargo context), so the image build stays lean by the same rules.

## Decision

Ship a multi-stage `Dockerfile` + a Rust `entrypoint` binary + a
`make docker` target:

- **Build stage**: `rust:1.88-alpine` (the MSRV toolchain on musl), release
  builds of `cypherlite` (full bundle), `cypherlite-bolt`, and the
  `entrypoint` binary. The `.dockerignore` keeps the context to what cargo
  needs. Build speed comes from two pinned tools: **cargo-chef** splits
  dependency compilation into its own layer (`cook` from `recipe.json`,
  package-scoped to the two shipped crates so testing natives stay out) —
  that layer is a cache hit until Cargo.toml/lock change, and layer
  round-trips are the cache mechanism that actually works with the CI's
  `type=local` exporter (cache mounts do not export — measured); and
  **sccache** (version + sha256 pinned) wraps rustc for the workspace
  crates, since cargo's fingerprints are mtime-based and `COPY` refreshes
  every mtime (measured: a restored target cache rebuilt everything).
  Measured locally: full build ~90 s; a source-only change ~13 s. Nothing
  from a cache mount lands in the image — the runtime stage copies the
  binaries out of `/out`. The build runs natively on `rust:1.88-alpine` —
  its host triple
  *is* `x86_64-unknown-linux-musl`, so the binaries are static by default
  with no cross-compiler.
- **Runtime stage**: `FROM scratch` — musl-static binaries only (the
  `cypherlite` full bundle, `cypherlite-bolt`, and a tiny Rust `entrypoint`
  binary, since scratch has no shell), plus the CA bundle at the standard
  path for ureq/rustls. Numeric `USER 1000:1000` (scratch has no
  `/etc/passwd`); the data volume must be writable by that UID.
- **Ports**: `EXPOSE 7687` (Bolt) and `7474` (reserved for the future
  HTTP/Query surface; the server is Bolt-only today).
- **Config resolution, most to least canonical**:
  (1) container args pass straight through to `cypherlite-bolt` (a
  `CMD` default of `--bind 0.0.0.0:7687 --uri /data/graph
  --allow-plaintext`); (2) a mounted
  `/config/seed.json` (path override: `CYPHERLITE_CONFIG`) drives seeding —
  ordered steps (`query`, `load`+`query`, `pull`+`query`), JSON not YAML
  because `serde_json` is in-tree and `serde_yaml` is archived; (3) env
  fallback for one-liners (`CYPHERLITE_QUERY`, `CYPHERLITE_LOAD` +
  `CYPHERLITE_LOAD_QUERY` (the `--load` flag, still on its own unmerged
 PR), `CYPHERLITE_PULL_FROM` + `CYPHERLITE_PULL_QUERY` ([ADR-0080](./0080-cli-pull-command.md)),
  `CYPHERLITE_FORCE_SEED`). The
  engine's native env vars (`CYPHERLITE_MEMORY_LIMIT`,
  `CYPHERLITE_DURABILITY_MODE`, `CYPHERLITE_CHECKPOINT_INTERVAL_SECS`,
  `CYPHERLITE_LOAD_CSV_DIR`) pass straight through untouched.
- **Seeding** runs through the plain CLI before the server execs: once when
  the database URI does not exist yet (always for `:memory:`, or with
  `CYPHERLITE_FORCE_SEED=1`), never while the server is live.
- The server is exec'd last, so it is PID 1 and owns the container
  lifecycle (signals, exit codes) — including the SIGINT/SIGTERM handlers
  that make `Ctrl-C` and `docker stop` effective against PID 1.

## Alternatives considered

- **Distroless/debian-slim runtime** — rejected: musl-static binaries make
  a true `FROM scratch` runtime possible, and the seed logic is a tiny Rust
  entrypoint binary instead of a shell script. Smallest surface, nothing to
  patch. (debian-slim was the first draft; scratch replaced it.)
- **Cross-compiling for musl** — rejected after trying both routes:
  Debian's `musl-gcc` wrapper rejects the `-m64` that `ring`'s `cc-rs`
  passes, and `zig cc` trips cc-rs's compiler-family detection (it invokes
  `zig -E` without a target). A musl-native Alpine builder needs neither.
- **YAML seed config** — rejected for the seed file: `serde_yaml` is
  archived/unmaintained; JSON covers the same shape with `serde_json`
  already in the dependency tree, and agents write JSON reliably.
- **A dedicated seed/sidecar container** — rejected: seed-once-then-serve in
  one entrypoint keeps the single-container story agents expect; a sidecar
  is still possible by running the same image with `cypherlite` as the
  command.
- **Seeding through a Bolt client at startup** — rejected: the CLI already
  seeds directly against the database URI with no protocol round-trip.

## Consequences

- `make docker` / `make docker-run` give a local server with a volume and
  seeded data in one command.
- The image depends on the CLI's `--load` flag for `CYPHERLITE_LOAD`
  seeding (still on its own unmerged PR); the entrypoint fails loudly with
  a clear message on a build without it. `pull`- and plain-query seeding
  work on any current build.
- `INSTANCES=N` spins up N servers in one container — a test fleet: ports
  step from the base `--bind` port (`docker run -p 7687-7688:7687-7688
  -e INSTANCES=2`), persistent URIs are suffixed per instance, each instance
  seeds its own URI. The supervisor is deliberately minimal — spawn,
  monitor, per-instance exit logging — no restarts. PID 1 ignores unhandled
  signals by kernel design, so both the exec'd server (single instance) and
  the fleet entrypoint install explicit SIGINT/SIGTERM handlers: the server
  exits, the fleet entrypoint kills every child then exits. Ctrl-C on an
  attached `docker run` and `docker stop` therefore work without SIGKILL;
  the fsync'd WAL makes any exit data-safe either way.
- The `--load` flag merge and this image are independent; both keep
  working alone.
- The default `CMD` carries `--allow-plaintext`: a container must bind
 `0.0.0.0` to be reachable at all, and the plaintext policy ([ADR-0109](./0109-bolt-tls.md))
  refuses a non-loopback plaintext bind otherwise. This is a dev/test
 image posture — auth ([ADR-0126](./0126-bolt-basic-auth.md)) is still on by default — and TLS is
  available by mounting cert/key and passing `--tls-cert`/`--tls-key`
  (scratch carries no openssl to generate them).
