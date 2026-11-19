---
type: adr
area: surface
claim: 0a744fbcdf95f3c281d45b97e4ae1fd3d1c87029
name: 0309-bolt-summary-metadata-neo4j-4-4
status: proposed
tags:
  - bolt
  - conformance
title: |-
  Bolt result summary metadata matches Neo4j 4.4: t_first on RUN SUCCESS; type, stats, t_last and db on the ending SUCCESS only
REFERENCES:
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0162-query-type-on-result-and-bolt]]"
  - "[[adr/0224-result-column-types]]"
  - "[[adr/0310-bolt-bookmarks]]"
  - "[[adr/0311-bolt-tx-open-results-by-qid]]"
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
---

# ADR-0309: Bolt result summary metadata matches Neo4j 4.4: t_first on RUN SUCCESS; type, stats, t_last and db on the ending SUCCESS only

## Context

The LABS-323 probes compared cypherlite-bolt with the latest Neo4j
(2026.09.0) at Bolt 4.4, through a raw wire probe (findings F-11, F-13,
F-18, F-19, F-20, F-40) and through the neo4rs driver (D-52 to D-56).
The summary metadata differs in five ways:

- RUN SUCCESS carries `type` ([ADR-0162](./0162-query-type-on-result-and-bolt.md) item 3) and no `t_first`. Neo4j
  sends `t_first` on RUN SUCCESS and `type` on the SUCCESS that ends the
  result.
- A PULL SUCCESS that leaves records unread carries `t_last` and `db`
  beside `has_more`. Neo4j sends only `{has_more: true}`.
- DISCARD SUCCESS carries only `db`. Neo4j's ending DISCARD SUCCESS has the
  same keys as an ending PULL SUCCESS.
- No SUCCESS carries `stats`, so a write's counters never reach a driver.
  neo4rs reads its summary from the ending SUCCESS only, so it reports
  query type `Unknown` and zero counters for every statement.
- The engine counts no schema changes. `QueryStats` has no index or
  constraint counters, and `cypherlite::query_stats_to_wire` hard-codes
  them to 0 for the HTTP, FFI, Python and MCP surfaces. Its label and
  property counters follow the openCypher TCK's graph diff, not Neo4j's
  update counters.

`columnTypes` on RUN SUCCESS is the [ADR-0224](./0224-result-column-types.md) CypherLite extension key. The
maintainers keep it (F-12 is an accepted divergence).

## Decision

1. **RUN SUCCESS** is `{fields, columnTypes, t_first}`, plus `qid` inside
 an explicit transaction ([ADR-0311](./0311-bolt-tx-open-results-by-qid.md)). `t_first` is the engine's
   `QueryResult.elapsed` in whole milliseconds: the time until the buffered
   result was available. RUN SUCCESS no longer carries `type`. This
 reverses [ADR-0162](./0162-query-type-on-result-and-bolt.md) item 3.
2. **A page that leaves records unread** (a PULL or DISCARD with `n`
   smaller than the remaining rows) answers exactly `{has_more: true}`.
3. **The SUCCESS that ends a result** (the PULL or DISCARD that consumes
   its last row) carries `type` (`"r"`, `"w"`, `"rw"` or `"s"`, the
 [ADR-0162](./0162-query-type-on-result-and-bolt.md) vocabulary, from `QueryResult.query_type`), `t_last` (whole
   milliseconds spent streaming or discarding the result, summed over its
   PULL and DISCARD messages) and `db` (the namespace the statement ran in,
 [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7). It adds `stats` when any counter is non-zero, and
 `bookmark` after an auto-commit statement ([ADR-0310](./0310-bolt-bookmarks.md)). A DISCARD that
   ends a read answers `{type: "r", t_last: 0, db: "neo4j"}` plus the
   bookmark.
4. **`stats`** holds Neo4j's kebab-case counters, only the non-zero ones:
   `nodes-created`, `nodes-deleted`, `relationships-created`,
   `relationships-deleted`, `properties-set`, `labels-added`,
   `labels-removed`, `indexes-added`, `indexes-removed`,
   `constraints-added`, `constraints-removed`, plus
   `contains-updates: true` whenever any counter is present. A statement
   whose counters are all zero (every read) sends no `stats` key. The keys
   are derived from the canonical `cypherlite_model::wire::STAT_COUNTERS`
   table (snake_case to kebab-case), so the Bolt map and the other
   surfaces' `stats` objects come from one counter list.
5. **Engine change: schema counters.** `QueryStats` gains
   `indexes_added`, `indexes_removed`, `constraints_added` and
   `constraints_removed`. The write overlay counts them where it records
   the schema change: an index created or dropped by index DDL, and a
   constraint created or dropped by constraint DDL (including the
   constraints a graph-type change derives or purges). The range index
   that backs a `UNIQUE`/`KEY` constraint is part of that constraint and
   does not count as an index. An `IF [NOT] EXISTS` statement that changes
   nothing counts 0.
6. **Engine change: Neo4j's update counters.** The existing label and
   property counters follow the openCypher TCK's graph diff: `labels_added`
   counts label *names* new to the graph (two `:A` nodes count 1), an
   overwrite counts one property set and one removed, and a deleted node
   counts its properties removed and, as the last carrier, its labels
   removed. Neo4j's counters, which drivers compare, count differently
   (`CREATE (:A), (:A)` is `labels-added: 2`; `DETACH DELETE` counts no
   labels or properties; `REMOVE n.p` is one `properties-set`). The TCK
   needs the first family, so `QueryStats` keeps it and gains the second
   beside it, counted at the same overlay mutations: `node_labels_added`
   (per node), `node_labels_removed` (per node, by `REMOVE` only) and
   `properties_written` (one per property a `CREATE`, `SET` or `REMOVE`
   writes).
7. **`query_stats_to_wire`** renders Neo4j's counters: `labels_added` and
   `labels_removed` from the per-node counters, `properties_set` from
   `properties_written`, and the four schema counters, which it no longer
   hard-codes to 0. Bolt renders that same snapshot, so the HTTP, FFI,
   Python, MCP and WASM `stats` objects change with it (AGENTS
   result-metadata parity). The Rust `QueryStats` keeps both families.

## Alternatives considered

- **Keep `type` on RUN SUCCESS as well as the ending SUCCESS.** Drivers
  that merge both maps would see it twice, and neo4rs would still miss it
  if only RUN carried it. Rejected: Neo4j sends it once, on the ending
  SUCCESS, and the Python driver's `summary.query_type` already reads the
  merged metadata, so moving it keeps that driver's behavior.
- **Send every counter, zeros included, as the HTTP envelope does.**
  Rejected: Neo4j omits zero counters and the whole map for reads, and
  drivers default absent counters to 0. Zeros would only add bytes.
- **Count the backing index of a `UNIQUE` constraint as an index.**
  Rejected: Neo4j reports `constraints-added: 1` and no index for that
  statement, and SHOW INDEXES already lists the backing index under the
  constraint.
- **Fold `properties_removed` into `properties_set` instead of a new
  counter.** Rejected: an overwrite counts in both TCK counters, and a
  delete counts its entity's properties removed, so the fold reports 2 for
  one overwrite and a property set for every delete, where Neo4j reports 1
  and 0.
- **Switch the existing label and property counters to Neo4j's
  semantics.** Rejected: the openCypher TCK side effects (`+labels`,
  `-properties`) are the language contract and are checked from these
  counters.
- **Render Neo4j's counters only on the Bolt surface.** Rejected: one
  helper renders the counters for every surface, and a Bolt-only
  rendering would make Bolt and HTTP report different counts for the same
  statement.
- **Measure `t_last` per PULL instead of summed over the result.**
  Rejected: a paged result would report only its last page's time.
  Neo4j's `t_last` is the time to the last record, which the sum
  approximates.

## Consequences

- neo4rs, the Python driver and the JS driver read `result_available_after`,
  `query_type`, `counters` and `consumed_after` from cypherlite-bolt as
  they do from Neo4j.
- `columnTypes` stays on RUN SUCCESS as a CypherLite extension.
- HTTP, FFI, Python, MCP and WASM callers now see non-zero schema counters
  and Neo4j's label and property counts. Their docs say so. The TCK side
  effects and PROFILE's `_profile_mutations` keep the graph-diff counters.
- [ADR-0162](./0162-query-type-on-result-and-bolt.md) gains a Later references note pointing here for item 3.
- Pinned by `tests/compat/scripts/protocol/summary-metadata.bolt` and the
  `protocol_summary_tests.rs` unit tests.

## Later references

- [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) adds `t_first` and the `EXPLAIN`/`PROFILE` plan pair
 (`plan`/`profile`, [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)) to the ending SUCCESS item 3 pins:
  Neo4j sends `t_first` on both summaries and carries the plan on the
  drained result, so a client that reads only the ending summary
  (cypher-shell) renders its `Time` and the plan from it. Everything else
  here — RUN's `{fields, columnTypes, t_first}`, the `has_more` page,
  `stats` and `bookmark` — is unchanged.
