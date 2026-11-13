# The native `Database` API

The package has two entry points. `relate.connect(...)` returns a
DB-API `Connection`/`Cursor` (what the main skill documents, and the right
default). `relate.Database` is the thin native handle underneath it —
reach for it when you want the engine's own result object or Arrow IPC
without decoding through Python rows.

```python
import relate

db = relate.Database(":memory:")          # or a path / a .rl file
result = db.execute("MATCH (p:Person) RETURN p.name AS name")
```

`Database(path, namespace="neo4j", read_only=False)` mirrors `connect()`'s
arguments (keyword-only `read_only`). It also exposes what the DB-API hides:

- lifecycle: `flush()`, `close()`, `reset()`
- namespace: `namespace()`, `set_namespace(name)`
- transactions: `begin_transaction()`, `commit_transaction()` (returns the
  bookmark, `relate:<n>`, or `None` when nothing was written),
  `rollback_transaction()`, `supports_transactions()`, `transaction_active()`
- schema/counts: `labels()`, `relationship_types()`, `node_count()`,
  `relationship_count()`

## `QueryResult`

`Database.execute(query, params)` returns a `QueryResult`:

| Field | Meaning |
| --- | --- |
| `columns` | column names |
| `data` | list of plain dict rows (not `Record`s) |
| `stats` | the 11 mutation counters |
| `namespace` | the statement's resolved namespace (ADR-0285) |
| `query_type` | `r` / `w` / `rw` / `s` (ADR-0162) |
| `bookmark` | `relate:<n>` after an auto-commit write (the engine's commit sequence, increasing per store), else `None` (ADR-0502 §7) |
| `notifications` | query notifications as dicts (ADR-0363): `code`, `gql_status`, `severity`, `category`, `title`, `description`, `remedy`, plus `operator_id` on operator-scoped kinds. Empty on a plain run; populated by `EXPLAIN`/`PROFILE` |

Unlike a `Cursor`, `QueryResult` has no fetch position, no driver
conveniences (`single()`, `to_df()`, `graph()`) — for ergonomics use
`connect()`. Its values are the same rich objects the cursor rows carry
(`Node`, `Relationship`, `Path`, `Point`, `Duration`, `Vector`, …).

Errors raise `RelateNativeError` (a `RuntimeError`) with `.kind`,
`.tck_class` and the engine's `.neo4j_code`, `.gql_status`, `.stage` and
`.retryable` (ADR-0502 §2); `connect()` maps it onto the DB-API tree and
keeps all six attributes.

## Arrow IPC

`Database.execute_arrow(query, params=None)` runs the query and returns
Arrow IPC **bytes** — one serialized conversion from engine rows, the bulk-conversion path to pandas/polars/duckdb:

```python
import pyarrow as pa

buf = db.execute_arrow("MATCH (p:Person) RETURN p.name AS name")
table = pa.ipc.open_stream(buf).read_all()      # needs pyarrow
df = table.to_pandas()                          # needs pandas
```

The DB-API equivalent for materialized results is `cursor.to_arrow()`
(pyarrow `Table` directly) or `cursor.to_df()`. Prefer `execute_arrow()`
when the result is large and you never need Python rows.

## Choosing

| Want | Use |
| --- | --- |
| Records, `single()`, `to_df()`, `graph()`, viz | `connect()` (DB-API) |
| Zero-copy Arrow IPC of a big result | `Database.execute_arrow()` |
| Engine lifecycle (`flush`, explicit transactions) | `Database` |
| Statement metadata (`namespace`, `query_type`, `column_types`, `notifications`) | either; `cursor.consume()` gives namespace/query type/notifications on the DB-API side, `cursor.description` the type codes |
