---
type: adr
area: surface
name: 0216-javascript-client-ergonomics
status: accepted
tags:
  - clients
title: Idiomatic Node.js/TypeScript client — driver-style result ergonomics
REFERENCES:
  - "[[adr/0213-python-driver-result-ergonomics]]"
  - "[[adr/0214-python-result-graph]]"
  - "[[adr/0215-python-result-summary]]"
---

# ADR-0216: Idiomatic Node.js/TypeScript client — driver-style result ergonomics

## Context

LABS-285. The Node client (`clients/javascript`, `@neo4j-labs/cypherlite`)
wraps the C ABI via koffi. Today `conn.query()` returns
`{ columns, rows, queryType, namespace }` where rows are **positional
arrays** of JSON-parsed values: no record access by name, no mutation
counters, no driver conveniences, no TypeScript types, no library skill.
The Python effort [ADR-0213](./0213-python-driver-result-ergonomics.md)/[ADR-0214](./0214-python-result-graph.md)/[ADR-0215](./0215-python-result-summary.md)) established the
ergonomics set; the survey behind it (Kùzu — archived Oct 2025, its
Node API being the closest precedent; FalkorDB; SurrealDB)
shows the embedded-in-Node niche is open and Arrow/df-shaped bulk output
plus driver-style results are what that audience expects.

## Decision

1. **`Result` object.** `conn.query()` returns a `Result` with
   `columns`, `stats` (the canonical 11 counters, snake_case), `queryType`,
   `namespace`, and **rows as objects keyed by column name** — a breaking
   change from positional arrays (pre-1.0 npm package, recorded in the
   CHANGELOG). The result is fully materialized at query time; there is
   no fetch position.
2. **Driver conveniences** (synchronous — the engine is in-process, so
   there is nothing to await; Kùzu's Node API is synchronous for the same
   reason): `single()` (throws `ResultNotSingleError` on zero or >1) and
   `consume()` → a `ResultSummary` with `counters`, `queryType`,
   `namespace`, and `elapsedMs` measured at the client edge with
 `performance.now` ([ADR-0215](./0215-python-result-summary.md)'s rationale verbatim: in-process, the
   client edge is the execution edge). `peek()`/`fetch(n)` are
   deliberately absent: unlike the Python cursor there is no fetch
   position — `rows` is already an array, so `rows[0]` and `rows.slice(0,
   n)` say it directly.
3. **`graph()`** returns a `ResultGraph` (`nodes`, `relationships`,
   id-ordered, de-duplicated by id) extracted from the wire JSON shapes —
   `{"id","labels","properties"}` nodes, `{"id","type","start_node",
   "end_node","properties"}` relationships, paths as alternating arrays —
 walking nested arrays/objects, mirroring [ADR-0214](./0214-python-result-graph.md).
4. **FFI**: the tabular C ABI gains `cypherlite_result_stats(result) ->
   str` returning the canonical counters JSON (`stats_to_wire_json` via
   `query_stats_to_wire`), stamped into the result at pack time. The
   committed header is hand-maintained C (cbindgen's default output is C++
   and drops the ownership/threading/panic-safety contract block, so
   `make gen-ffi-header` was removed in favor of hand-maintaining the
   declaration); every other C-ABI client
   may adopt it in its own change (cross-surface metadata rule).
5. **TypeScript**: a hand-written `index.d.ts` ships in the package —
   full types for `Database`, `Connection`, `Result`, `ResultSummary`,
   `ResultGraph`, and the wire entity shapes.
6. **Library skill**: `.agents/skills/cypherlite/SKILL.md` ships inside
   the npm package (library-skills.io convention, like the Python wheel),
   and the CLI bundle's `client-javascript.md` reference updates in the
   same change.

## Alternatives considered

- **Keep positional rows, add a `records` view** — rejected: two row
  shapes on one result; column names are already known, so objects are
  the JS/TS idiom and the break is cheap pre-1.0.
- **Async API (`await conn.query()`)** — rejected: the engine runs
  in-process and synchronously; an async wrapper would add a promise per
  statement for no I/O (Kùzu's precedent is synchronous; the Neo4j driver
  is async only because it is a network client). An async wrapper can be
  layered later if wanted.
- **Apache Arrow output (`toArrow()`)** — deferred, not rejected: no
  consumer demand yet and the JSON cell path is the ABI's only bulk
  channel; revisit with the engine-side Arrow FFI if demand appears.
- **Counters via the JSON envelope call instead of a new FFI export** —
  rejected: would re-run the statement's serialization path and change
  the client's call pattern; one typed export is cheaper and reusable by
  every C-ABI client.

## Consequences

- Breaking change to `result.rows` (positional → objects); CHANGELOG
  records it; the loadtest and README examples update with it.
- `clients/javascript` gains `index.d.ts`, the library skill, and a
  rewritten `test.js` covering the new surface.
- Docs per the client-facing rules: `docs/api/javascript/` spec +
  description + examples, `client-javascript.md` in the CLI bundle,
  CHANGELOG.
- Other C-ABI clients (Go, Java) can adopt the stats export later —
  cross-surface result-metadata alignment tracks that separately.
