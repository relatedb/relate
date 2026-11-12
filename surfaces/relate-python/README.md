# relate-db — Relate for Python

**An embedded graph database for Python. Query it in Cypher, in-process — no server, no network, nothing to deploy.**

Relate is an in-memory property-graph engine written in Rust. This package
embeds it as a native extension (`import relate`): queries run in your
process, on a graph that is pure in-memory, a directory, or a single `.rl`
file, made durable by a fsync'd write-ahead log and Parquet checkpoints.

> An experimental Neo4j Labs project, not part of Neo4j's supported product
> lineup. The PyPI name `relate` belongs to another project; this package is
> `relate-db`, and the import is `relate`.

## Install

```bash
pip install relate-db
```

Optional extras — each is only needed for the matching result helper:

```bash
pip install "relate-db[pandas]"   # cursor.to_df()
pip install "relate-db[arrow]"    # cursor.to_arrow() (pyarrow)
```

Wheels are published for Linux (x86-64, aarch64), macOS (arm64) and Windows
(x86-64); Python 3.9+ (one `abi3` wheel per platform). No Rust toolchain
needed.

## Quick start

```python
import relate

conn = relate.connect(":memory:")          # or a path: relate.connect("./friends")

conn.execute("CREATE (:Person {name: $name, age: $age})", {"name": "Alice", "age": 30})
conn.execute("CREATE (:Person {name: $name, age: $age})", {"name": "Bob", "age": 28})
conn.execute(
    "MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}) "
    "CREATE (a)-[:FRIENDS_WITH {since: 2020}]->(b)"
)

cursor = conn.execute(
    "MATCH (p:Person)-[:FRIENDS_WITH]->(f) RETURN p.name AS person, f.name AS friend"
)
for row in cursor:
    print(row["person"], "->", row.friend)   # rows are dicts with attribute access

conn.close()
```

`relate.connect()` is a sqlite3-style, DB-API 2.0 flavoured connection
(`execute`, `executemany`, cursors, `commit`/`rollback`, context manager).
The native `relate.Database` is the direct engine handle, with Arrow IPC via
`execute_arrow()`.

## Persistence

| Store | Open with |
| --- | --- |
| In-memory — gone at exit | `relate.connect(":memory:")` |
| Directory — WAL + Parquet shards (default) | `relate.connect("./friends")` |
| Single file — WAL, snapshot and catalog in one `.rl` file | `relate.connect("./friends.rl")` |
| Read-only — load once, refuse writes and checkpoints | `relate.connect("./friends", read_only=True)` |

A second *writable* open of the same store is refused (single-writer lease);
use `read_only=True` for readers.

## Parameters

Pass a `dict` and reference values as `$name`. Python types map the way you
would expect: `None`, `bool`, `int` (64-bit), `float`, `str`, `list`/`tuple`,
nested `dict`, `datetime`/`date`/`time`/`timedelta`, `uuid.UUID` (as its
string) and `bytes` (as a list of byte values). Relate adds `relate.Duration`,
`relate.Vector` and `relate.Point` for values stdlib types cannot carry
without loss. A plain dict with `srid`/`x`/`y` keys stays an ordinary map —
points are explicit.

```python
conn.execute("UNWIND $rows AS r CREATE (:Item {id: r.id, tags: r.tags})",
             {"rows": [{"id": 1, "tags": ["a", "b"]}, {"id": 2, "tags": []}]})
```

## Results

Rows are `relate.Record`s — a `dict` with driver-style conveniences:

```python
row = conn.execute("MATCH (p:Person) RETURN p.name AS name, p.age AS age ORDER BY name").fetchone()
row["name"] == row.name == "Alice"
row.data()      # {"name": "Alice", "age": 30} — nodes, relationships and
                # paths inside the row become plain dicts/lists, like the Neo4j driver
row.to_dict()   # like data(), but nodes/relationships/paths keep ids, labels,
                # endpoints and a "properties" dict — JSON-ready
row.keys()      # ["name", "age"]
```

On the cursor: `fetchone` / `fetchmany` / `fetchall`, `data()`, `peek()`,
`single()` (exactly one row or `ResultNotSingleError`), `keys()`, plus
`to_df()` (pandas) and `to_arrow()` (pyarrow, the fast bulk path).

```python
n = conn.execute("MATCH (p:Person) RETURN count(p) AS n").single().n

summary = conn.execute("MATCH (p:Person) RETURN p").consume()
summary.counters, summary.query_type, summary.namespace
summary.elapsed_ms, summary.parse_ms, summary.plan_ms, summary.execute_ms

graph = conn.execute("MATCH (a)-[r]->(b) RETURN a, r, b").graph()
graph.nodes, graph.relationships          # de-duplicated, id-ordered
```

Temporal results come back as stdlib types when that is lossless and as
`relate.Temporal` (nanoseconds, IANA zone) when it is not; durations are
always `relate.Duration`. Node and relationship `element_id` equals Cypher's
`elementId()`, so `WHERE elementId(n) = $id` finds the entity again. For a
quick picture, `relate.viz.to_visualization_graph(...)` builds a
[neo4j-viz](https://pypi.org/project/neo4j-viz/) graph.

## Transactions

Autocommit by default: a statement is durable when `execute()` returns. Turn
it off to group statements.

```python
conn.autocommit = False
try:
    conn.execute("CREATE (:Person {name: 'Carol'})")
    conn.execute("CREATE (:Person {name: 'Dan'})")
    bookmark = conn.commit()      # "relate:<n>", the engine's commit sequence
except Exception:
    conn.rollback()
    raise
```

Closing a connection rolls back an open transaction.

## Errors

Every error inherits `relate.RelateError` (PEP 249 hierarchy:
`OperationalError`, `ProgrammingError`, `DataError`, …) and carries the
engine's own classification — `neo4j_code`, `gql_status`, `stage` and a
`retryable` flag — so a retry loop never parses messages:

```python
try:
    conn.execute("MATCH (n RETURN n")
except relate.RelateError as e:
    print(e.neo4j_code, e.retryable)
```

## Threads

`relate.threadsafety` is 1: share the module, not a connection. A
connection must be used (and dropped) on the thread that opened it; using it
elsewhere raises `ProgrammingError` and leaves the owner thread's connection
intact. In a threaded server, own the connection on one worker thread and
submit work to it.

## Bigger loads

Send rows in batches with `UNWIND $rows`, a few tens of thousands per
statement, rather than one huge statement or one statement per row.

## Serving it over the network

The same engine runs as a server. Start `relate serve --bolt 7687` (see the
[CLI](https://github.com/relatedb/relate/blob/main/docs/api/cli/spec.md))
and use the stock `neo4j` Python driver against it.

## Documentation

- [Python library reference](https://github.com/relatedb/relate/blob/main/docs/reference/python-library.md) — every method and property
- [API spec](https://github.com/relatedb/relate/blob/main/docs/api/python/spec.md) and [runnable examples](https://github.com/relatedb/relate/tree/main/docs/api/python/examples)
- [Project home](https://github.com/relatedb/relate) · [relatedb.ai](https://relatedb.ai/) · [known limitations](https://github.com/relatedb/relate/blob/main/docs/limitations.md)

The wheel also bundles an agent skill (`.agents/skills/relate/SKILL.md`,
[library-skills](https://library-skills.io)).

## Development

```bash
pip install maturin
cd surfaces/relate-python && maturin develop --release
```

License: Apache-2.0.
