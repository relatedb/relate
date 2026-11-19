---
type: adr
area: engine
date: 2026-10-05
deciders: CypherLite maintainers
name: 0162-query-type-on-result-and-bolt
status: accepted
tags:
  - result-metadata
title: Query type on QueryResult and the Bolt RUN summary
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0283-no-c-abi-query-type-accessor]]"
  - "[[adr/0285-c-abi-result-metadata-accessors]]"
  - "[[adr/0309-bolt-summary-metadata-neo4j-4-4]]"
  - "[[adr/0319-bolt-execution-failures-at-pull]]"
  - "[[adr/0332-query-type-w-rw-by-returned-columns]]"
---

# 0162. Query type on QueryResult and the Bolt RUN summary

## Context

LABS-175, verified live against Neo4j 2026.09-community via Query API v2:
every response carries a top-level `queryType` — `"r"` (read, including
EXPLAIN of a read), `"w"` (write), `"rw"` (read-write, e.g. `MATCH … SET`,
and PROFILE of one), `"s"` (schema, e.g. `CREATE INDEX`). EXPLAIN
additionally returns a `queryPlan` tree, PROFILE a `profiledQueryPlan`,
and write counters live in a separate `counters` object. CypherLite keeps
its own clause-pipeline plan rendering (GitHub issue #14,
`cypherlite/src/explain.rs`); only the query-type classification is in
scope here.

Today a caller cannot learn whether a statement reads or writes without
executing it:

- `EXPLAIN` returns a single `plan` column of text rows
  (`explain::explain_result`); `PROFILE` appends `_profile_plan` /
  `_profile_time` / `_profile_rows` / `_profile_mutations` columns to the
  statement's own result. Neither names the statement's access mode.
- `QueryResult { columns, rows, stats, namespace }` carries no type
  field; `QueryStats` counts mutations after execution — too late for a
  routing decision.
- The Bolt server answers RUN with `fields` and PULL/DISCARD with
 `t_last` / `db` / `has_more` ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7). Neo4j drivers read the
  `type` key of the RUN SUCCESS summary to route read-only vs read-write
  statements; CypherLite does not send it.

The engine already has everything needed at plan time: the EXPLAIN
early-return in `Connection::run_query` holds the logical plan, and
`cypherlite_planner::clause_kind::is_write_clause` is the shared write
classifier, with the `call_is_write` hook covering write procedures. The
`QueryResult.namespace` field ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7) is the in-tree precedent for
stamping result metadata at construction: "construction is the stamp".

## Decision

1. **`QueryResult` gains a public `query_type` field of a new public enum
   `QueryType`**, set for every statement — plain runs, EXPLAIN, and
   PROFILE alike — at result construction, exactly like `namespace`
 ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7). The enum is re-exported by the `cypherlite` facade like
   `Namespace`. The value set is pinned to Neo4j's (LABS-175):
   `Read` / `Write` / `ReadWrite` / `Schema`, with wire codes `"r"`,
   `"w"`, `"rw"`, `"s"`.
2. **Classification is plan-based and total.** It is derived from the
   logical plan above the EXPLAIN/dispatch split in
   `Connection::run_query`, reusing `clause_kind::is_write_clause` and
   the `call_is_write` procedure hook, extended to tell schema DDL
   (index/constraint create/drop) apart from data writes: no write or
   schema clause → `r`; data writes without reads → `w`; reads and data
   writes → `rw`; schema DDL → `s`. Every planned statement gets exactly
   one value. EXPLAIN and PROFILE report the **wrapped statement's**
   type, so `EXPLAIN` answers "may this run on a read-only target?"
   without executing anything.
3. **The Bolt RUN SUCCESS summary gains a `type` key** carrying the same
   `"r"` / `"w"` / `"rw"` / `"s"` code — the key Neo4j drivers read for
 routing. Bolt stays a transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)), not Neo4j feature parity;
 this is one cheap summary key beside the [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7 `db` echo, not a
   `queryPlan` tree.
4. **EXPLAIN's `plan` column shape is unchanged.** The query type is
   result metadata, never a result row, so the TCK/cip-eval Gherkin
   assertions that trim and match plan lines are untouched.

## Alternatives considered

- **An extra column in the EXPLAIN/PROFILE result rows only
  (`explain-column`)**, like the `_profile_*` columns. Rejected: it
  changes the single-`plan`-column table shape the TCK/cip-eval Gherkin
  dialect trims and matches; it covers EXPLAIN/PROFILE but not plain
  runs, so a driver or FFI caller still cannot classify an ordinary
  statement; and no programmatic consumer should have to parse result
  rows for routing metadata.
- **CLI-only rendering of the type (`cli-display`)**, no engine API
  change. Rejected: the CLI is one surface of many; the Rust API, the C
  ABI and its JS/Java/Go clients, Python, WASM, and Bolt would all stay
  blind. The engine knows the type at plan time, so restricting it to
  one presentation layer is the same classification work for a fraction
  of the coverage. (The CLI may later render `query_type` in its output;
  that is a presentation choice, not this decision.)
- **Bolt summary key only, no engine field.** Rejected: it inverts the
  dependency direction — the transport would derive what the engine
  already knows — and every non-Bolt surface (Rust API, FFI, Python,
  WASM, CLI) would have no access. The `namespace` precedent
 ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7) puts result metadata on `QueryResult` and lets surfaces
  echo it.
- **Engine field only, no Bolt key.** Rejected: the primary consumer of
  `queryType` is a Neo4j driver making a routing decision from the RUN
  SUCCESS summary; without the `type` key such drivers must treat every
  CypherLite statement as a write. One map entry beside the existing
 `db` echo is cheap and keeps Bolt a transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)).
- **Adopt Neo4j's `queryPlan` / `profiledQueryPlan` tree and `counters`
  object wholesale.** Rejected: CypherLite deliberately keeps its own
  clause-pipeline plan rendering (`explain.rs`); LABS-175 scopes this
  work to the query-type classification, and cloning Neo4j's plan JSON
  is a separate, much larger decision.

## Consequences

- Every `QueryResult` — plain, EXPLAIN, PROFILE — carries a `query_type`;
  a caller routes read-only vs read-write from `EXPLAIN` without
  executing, over the Rust API and (via the RUN SUCCESS `type` key) over
  Bolt.
- The value set is exactly `"r"` / `"w"` / `"rw"` / `"s"`, aligned with
  the LABS-175 Neo4j measurements. A future fifth Neo4j value is a new
  ADR, not a silent enum growth.
- The public Rust surface grows `QueryType` and `QueryResult::query_type`;
  per the public-API rules the implementing change ships the
  `docs/api/rust/` spec + description + example, a C ABI accessor with
  `docs/api/c/` updates (language clients wrap the C ABI only), and the
  `docs/api/bolt/` spec entry for the RUN SUCCESS `type` key, all in the
  same change.
- `is_write_clause` gains a schema-DDL distinction for classification;
  its existing consumers (`validate.rs`, `namespaces.rs`) keep their
  current semantics — schema DDL still counts as a write for the
 read-only-namespace gate ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §4).
- `QueryStats` keeps counting mutations after execution; `query_type` is
  a pre-execution classification, not a counter summary. PROFILE's
  `_profile_mutations` column is unchanged.
- EXPLAIN/PROFILE result columns are unchanged; TCK and cip-eval
  assertions on the `plan` lines are unaffected.

## Later references

- [ADR-0283](./0283-no-c-abi-query-type-accessor.md): narrows the third Consequences bullet — the implementing
  change ships no C ABI accessor for `query_type` (the C ABI has no
  result-metadata accessor precedent; `namespace` has none either). The
  Rust and Bolt surfaces ship as decided; a
  `cypherlite_result_query_type` accessor is an additive follow-up if a
  C-ABI client ever needs it.
- [ADR-0285](./0285-c-abi-result-metadata-accessors.md): lands that follow-up — adds `cypherlite_result_query_type`
  and `cypherlite_result_namespace` to the C ABI as result-owned
  `const char *` accessors, with Python/JS/Java/Go client exposure.
- [ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md): reverses Decision item 3 — the Bolt RUN SUCCESS no longer
  carries `type`; it moves to the SUCCESS that ends the result (the last
  PULL or the DISCARD), where latest Neo4j and neo4rs read it. The
  `QueryResult.query_type` stamp and its vocabulary are unchanged.
- [ADR-0332](./0332-query-type-w-rw-by-returned-columns.md): supersedes Decision item 2's `w`/`rw` criterion — a write
 splits on returned columns (`plan_output_names`, [ADR-0319](./0319-bolt-execution-failures-at-pull.md)): a write
  with no returned columns is `w`, a write with returned columns is `rw`
  (latest Neo4j 2026.09.0; the LABS-323 driver findings D-53/D-54). The
  clause-presence split ("reads and data writes → `rw`") is no longer the
  rule.
