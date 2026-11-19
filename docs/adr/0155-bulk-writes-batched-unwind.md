---
type: adr
area: engine
date: 2026-10-01
deciders: CypherLite maintainers (agreed in conversation)
name: 0155-bulk-writes-batched-unwind
status: accepted
tags:
  - load
  - write-path
title: "Bulk writes and imports: batched UNWIND, bounded failure, idempotent MERGE mode"
REFERENCES:
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0154-value-clone-cost]]"
  - "[[adr/0158-streaming-single-process-pull]]"
  - "[[adr/0171-lazy-per-label-snapshot-probe]]"
---

# 0155. Bulk writes and imports: batched UNWIND, bounded failure, idempotent MERGE mode

## Context

`cypherlite pull` ([ADR-0080](./0080-cli-pull-command.md)) materializes a fetched subgraph with **one
engine statement per element**: `CREATE (n:Label) SET n = $props RETURN n`
per node, `MATCH (a), (b) WHERE id(a) = $s AND id(b) = $e CREATE (a)-[r:T]->(b)`
per relationship. The graph-examples differential suite ([ADR-0144](./0144-graph-examples-differential-suite.md)) showed
this cannot ingest real datasets: recommendations (28,863 nodes) ran 2 h at
100 % CPU without finishing; pole (61,521) timed out at 10 min;
fraud-detection (332,973) could not even fetch its single-response payload.

Measured causes (release/debug, `examples/pull_rca.rs` in
`testing/cypherlite-graph-examples`):

1. **Per-statement cost is O(N).** Any statement that touches a label pays a
   one-time cost proportional to the graph: labeled create 5.0 → 17.6 →
   64.5 ms/op at 1k/5k/20k nodes; labeled match + `SET` 11.1 → 51.5 →
   203.6 ms/op. Read-only, bare `CREATE`, and property-only writes are flat.
   One statement per element therefore makes a pull O(N²).
2. **Batched statements amortize that cost away.** `UNWIND range(1,10000) AS
   i CREATE (n:Batch {v: i})` = 0.39 s total; 20,000 nodes with labels and
   properties = 1.5 s total; 6,000 relationships in one statement = 0.40 s.
3. **`id()` equality is not a lookup.** `MATCH (a) WHERE id(a) = $r` scans
   per row (200 lookups: 0.36 s at 2k nodes, 7.5 s at 20k), and
   `MATCH (a), (b) WHERE id(a) = $s AND id(b) = $e` is a true cartesian
   (200 rows over 2k nodes > 600 s). This is what makes per-relationship
   statements the worst case. Tracked as its own engine fix (id-lookup
   pushdown); this ADR defines the loader-side shape that works with or
   without it.
4. **Variable reads deep-clone their value** ([ADR-0154](./0154-value-clone-cost.md)), which multiplies
   through any loader that re-reads large property maps.

## Decision

Bulk writes and imports use **batched `UNWIND $rows` statements** with
**bounded batch size**, and the loader gains an **idempotent MERGE mode**:

- **Batch shape.** One statement per chunk (`BATCH_ROWS = 1_000`, shipped):
  - nodes: `UNWIND $rows AS row CREATE (n:$(row.labels)) SET n = row.props
    RETURN row.eid AS reid, elementId(n) AS lid` — dynamic labels and rel
    types are supported and verified, so no grouping by label set is needed;
    the returned rows give the remote-elementId → local-id mapping in one
    pass, and `elementId()` is used because Neo4j deprecates `id()`. The
    element-id string form has one owner
    (`cypherlite_core::{node,rel}_element_id` / `parse_{node,rel}_element_id`),
    shared by `elementId()` and the loader.
  - relationships: `UNWIND $rows AS row MATCH (a) WHERE elementId(a) = row.s
    MATCH (b) WHERE elementId(b) = row.e CREATE (a)-[r:$(row.type)]->(b)
    SET r = row.props RETURN row.eid AS reid, elementId(r) AS lid` — the
    `elementId()` equality is an O(1) seek (executor id seek, same change),
    which is what makes the per-row endpoint lookup viable.
  - reuse path: the same batched shapes keyed on the local element id
    (`MATCH (n) WHERE elementId(n) = row.eid SET n = row.props SET
    n:$(row.labels)`) for elements already bound to this source.
  - every batch asserts the returned row count matches the batch (a partial
    batch would silently under-bind and duplicate on a re-pull).
- **Bounded failure.** A failing batch does not abort the whole import by
  default. The loader retries the batch in halves (binary dissection) down to
  a single row, so one poison row costs its own failure while every other row
  lands; the failures are reported as `(row index, error)` and the import
  returns a partial-success result with counts. A `--strict` mode (whole
  import fails on the first bad batch) stays available for callers that need
  all-or-nothing semantics. Rationale: for a data import, salvaging 99.99 %
  of rows and reporting the rest beats losing the batch, and the retry cost
  is logarithmic in the batch size.
- **Idempotent MERGE mode (opt-in).** An alternative materialization that
  emits `MERGE` keyed on a **merge key**, derived from the source's
  uniqueness constraints / node keys when they exist (Neo4j `SHOW
  CONSTRAINTS` / `apoc`-less introspection), else from a configured key
  property (default: the remote elementId). Nodes:
  `UNWIND $rows AS row MERGE (n:Label {key: row.key}) SET n += row.properties`;
  relationships: `MERGE (a)-[r:T {key: row.key}]->(b)`. A merge key MUST be
  backed by a uniqueness constraint created before the load, otherwise every
  MERGE is a scan (the constraint's owned index is what makes it a lookup).
  This mode is for repeated/incremental loads where the id mapping is not
  available (no prior pull state).
- **Where the same pattern applies.** The rule — *never issue one statement
  per element* — applies to every bulk path in the workspace: the dataset
  generators (`testing/cypherlite-datasets` fraud/identity loaders), any
  future `LOAD CSV` batching, the Bolt ingestion path for large UNWIND
  batches, and the graph-examples suite's own setup. Each is converted as it
  is touched; the ADR is the reference for the shape.

## Alternatives considered

- **Keep per-element statements and optimize only the engine**: the
  per-statement label cost is real and worth fixing (see Consequences), but
  even at zero cost a statement per element pays parse + plan + execute
  overhead per element; batching is the structurally right shape for bulk
  data. Rejected as the primary answer.
- **One giant statement for the whole dataset**: unbounded memory (all rows
  materialized at once), a single failure loses everything, and the
  transaction becomes enormous. Bounded chunks with dissection keep both
  properties. Rejected.
- **Abort on first failure**: loses an entire import for one bad row and
  gives the operator no way to see how far it got. Rejected as the default;
  kept as `--strict`.
- **Always MERGE**: MERGE is slower than CREATE and needs a constrained key;
  for a first-time load into an empty database it is pure overhead. Rejected
  as the default; kept as an opt-in mode.
- **`apoc.periodic.iterate`-style server-side batching**: not available (no
  APOC in the product). Rejected.

## Consequences

- A whole-database pull becomes O(N) with a small constant: the 20k-node
  case is seconds, not minutes; the 330k-node case needs only the response
  pagination companion fix (below).
- **Companion requirement**: the whole-database shape query returns every
  element in one HTTP response and busts the 64 MiB response cap on large
  graphs (fraud-detection, 332,973 nodes). Pull must page the shape query
  (key-range cursor on element id) and materialize each page as a batch.
- Failure reporting changes shape: `LoadStats` gains a `failed` count and
  the CLI reports per-row failures; `--strict` preserves the old
  all-or-nothing behaviour for scripted callers.
- The engine-side fixes (id-lookup pushdown, value-clone, per-statement
  label cost) remain tracked separately; the loader is designed to be
  correct before them and fast after them.
- Tests: batched materialization round-trips (create, reuse, mixed),
  binary-dissection failure injection (one bad row in a batch of 1,000 →
  1 failure, 999 loaded), MERGE-mode idempotency (second load changes
  nothing), and a regression bench
  (`cargo run -p cypherlite-graph-examples --example pull_rca`).

## Later references

- [ADR-0158](./0158-streaming-single-process-pull.md) keeps the batched UNWIND shapes but scopes reuse anchoring (`elementId(r) = row.rid`) to within a single pull's statement list; with the in-memory identity map there is no cross-process reuse anymore.
- [ADR-0171](./0171-lazy-per-label-snapshot-probe.md) resolves the per-statement label cost (cause 1): a lazy per-label probe of `Graph::label_index` replaces the O(N) snapshot label census.
