# 0104. Cargo feature bundles, starting with the CLI

- **Status**: Accepted
- **Date**: 2026-09-25
- **Deciders**: CypherLite maintainers

## Context

Linear LABS-103 asks to separate CypherLite into a minimal engine core with a
layer above it for surfaces, integrations, and UX, and to offer
"use-case-specific bundles that cherry-pick features, plus a larger bundle
containing all features". ADR-0103 does the structural half: it moves the
engine crates under `cypherlite/`, the product surfaces under `surfaces/`, and
test/benchmark tooling under `testing/`. The `cypherlite` CLI crate
(`cypherlite-cli`) lands at `surfaces/cypherlite-cli`. ADR-0103 does not decide
how a single crate offers less or more capability; that is this ADR.

Today the CLI is monolithic. Every build links:

- `repl.rs` — the `rustyline` line editor for the interactive shell
  (ADR-0073). It already has a `run_plain_interactive` plain-stdin fallback
  used when the editor cannot start.
- `pull_cmd.rs` — `cypherlite pull` (ADR-0080), which depends on
  `cypherlite-load` (Neo4j/Aura Query API v2 client).
- `skill/` + `skill_cmd.rs` — `cypherlite skill install|remove`, which writes
  the embedded agent skill bundle into agent config directories.
- `upgrade_cmd.rs` — `cypherlite upgrade` / `update`, which downloads and
  verifies release binaries using `ureq` and `sha2`.
- `telemetry/` + `telemetry_cmd.rs` — `cypherlite telemetry on|off|status` and
  anonymous usage-metrics emission (ADR-0079), using `ureq` (and `rand` for
  event identifiers).

The first concrete use case for a smaller build is sandbox and Docker images
for AI agents. Those run one-shot or stdin queries with JSON output. They must
not self-update (the image is the version) and should make no telemetry
network calls, and they gain nothing from an interactive line editor.
Operators currently get all of that whether they want it or not, plus the HTTP
and TLS dependency surface that comes with it.

ADR-0069 fixes the CLI command grammar: exit-after lifecycle verbs (`skill`,
`upgrade`/`update`, `telemetry`, `pull`) are positional commands recognized
only as the first argument, and a database path that collides with a command
word stays reachable through `./` or `--`. Any way of removing commands must
keep that grammar stable, or the same argv would mean "run a command" in one
build and "open a database" in another.

## Decision

Offer compile-time capability as **Cargo feature bundles**: named feature sets
on the crate that owns the capability. The CLI is the first crate to adopt
this.

### Rules for every feature and bundle

1. A bundle is a named Cargo feature that only enables other features. Every
   feature only **adds** capability. There are no negative (`no-*`) features,
   so Cargo's feature unification can never remove something another
   dependent relies on.
2. `full` is the `default` feature. A plain `cargo build` or `cargo install`
   produces exactly today's binary.
3. Release artifacts (GitHub releases, install script, Homebrew) stay
   `full`-only for now. Smaller bundles are built from source
   (`cargo build -p cypherlite-cli --no-default-features --features agent`).
   Publishing other bundles as artifacts needs its own ADR.
4. A command word whose feature is compiled out is **still recognized as a
   command**. As the first argument (not after `--`, not `./`-prefixed), it
   prints `Error: the '<command>' command is not included in this build of
   cypherlite` to stderr and exits with code **2**. The argv grammar from
   ADR-0069 is therefore identical in every bundle: `cypherlite pull` never
   silently opens a database named `pull`, and `./pull` or `-- pull` is the
   escape in every build.
5. The hand-written `--help` text lists only the commands compiled into the
   binary. `docs/api/cli/spec.md` documents every command with the feature
   that provides it, plus the bundle table below, so a given build's
   `--help` matches the spec for that feature set.
6. CI builds, runs clippy on, and tests every named bundle (`full`, `agent`,
   `minimal`), not only the default. Tests for a gated command are gated on
   the same feature. The compiled-out path is tested in the bundles
   that exclude the command.
7. Later engine-level features (for example Parquet checkpoints or a `.cl`
   bundle VFS) follow the same rules. Each gets its own ADR that names the
   feature, what it gates, and which bundles include it.

### CLI features

| Feature | Gates | Optional dependencies | Without it |
| --- | --- | --- | --- |
| `repl` | `rustyline` line editor in `repl.rs` (history, editing, multi-line prompt) | `rustyline` | Interactive mode uses the existing `run_plain_interactive` plain-stdin loop; one-shot and piped stdin are unaffected |
| `pull` | `cypherlite pull` command (`pull_cmd.rs`) | `cypherlite-load` | `pull` is a recognized, not-included command (exit 2) |
| `skill` | `cypherlite skill install\|remove` (`skill/`, `skill_cmd.rs`, embedded skill bundle) | none | `skill` is a recognized, not-included command (exit 2) |
| `upgrade` | `cypherlite upgrade` and its `update` alias (`upgrade_cmd.rs`) | `ureq`, `sha2` | `upgrade`/`update` are recognized, not-included commands (exit 2) |
| `telemetry` | `cypherlite telemetry on\|off\|status` (`telemetry_cmd.rs`) and all usage-metrics emission (`telemetry/`) | `ureq`, `rand` | `telemetry` is a recognized, not-included command (exit 2); emission compiles to a no-op, so the binary makes no telemetry network calls |

A shared dependency (`ureq`) is linked when any feature that needs it is on.

### CLI bundles

| Bundle | Features | Intended use |
| --- | --- | --- |
| `full` (**default**) | `repl`, `pull`, `skill`, `upgrade`, `telemetry` | Today's release binary; developer workstations |
| `agent` | `skill`, `pull` | Sandbox/Docker images for AI agents: one-shot/stdin queries, JSON output, no self-update, no telemetry network calls |
| `minimal` | *(none)* | Smallest query-only shell: one-shot, stdin, and plain interactive input |

The core query-engine run (flags, output modes, params, storage knobs) is
never gated; every bundle can run queries against in-memory and persistent
databases.

## Alternatives considered

- **Separate binaries per bundle** (for example `cypherlite-agent` next to
  `cypherlite`). One binary name per use case means duplicated `[[bin]]`
  targets, separate docs and help text for each, and scripts and skills that
  must know which name they are on. Rejected: the capability differences fit
  in one crate behind features, and a single `cypherlite` name keeps
  docs, skills, and invocations the same everywhere.
- **Runtime configuration flags instead of compile-time features** (for
  example `--no-telemetry` or an env var disabling `upgrade`). The code and
  its HTTP/TLS dependencies still ship, so the attack surface and binary size
  do not shrink, and a sandbox operator must trust that every entry point
  honours the switch. It also breaks the ADR-0069 rule that flags configure a
  query run and never encode lifecycle policy. Rejected: sandbox images need
  the capability to be physically absent, not merely switched off.
- **Negative `no-*` features** (for example `no-telemetry` subtracting from a
  full default). Cargo unifies features additively across the dependency
  graph, so one crate turning on `no-telemetry` would silently remove
  telemetry for every other dependent in the same build, and the
  combinations become hard to reason about and to test. Rejected: additive
  features are the only kind Cargo composes safely.
- **Starting with engine-level features first** (Parquet, VFS backends, and
  similar in the engine crates). Engine features cross crate boundaries
  (storage, executor, FFI, clients) and interact with the ADR-0103 moves that
  are still landing. The CLI features are self-contained within one crate and
  directly serve the agent-sandbox use case. Rejected as the starting point:
  the CLI proves the rules at low risk, and engine features follow later
  under the same rules, each with its own ADR.

## Consequences

- `cypherlite-cli` gains `[features]` with `default = ["full"]`; `rustyline`,
  `cypherlite-load`, `ureq`, `sha2`, and `rand` become optional dependencies.
  Default builds and release artifacts are unchanged.
- `main.rs` keeps recognizing every command word in all builds. Only the
  dispatch target changes: the real command, or the shared not-included error
  (exit 2).
- The hand-written `--help` becomes feature-conditional, and
  `docs/api/cli/spec.md` gains a feature/bundle table and marks each command
  with its feature. Help and spec must stay in sync per build, under the same
  AGENTS.md §3 rule as today.
- CI time grows by one build-and-test job per bundle. That is the cost of the
  guarantee that no bundle rots.
- Agent sandbox images can be built without self-update, telemetry, or the
  line editor, and with a smaller dependency and network surface.
- The rules here are the template for engine-level features. Each future
  feature needs its own ADR before it is implemented.

## Later references

ADR-0105 adds a `parquet` feature to the engine crates and the CLI. Every flag
still parses in every bundle, but the `--snapshot-format parquet` value, like
`CYPHERLITE_SNAPSHOT_FORMAT=parquet`, needs that feature and fails at open
without it, so "storage knobs are never gated" no longer holds for that one
value.

ADR-0267 adds a `fetch-params` feature to the CLI and adds it to the `full`
and `agent` bundles (`minimal` stays query-only). Per ADR-0270, the flag
parses in every build (the parse half is ungated in `params.rs`) and, like
`--snapshot-format parquet` without `parquet`, fails closed at resolution
without the feature with a typed not-compiled message — so "every flag
parses in every build" holds; `--help` lists it only when compiled in.
