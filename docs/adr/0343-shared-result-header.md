---
type: adr
area: engine
claim: d1c20032bb03ba8749426f4d099085880357cab2
name: 0343-shared-result-header
status: proposed
tags:
  - errors
  - result-metadata
title: One shared result header for QueryResult and ExecutionFailure
REFERENCES:
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0162-query-type-on-result-and-bolt]]"
  - "[[adr/0221-engine-reported-statement-elapsed]]"
  - "[[adr/0224-result-column-types]]"
  - "[[adr/0319-bolt-execution-failures-at-pull]]"
---

# ADR-0343: One shared result header for QueryResult and ExecutionFailure

## Context

[ADR-0319](./0319-bolt-execution-failures-at-pull.md) added the staged entry point: a statement that planned but failed
while executing is reported as `ExecutionFailure` so a surface can announce
the result before it reports the error. `ExecutionFailure` re-declared the
five result-metadata fields `QueryResult` already declared — `columns`,
`column_types`, `query_type`, `namespace`, `elapsed` — with near-copies of
their docs. Two structs, one vocabulary: adding or changing a metadata
field means editing both, and the two can drift (the review flagged the
duplication, finding F-1-88, under the repository's "parallel state is a
bug" and "don't re-split a fact" code-shape rules).

Both types are stamped from the same construction facts: the planned output
names (`cypherlite_planner::plan_output_names`), the [ADR-0224](./0224-result-column-types.md) per-column
type stamp, the [ADR-0162](./0162-query-type-on-result-and-bolt.md) plan-derived query type, the [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7 resolved
namespace and the [ADR-0221](./0221-engine-reported-statement-elapsed.md) wall-clock elapsed. There is no reason for the
finished-result and failed-statement shapes to declare them separately.

## Decision

1. **`cypherlite::ResultHeader` (in `result.rs`) is the one home** for the
   result-metadata vocabulary: `columns`, `column_types`, `query_type`,
   `namespace`, `elapsed`.
2. **Both result shapes embed it.**
   `QueryResult { header: ResultHeader, rows, stats }` and
   `ExecutionFailure { header: ResultHeader, error }`. Construction stamps
   the same values as before; no behavior changes.
3. **The field paths change with it**: `result.columns` becomes
   `result.header.columns`, and so on for the other four fields. This is a
   breaking change to the public Rust API of the released `cypherlite`
   crate; every in-tree consumer (the Bolt server, CLI, FFI, HTTP, load
   tool, MCP, Python bindings, the test harnesses and examples) is updated
   in the same change, and `docs/api/rust/spec.md` documents the new shape.

## Alternatives considered

- **Keep the duplicated fields on both structs.** Rejected: parallel
  state — the two declarations can drift, and every metadata change must
  be made twice.
- **Give `ExecutionFailure` a nested copy of the fields under a private
  type only it uses.** Rejected: still a second declaration of the same
  vocabulary; the "one home" rule wants a single shared type, not a
  look-alike.
- **Keep `QueryResult`'s flat fields and expose the header through
  `Deref`.** Rejected: two paths to the same field; `Deref` as field
  sugar also hides the break instead of recording it, and moves out of a
  deref target are impossible, so call sites would mix both spellings.
- **Expose accessor methods instead of a public header.** Rejected: the
  established API style is public fields; methods would break every call
  site anyway and add a second way to read a public field.

## Consequences

- `cypherlite::QueryResult` and `cypherlite::ExecutionFailure` carry
  `header: ResultHeader`; metadata reads are `result.header.<field>` /
  `failure.header.<field>`. `ResultHeader` joins `QueryResult` in the
  crate-root re-exports.
- Adding a future result-metadata field edits `ResultHeader` once; both
  the finished and failed shapes get it.
- No behavior change: the same five values are stamped at the same points
  (`QueryResult::from_exec`, `ExecutionFailure::staged`,
  `explain::explain_result`, `explain::profile_result`), and
  `QueryResult::elapsed_ms` still performs the one Duration→ms conversion.
- `docs/api/rust/spec.md` (the `QueryResult` section), `staged.md` and
  the elapsed/query-type examples document the new paths in this change.
- Pinned by the engine tests that read the metadata (Labs-286 elapsed,
  Labs-290 column types, EXPLAIN/PROFILE, staged runs) and the surface
  tests that announce it (Bolt summary, FFI tabular, MCP, HTTP, CLI,
  Python, query-latency).
