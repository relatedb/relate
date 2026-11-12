# Python client: relate-db

## Install

```bash
pip install relate-db            # import relate
pip install 'relate-db[arrow]'   # + cursor.to_arrow()
pip install 'relate-db[pandas]'  # + cursor.to_df()
```

Python 3.9+; a PyO3 extension embedding the engine in-process — it does
not wrap the C ABI and needs no native library on a path. Until the first
tagged PyPI publish, build from a checkout with `make install-python`.

## Minimal usage

```python
import relate

conn = relate.connect(":memory:")     # or "./mydata" / "./task.rl"
cur = conn.execute("CREATE (p:Person {name: $name}) RETURN p", {"name": "Ada"})
p = cur.single()["p"]                    # a relate.Node
p.element_id                             # '4:rl-id:n0' == elementId(p)
cur.consume().bookmark                   # 'relate:1' — auto-commit write

conn.autocommit = False                  # explicit transaction
conn.execute("CREATE (:Person {name: 'Bob'})")
conn.commit()                            # 'relate:2' — its bookmark
conn.close()
```

Rows are `Record` dicts; `row.data()` converts nodes, relationships and paths
to plain dicts/lists (properties only, like the Neo4j driver); `row.to_dict()`
is the lossless JSON-ready form (ids, labels, type, endpoint ids, `properties`).
The cursor adds the Neo4j driver's `single()`,
`peek()`, `fetch(n)`, `data()`, `consume()` (a `ResultSummary`: counters,
resolved namespace, query type, bookmark, timings, notifications, plan)
and `graph()`.

## Gotchas

- **Errors carry the engine's classification** — every `relate.Error`
  (and the native `RelateNativeError`) has `neo4j_code`, `gql_status`,
  `stage` (`compile`/`runtime`/`internal`) and `retryable`, from the
  engine's one source. Retry a transaction only when `e.retryable` is true
  (a write-write conflict); all four are `None` on a binding-level misuse
  (closed connection, wrong thread).
- **Durations are `relate.Duration`, never `timedelta`** — months,
  days, seconds and nanoseconds as Cypher keeps them, so `PT36H !=
  P1DT12H`; `.to_timedelta()` is the explicit lossy projection. Bind
  `relate.Duration(months=1)` for a month duration.
- **Vectors are `relate.Vector`** at every depth (`.coordinate_type`,
  `.dimension`, `.values`), never a dict; build new ones in Cypher with
  `vector($list, 3, FLOAT32)`.
- **Element ids are Cypher's `elementId()`** — `4:rl-id:n<id>` /
  `5:rl-id:r<id>`; relationships add `start_node_element_id` /
  `end_node_element_id`. Integer ids stay on `.id` / `.start_node` /
  `.end_node`.
- **Thread-affine connections** (`threadsafety` 1) — use a connection only
  on the thread that opened it; others get `ProgrammingError`.
- **One writer process per database**; `read_only=True` opens are
  unlimited. `:memory:` vanishes on close.
- **Dicts are always maps** — pass `relate.Point(srid, x, y)` for a
  point; a missing `$param` fails the statement, never binds `null`.
- **Library skill in the wheel** — `relate/.agents/skills/relate/SKILL.md`
  ships in the package; `uvx library-skills` links it for agents.

Full contract: `docs/api/python/spec.md` (parameters, returned values,
error classification, bookmarks, threading).
