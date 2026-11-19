---
type: adr
area: surface
name: 0215-python-result-summary
status: accepted
tags:
  - python
title: "`Cursor.consume` → `ResultSummary` with client-edge timing"
REFERENCES:
  - "[[adr/0213-python-driver-result-ergonomics]]"
  - "[[adr/0221-engine-reported-statement-elapsed]]"
  - "[[adr/0285-c-abi-result-metadata-accessors]]"
---

# ADR-0215: `Cursor.consume()` → `ResultSummary` with client-edge timing

## Context

[ADR-0213](./0213-python-driver-result-ergonomics.md) deferred `consume`/`ResultSummary` because the engine's stats
carry no statement timing and the driver's summary is server-side. But
CypherLite's Python binding is in-process: there is no round trip, so a
`time.perf_counter()` pair around the native `Database.execute()` call
measures the whole statement execution (parse, plan, run, hydration) with
only PyO3 boundary overhead on top — the same number the Stack Overflow
notebook computes by hand. LABS-286 tracks the engine-side elapsed field
as future work; the Python surface should not wait for it.

## Decision

Add `Cursor.consume()` returning a `cypherlite.ResultSummary`:

- `counters` — the 11 mutation counters (same dict as `Cursor.stats`)
- `namespace` — the statement's resolved namespace ([ADR-0285](./0285-c-abi-result-metadata-accessors.md))
- `query_type` — the plan-time classification (`"r"`/`"w"`/`"rw"`/`"s"`)
- `elapsed_ms` — wall-clock milliseconds measured at the client edge
  (`perf_counter` around the native execute call), so an in-process
  statement's timing is its execution timing

Driver semantics otherwise: `consume()` discards any remaining rows and
returns the summary; it is idempotent (a second call returns the same
summary). `Connection._raw_execute` captures `namespace`, `query_type`
and the timing alongside `(columns, rows, stats)`; `executemany` reports
the final execution's values. When the engine later reports elapsed
itself (LABS-286), `elapsed_ms` switches to that value — a semantic
refinement, not an API change.

The native `QueryResult` does not gain `consume()`: it is a value object
the caller already holds entirely; a summary there would restate its own
fields.

## Alternatives considered

- **Wait for engine-side timing ([ADR-0213](./0213-python-driver-result-ergonomics.md)'s stance)** — rejected: in an
  embedded engine the client edge *is* the server edge minus a thin FFI
  boundary; the measurement is available now and the field can be
  re-sourced later without API change.
- **`elapsed_ms` on `Cursor.stats`** — rejected: `stats` is the counters
  dict and stays counters-only; timing belongs to the summary object, as
  in the driver.
- **A `t_first`/`t_last` pair like Bolt** — rejected: rows are
  materialized in one native call; there is no streaming to measure
  against.

## Consequences

- New public API item: spec + description + example + website reference +
  CLI skill bundle + bundled library skill update in the same change.
- `elapsed_ms` is a client-edge measurement and is documented as such; a
  future engine-side field supersedes it inside the same property.
- LABS-286 narrows to the engine-side elapsed + cross-surface rollout.

## Later references

- [ADR-0221](./0221-engine-reported-statement-elapsed.md) lands the engine-reported `elapsed` on `QueryResult`;
  `ResultSummary.elapsed_ms` re-sources to it (the switch predicted
  above), deleting the `perf_counter` measurement outright — the native
  module ships in the same wheel, so the stamp is always present.
