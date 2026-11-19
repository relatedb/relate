---
type: adr
area: engine
claim: aa8443a0dfdb341b456a7ae680bf62a24643ac21
name: 0332-query-type-w-rw-by-returned-columns
status: proposed
tags:
  - conformance
  - result-metadata
title: Query type w vs rw split by returned columns (Neo4j 2026.09)
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0162-query-type-on-result-and-bolt]]"
  - "[[adr/0309-bolt-summary-metadata-neo4j-4-4]]"
  - "[[adr/0319-bolt-execution-failures-at-pull]]"
SUPERSEDES:
  - "[[adr/0162-query-type-on-result-and-bolt]]"
---

# ADR-0332: Query type w vs rw split by returned columns (Neo4j 2026.09)

## Context

The LABS-323 one-time neo4rs 0.9.0-rc.10 driver probe against latest
Neo4j (2026.09.0) at Bolt 4.4 recorded the result summary's query type
for the summary scenarios:

- `CREATE (:N {…})-[:REL {…}]->(:N)` → `Write` (`w`)
- `MATCH (n:N) SET n:X, n.c = 3 REMOVE n.b` → `Write` (`w`) — findings
  D-53
- `MATCH (n:N) DETACH DELETE n` → `Write` (`w`) — finding D-54
- `UNWIND … RETURN …` → `Read` (`r`); `CREATE/DROP INDEX` →
  `SchemaOnly` (`s`)

[ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md) landed where the `type` key rides (the SUCCESS that ends the
result), so the placement matches. The classification values do not:
[ADR-0162](./0162-query-type-on-result-and-bolt.md) item 2 splits `w` from `rw` by clause presence — any read clause
beside a data write is `rw`, and `MATCH` counts as a read clause — so
`MATCH … SET`, `MATCH … DETACH DELETE` and `MATCH … REMOVE` without
RETURN report `rw` where Neo4j 2026.09.0 reports `w`. D-53 and D-54 are
the last failing divergent driver scenarios.

The read half of Neo4j's `rw` is not "the statement contains a MATCH" but
"the statement returns columns": `CREATE (n) RETURN n` is `rw`, while
`MATCH (n) SET n.x = 1` — which reads to feed the write but returns
nothing — is `w`.

The engine already owns that column fact at plan time:
`cypherlite_planner::plan_output_names` ([ADR-0319](./0319-bolt-execution-failures-at-pull.md)), the final stage's
exported names — the same fact every surface announces as the result's
columns. A planned statement passed the UNION column-agreement check at
validation, so its output names always derive.

## Decision

1. **The `w` vs `rw` criterion in `cypherlite_planner::query_type`
   changes from "a read clause sits beside the write" to "the planned
   statement returns columns"** — `plan_output_names(plan)` non-empty.
   No write and no schema clause → `r`; schema DDL with no data clause
   alongside → `s`; a write with no returned columns → `w`; a write with
 returned columns → `rw`. This supersedes [ADR-0162](./0162-query-type-on-result-and-bolt.md) item 2's "reads and
 data writes → `rw`" criterion; the rest of [ADR-0162](./0162-query-type-on-result-and-bolt.md) (the
   `QueryResult.query_type` stamp, the pinned four-value set) is
   unchanged.
2. **One place, no per-surface re-derivation.** The classification stays
   the single plan-derived `query_type` in the planner; every surface
   reads the stamped `QueryResult.query_type` (Bolt's ending-SUCCESS
   `type`, the FFI's `cypherlite_result_query_type`, Python's
   `query_type`, the JS/Java/Go clients over the C ABI). A surface that
   recomputed the split from its own knowledge of the result would be a
   second classification table (AGENTS result-metadata alignment).
3. **The shared access fold is unchanged.** The write/schema flags and
   the schema-vs-data-mix decision (a schema clause mixed with data
   clauses across NEXT stages still counts as a write) keep coming from
 the one fold the [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §4 namespace gate shares
   (`cypherlite_planner::access`); only its projection into the query
   type changes. A `CALL` of a writes-flagged procedure stays a write
   and — the built-in write procedures are all void, and a bare `CALL`'s
   plan-time output names are empty — a bare one stays `w`.
4. **EXPLAIN and PROFILE keep reporting the wrapped statement's type.**
   The classification is plan-based and pre-execution, so
   `EXPLAIN CREATE (n)` answers `w` (a write with no returned columns)
   without executing anything — unchanged behavior, now for the new
   criterion too.

## Alternatives considered

- **Keep the clause-presence split (`rw` whenever a read clause sits
  beside a write).** Rejected: it diverges from latest Neo4j
  (2026.09.0) — the recorded D-53/D-54 outcomes are `Write`, and the
  neo4rs driver keeps reading the server's `rw`; a MATCH that only
  feeds a SET returns no columns, so `rw` overstates what the
  statement did.
- **Derive the split per surface (Bolt from its own fields/rows
  knowledge).** Rejected: parallel state — the query type is result
  metadata every surface must report identically; a second derivation in
  any surface can drift from the engine's.
- **Classify from the executed result's columns instead of the plan.**
  Rejected: the classification must precede execution — EXPLAIN reports
  the wrapped statement's type without running it, and the type gates
  access before dispatch (HTTP READ transactions, the MCP `read-cypher`
  tool, the Bolt bookmark-after-write decision).
- **Treat a trailing bare `CALL` as returning columns, conservatively
  (procedure default yields).** Rejected: the procedure signature
  registry lives in the executor, so the planner cannot know default
  yields at plan time; every built-in writes-flagged procedure is void,
  so a bare write-procedure `CALL` returning "columns" would report
  `rw` where both the plan and the runtime result have none.
- **Add a fifth value (e.g. read-feeds-write).** Rejected: the value set
 is pinned to Neo4j's four ([ADR-0162](./0162-query-type-on-result-and-bolt.md)); a fifth value is a new ADR, not
  a fix for a divergence.

## Consequences

- Writes without RETURN — `MATCH … SET`, `MATCH … REMOVE`,
  `MATCH … DETACH DELETE`, `DELETE`, `CREATE`, `MERGE`, `FOREACH` — now
  report `w` on every surface; writes with RETURN (`CREATE (n) RETURN n`,
  `MATCH … SET … RETURN …`) stay `rw`. Reads stay `r`, schema DDL stays
  `s`.
- Pinned values flip in the same change: the planner's `query_type` unit
  tests, the engine's EXPLAIN/PROFILE tests, the Bolt protocol summary
  tests, the JS client test, the Python driver-ergonomics example, and
  the affected `docs/api/` specs (rust, bolt, c, python, javascript, go,
  java) — the result-metadata alignment rule.
- Gates that test `== Read` / `!= Read` (HTTP READ transactions, the MCP
  `read-cypher` tool, Bolt's bookmark-after-write) are unaffected: `w`
  and `rw` were already on the same side of each check.
- D-53 and D-54 were the last failing driver scenarios of the one-time
  neo4rs 0.9.0-rc.10 probe; both now answer like Neo4j.
- [ADR-0162](./0162-query-type-on-result-and-bolt.md) gains a Later references note pointing here for item 2.
- Pinned by the planner unit tests, the engine tests, the Bolt protocol
  tests, and
  `surfaces/cypherlite-bolt/tests/compat/scripts/protocol/query-type.bolt`.
- The openCypher TCK is unaffected: query type is engine result
  metadata, never a TCK assertion.
