---
name: relate
description: Relate is an embedded openCypher graph database, and this is its Python package (import relate). Use this skill when writing Python code that creates, queries, or embeds a local graph database with Cypher — connecting, parameters, transactions, result handling (records, DataFrames, Arrow, graph extraction), and visualization.
---

# Relate (Python)

Relate embeds a full openCypher engine in-process (Rust core, PyO3
extension; no server, no subprocess). `pip install relate-db` —
`import relate`. Databases are a local directory, `:memory:`, or a
`.rl` file. One writer process per database.

## Connect and query

```python
import relate

db = relate.connect("./mydata")           # or ":memory:" / "./task.rl"
db.execute("CREATE (p:Person {name: $name})", {"name": "Alice"})
for row in db.execute("MATCH (p:Person) RETURN p.name AS name"):
    print(row["name"], row.name)               # dict access and attribute access
db.close()
```

Parameters are `$name`, passed as a plain dict with string keys
(ADR-0219): `uuid.UUID` crosses as its canonical string,
`bytes`/`bytearray` as a list of byte values 0–255, tuples as lists, and
dicts are always maps — a `{"srid": …, "x": …, "y": …}` dict is NOT
silently turned into a point; pass `relate.Point(srid, x, y)` or use
`point($coords)` in Cypher. Non-dict params or non-string keys raise
`relate.ProgrammingError`, and so does a statement that references a
parameter missing from the dict (`ParameterMissing: Expected
parameter(s): <names>`) — pass `None` for null, never omit the key.
Transactions:
`db.autocommit = False`, then `db.commit()` / `db.rollback()` — closing
with an open transaction rolls back. `db.commit()` returns the
transaction's bookmark (`relate:<n>`, the engine's commit sequence,
increasing per store; `None` if nothing was written); an auto-commit
write's bookmark is `cursor.consume().bookmark`. Read-only access (multiple readers
per database, no writes or checkpoints):
`relate.connect(path, read_only=True)` — the store must exist.

## Result handling

Every row is a `Record` — a `dict` with read-only attribute access,
`.data()`, and column-ordered `.keys()`. The cursor carries the Neo4j
driver's conveniences: `single()` (exactly one row or `ResultNotSingleError`),
`peek()`, `fetch(n)`, `data()`, `keys()`, and `consume()` — a
`ResultSummary` with the mutation counters, resolved namespace, query
type, `bookmark` (`relate:<n>` after an auto-commit write, else
`None`), `elapsed_ms` (engine-reported statement elapsed, ADR-0221) with its
`parse_ms`/`plan_ms`/`execute_ms` phase split (ADR-0491 — the PARSE /
PLAN / EXECUTE sub-windows of the total, answering where a slow statement
spends its time), and
`notifications` — the engine-native query-notification list (ADR-0363;
empty on a plain run, populated by `EXPLAIN`/`PROFILE`) — and `plan`, the
structured `relate.plan/v1` document (LABS-370, ADR-0381; `None` on a
plain run): the operator tree with per-node estimates, PROFILE metrics
(including the read-kind mix, the seek/walk phase times, the cumulative
`total_time_ms`, the arm's `loops`, the walk's start, `batched` —
whether the operator ran the batch scan/filter/project chain, and
`zone_skips` — how many conjunct-batch evaluations that chain's filter
provably replaced with the correct empty survivor set via the chunk's
derived per-column min/max zone) and the
operator's remedy-bearing warnings. Reach for `summary.plan` when a query is slow:
the `actual` numbers and the warnings name the operator to fix.
`cursor.description` type codes come from the engine-reported column
types (ADR-0224): `STRING`/`BINARY` map to themselves,
`INTEGER`/`FLOAT`/`BOOLEAN` fold into `NUMBER`, `DATETIME`/`DATE`/`TIME`
fold into `DATETIME`, and graph, spatial, list, map and duration columns
— plus `ANY` (empty or `Null` first row) — report `None`.

```python
cur = db.execute("MATCH (p:Person) RETURN p.name AS name, p.age AS age")
cur.to_arrow()   # pyarrow.Table — pip install relate-db[arrow]
cur.to_df()      # pandas DataFrame — pip install relate-db[pandas]
```

`cur.graph()` returns the result's graph entities — every `Node`,
`Relationship`, `Path` in the rows (nested values walked), de-duplicated
by id — as a `ResultGraph` (`nodes`, `relationships`, id-ordered). Nodes
and relationships expose `id`, `labels`/`type`, `properties`,
`element_id` (exactly Cypher's `elementId()`: `4:rl-id:n0` /
`5:rl-id:r0`, so `WHERE elementId(n) = $id` finds it again),
`items()`, and `start`/`end`; relationships add `start_node_element_id` /
`end_node_element_id`. This is viz-independent:
convert to networkx/igraph without a bridge dependency (see
`references/networkx-igraph.md`), or render with the optional
`relate.viz.to_visualization_graph(cur)` (neo4j-viz).

## The native API

`relate.Database` is the thin native handle under `connect()` — use it
for `execute_arrow()` (zero-copy Arrow IPC bytes), engine lifecycle
(`flush`, explicit transactions), or its `QueryResult` (dict rows
plus `namespace` / `query_type` / `bookmark` / `column_types` /
`notifications` / `plan`).
Everything ergonomic lives on the DB-API side; see
`references/native-api.md` for the comparison.

## Gotchas

- `:memory:` vanishes on close — use a path for durable work.
- One writer per database; multiple read-only processes are fine.
- Connections are thread-affine (`threadsafety` 1): use one only on the
  thread that opened it — any other thread gets a `ProgrammingError` (not
  fatal for the owning thread), and a foreign-thread drop is safe. In
  threaded servers, own the connection on one dedicated worker thread (or
  use `:memory:` per thread).
- Errors carry the engine's classification on every `relate.Error`:
  `e.neo4j_code`, `e.gql_status`, `e.stage` (`compile`/`runtime`/
  `internal`) and `e.retryable` — retry a transaction only when
  `e.retryable` is true (a write-write conflict). All four are `None` on a
  binding-level misuse (closed connection, wrong thread).
- Write counters live on `cursor.stats` (11 counters, zero-padded; Neo4j's
  counting: labels per node, every property write, schema DDL counted).
- Temporal values are `datetime` objects when stdlib carries them losslessly
  (microsecond-aligned, offset only); a value with sub-microsecond nanoseconds
  or an IANA zone name comes back as `relate.Temporal` — read `.text` (the
  engine's canonical ISO text, also `str()`), `.nanosecond`, `.zone_name`,
  `.offset_seconds`, or `.to_native()` for the lossy stdlib value. Nodes come
  back as rich `Node` objects, not dicts.
- Durations are always `relate.Duration` (`.months`, `.days`,
  `.seconds`, `.nanoseconds`; `str()` is the ISO text) — never a
  `timedelta`, so `PT36H != P1DT12H`. `.to_timedelta()` is the explicit
  lossy projection (raises `ValueError` on months or sub-µs). Bind
  `relate.Duration(months=1)` for a month duration.
- Vectors are `relate.Vector` (`.coordinate_type`, `.dimension`,
  `.values`) at every depth, never a dict; a returned one binds back as a
  parameter. Build new ones in Cypher: `vector($list, 3, FLOAT32)`.
- `bytes`/`bytearray` parameters arrive as a `LIST<INTEGER>` (0–255) and
  come back as a `list` of `int`; the engine's real byte array (ADR-0327)
  is Bolt-only and cannot be stored as a property.

Full spec: https://github.com/relatedb/relate — `docs/api/python/`.
