---
type: adr
area: cli
date: 2026-09-25
deciders: CypherLite maintainers (agreed in conversation)
name: 0267-cli-fetch-params-flag
status: accepted
tags:
  - commands
title: CLI `--fetch-params NAME=SOURCE` binds fetched JSON as a query parameter
REFERENCES:
  - "[[adr/0081-opt-in-load-csv-file-capability]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0270-cli-flag-parse-fetch-split]]"
---

# 0267. CLI `--fetch-params NAME=SOURCE` binds fetched JSON as a query parameter

- **Renumbered**: from 0136 (shared-number cleanup)

## Context

Agents integrating CypherLite with remote or file data currently have to
round-trip it themselves: fetch JSON in a wrapper script, then pipe it in via
`--param name=<json>` (as `neo4j-so.py` in the Stack Overflow demo does) or
through the Python API. That works but forces shell-quoting gymnastics for
large documents and makes the "fetch → bind → UNWIND → MERGE" pipeline a
script instead of one CLI invocation.

The `LOAD JSON` clause (the engine-side answer, in the spirit of the user's
2013 Neo4j `LOAD JSON` proposal that later became `apoc.load.json`/`apoc.load.jsonParams`)
is being designed separately; until it lands, the CLI flag is the vehicle —
and the clause can later wrap the same JSON→`CypherValue` conversion, so no
work is throwaway.

`--fetch-params` is a **CLI-process** fetch, not an engine capability: the CLI reads
the source, parses the JSON, and hands the engine an ordinary bound parameter.
The engine's `LOAD CSV` capability model ([ADR-0081](./0081-opt-in-load-csv-file-capability.md)) is untouched.

## Decision

Add a `--fetch-params NAME=SOURCE` flag (repeatable) to the `cypherlite` CLI:

- `SOURCE` is an `http://`/`https://` URL (GET) or a filesystem path (bare or
  `file://`-prefixed). Any other URL scheme is a usage error.
- The fetched/read bytes must parse as JSON; the document (any shape) is
  bound as the parameter `NAME` exactly like `--param` (same
  JSON→`CypherValue` conversion).
- Malformed flag values (no `=`, empty name, unsupported scheme) exit 2
  (usage); fetch, I/O, HTTP-status, size-cap, and JSON-parse failures exit 1
  (runtime), reported as `--fetch-params NAME: …`.
- Caps: 64 MiB per source (matching `pull`'s response cap, detected — not
  truncated — by a cap+1 read), one 60 s global timeout per request
  (matching `pull`'s HTTP posture).
- No network allowlist for this flag: the fetch runs in the CLI process under
  the invoker's own authority — the same trust as `curl | cypherlite
  --param`, and the same boundary the embedded model already relies on. The
  engine-level URL policy question belongs to the `LOAD JSON` clause ADR.
- The flag ships behind a `fetch-params` cargo feature ([ADR-0104](./0104-cargo-feature-bundles.md) bundles: part of
  `full` and `agent`); `--help` lists it only when compiled in.

## Alternatives considered

- **Engine `LOAD JSON` clause now** — the right long-term surface, but a
  dialect change with a URL-fetch policy to design; deliberately deferred to
  its own ADR (the apoc.load.json-shaped work in flight will feed it).
  Rejected for this change only because the CLI flag is the smaller surface
  that agents can use today.
- **`--param` with URL auto-detection** — rejected: surprising implicit
  network I/O hidden behind a value-parsing flag; an explicit `--fetch-params` makes
  the fetch visible and separately gated.
- **A plugin/WASM host function** — rejected for this need: the process
  boundary already exists for a local CLI; a sandbox is only forced for
  server-side untrusted code.

## Consequences

- One CLI invocation covers fetch→bind→UNWIND→MERGE for JSON APIs
  (Stack Exchange, GitHub, deps.dev) and JSON files.
- `pull` (graph→graph) and `--fetch-params` (document→parameter) stay complementary;
  neither subsumes the other.
- The CLI gains a `ureq` dependency path for the `fetch-params` feature (already a
  workspace dependency via `upgrade`/`telemetry`/`pull`); no new dependency.
- When the `LOAD JSON` clause lands, `--fetch-params` remains the CLI sugar for it
  (or is re-implemented on top of it); the parameter semantics are identical,
  so no breaking change is anticipated.

## Later references

[ADR-0270](./0270-cli-flag-parse-fetch-split.md) amends this decision: `NAME=SOURCE` parsing and source classification
are ungated (`params.rs`), so the flag parses in every build instead of being an
unknown flag in `minimal`; without the `fetch-params` feature resolution fails
closed with a typed not-compiled message. Caps, trust model, and bundle
membership are unchanged.
