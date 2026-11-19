---
type: adr
area: surface
claim: 4d40c6e1dbccb6dfe2c03fb3cff9b32a5379d107
name: 0470-serve-durability-mode
status: proposed
tags:
  - serve
  - wal
title: Durability mode selectable through serve
REFERENCES:
  - "[[adr/0077-embedded-durability-boundary]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0368-one-product-binary-serve-absorbs-bins]]"
  - "[[adr/0390-serve-memory-limit-flag]]"
---

# ADR-0470: Durability mode selectable through serve

## Context

[ADR-0077](./0077-embedded-durability-boundary.md) made the commit durability policy a three-mode setting — `full`,
`everysecond`, `snapshot` — selected through `OpenOptions::durability_mode`,
the query session's pre-verb `--durability-mode` flag, and the then-standalone
bolt server's own flag. [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md) retired the standalone servers: `cypherlite
serve` is the one product binary, and the storage knobs the bolt binary
carried — `--durability-mode` among them — were left library-level only
("giving serve any of them is a future ADR, not a silent flag here").
[ADR-0390](./0390-serve-memory-limit-flag.md) was that future ADR for the live-graph cap; the durability mode is
still unreachable through the product binary: `serve` parses no mode flag,
and no code reads a `CYPHERLITE_DURABILITY_MODE` variable (a repo-wide grep
finds none) — every serving process runs `full` unless the embedding
application sets `OpenOptions` by hand.

The Jepsen certification work (LABS-653) needs the opposite: its
crash-recovery matrix sweeps the durability modes against the served store,
so every mode must be selectable through the product binary — from the
harness's command line and from its environment, the two channels a nemesis
kills and respawns a process with.

## Decision

Both channels, one resolution — `--durability-mode` >
`CYPHERLITE_DURABILITY_MODE` > `full`:

1. **`cypherlite serve` accepts `--durability-mode full|everysecond|snapshot`**
 — a storage flag in [ADR-0390](./0390-serve-memory-limit-flag.md)'s shape, not a surface flag: it configures
   the served store's open, so it is not per-listener feature-gated, it is
   once-only, and it takes the query session's value grammar through
   `DurabilityMode::from_str`. The query session's pre-verb flag is
   unchanged.
2. **`OpenOptions::durability_mode` becomes `Option<DurabilityMode>`**:
   `Some(mode)` wins; `None` reads `CYPHERLITE_DURABILITY_MODE`, then the
   default `full`. The resolution lives in `open_config.rs` beside
   `CYPHERLITE_CHECKPOINT_INTERVAL_SECS`'s (`resolve_durability_mode`,
   resolved in the fail-before-lease preflight), but with the fail-open
   contract every value variable documented since carries: a set value that
   does not parse fails the open — the interval variable's silent
   `unwrap_or` default is legacy, not the pattern copied. Every open honors
   the variable: the query session and serve through the CLI's checked
   resolution (`flags::durability_mode_from_env_checked`, a usage error at
   exit 2 before the store opens — the `--max-traversal-depth` ladder), and
   library embedders (python, FFI, WASM, the bolt library entry) through the
   engine resolver.

The one resolution is shared by query and serve through the one helper, so
the two processes cannot drift on the same store — [ADR-0390](./0390-serve-memory-limit-flag.md)'s invariant,
restated for this knob.

## Alternatives considered

- **Serve flag only, no environment variable.** Rejected: harnesses and
  container supervisors configure processes through the environment, and the
  engine's other open-time defaults (checkpoint interval, snapshot format)
  are already variables — the mode would be the one knob a deployment
  cannot set without editing a command line.
- **Environment variable only, no serve flag.** Rejected for [ADR-0390](./0390-serve-memory-limit-flag.md)'s
  reason verbatim: the env is the *default*, not the only channel — the mode
  is a data-loss posture and belongs stated on the command that runs it;
  the loadtest and harness pin the flag spelling they pass.
- **Resolve the variable only in the CLI, keeping
  `OpenOptions::durability_mode: DurabilityMode`.** Rejected: the engine
  cannot distinguish "explicitly full" from "defaulted full", so the
  variable would either be invisible to every embedder that opens with
  defaults (python, FFI, WASM) or need a second parallel option field —
  parallel state for one policy.
- **Copy `CYPHERLITE_CHECKPOINT_INTERVAL_SECS`'s silent parse fallback
  (`unwrap_or(default)`).** Rejected: a typo'd value would silently serve
  `full` when `everysecond` was asked for — a silent fallback on a knob the
  operator must know is in effect; every value variable documented since
  (logical-graph-size-limit, max-traversal-depth, markdown-body-properties)
  fails open instead.

## Consequences

- `everysecond` and `snapshot` are selectable through the product binary:
  `cypherlite serve --durability-mode everysecond …`, and
  `CYPHERLITE_DURABILITY_MODE=snapshot cypherlite …` for the query session
  and serve alike.
- `OpenOptions::durability_mode` is a breaking type change
  (`DurabilityMode` → `Option<DurabilityMode>`) in a crate not yet on
  crates.io; every internal call site moves in this change (the CLI, the
  bolt library args, the loadtests, the query-latency suite), and the rust
  surface spec and reference move with it.
- A bad `CYPHERLITE_DURABILITY_MODE` — or a bad flag value — is a loud
  usage error (exit 2) on the CLI paths and a failed `Database::open` for
  embedders, never a silent default.
- The bolt library's `Args::durability_mode` becomes optional so the
  variable is not shadowed by that entry point's own default.
- [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md)'s "no product-binary surface" list of the retired bolt binary's
  storage knobs shrinks by this flag; the remaining ones
  (`--checkpoint-interval-secs`, `--wal-dir`, TLS) stay future ADRs.

References [ADR-0077](./0077-embedded-durability-boundary.md), [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md), [ADR-0390](./0390-serve-memory-limit-flag.md), [ADR-0232](./0232-network-surfaces-charter.md).
