# Python — spec

Package: `relate-db` on PyPI (import `relate`; PyO3 crate
`relate-python`). Not the deleted ctypes client.

```python
import relate
conn = relate.connect(":memory:")
conn.execute("RETURN 1 AS n")
```

`relate.connect(uri)` and `relate.Database(uri)` open
`relate::Database`. Pass `read_only=True` (`connect`, `Connection`, and
`Database`) to load once and refuse writes and checkpoints (ADR-0136) — a
missing store is an error; the default stays writable and creates a missing
store at open. `close`,
`reset`, `flush`, and dropping a read-only handle skip checkpoint.
Empty directories honor `RELATE_SNAPSHOT_FORMAT`.
See the user book [Python Library](../../reference/python-library.md).

## Parameters (ADR-0219)

`Database.execute` / `execute_arrow` (and the DB-API `Connection.execute` /
`Cursor.execute` / `executemany` layered on them) take `params` as a `dict`
with string keys, referenced as `$name` in the query. Value conversion:

| Python | Cypher value |
| --- | --- |
| `None`, `bool`, `int`, `float`, `str` | `NULL`, `BOOLEAN`, `INTEGER` (64-bit; larger raises), `FLOAT`, `STRING` |
| `datetime.datetime` / `date` / `time` / `timedelta` | the matching temporal value (`timedelta` normalizes hours into days itself: `timedelta(hours=36)` binds as `P1DT12H`) |
| `relate.Duration` | `DURATION`, losslessly — months, days, seconds and nanoseconds as given (ADR-0502 §1) |
| `relate.Vector` (as returned by a query) | `VECTOR`, unchanged |
| `uuid.UUID` | `STRING`, canonical lowercase hyphenated form (CIP-246: UUIDs stay strings on this surface) |
| `bytes`, `bytearray` (incl. the PEP 249 `Binary` constructor) | `LIST<INTEGER>` of byte values 0–255 — the lossless form on this surface (the engine's statement-scoped byte array, ADR-0327, is reached only through Bolt) |
| `list`, `tuple` | `LIST` (nested values convert recursively) |
| `dict` | `MAP` — always; see points below |
| `relate.Point` | `POINT` |

Points are explicit-only: a plain dict with `srid`/`x`/`y` (or
`longitude`/`latitude`) keys is an ordinary `MAP`, never silently
reinterpreted as a point. Build a point with `relate.Point(srid, x, y)`
(the shape returned points already have) or in Cypher via `point($coords)`.

Returned maps keep the engine's key order: the converter walks the engine's
ordered map and inserts into the Python `dict` in that order (dicts are
insertion-ordered).

### Returned durations (ADR-0502 §1)

Every returned duration — at every depth: a column, a list or map element, an
entity property — is a `relate.Duration`, never a `datetime.timedelta`
(which would merge `PT36H` into `P1DT12H`, drop months and floor
nanoseconds) and never a string.

| Member | Meaning |
| --- | --- |
| `Duration(months=0, days=0, seconds=0, nanoseconds=0)` | constructor (every argument an `int`); binds as a parameter without loss |
| `.months` / `.days` / `.seconds` / `.nanoseconds` | the engine's four components, as the engine keeps them (`PT36H` is `seconds=129600`; `P1DT12H` is `days=1, seconds=43200`) |
| `str()` | the engine's ISO-8601 rendering (`P1Y2M3DT4.000000005S`) |
| `==` / `hash()` | the engine's `=`: months and days compare as-is, seconds and nanoseconds as their combined total — `PT36H != P1DT12H`, `Duration(seconds=1, nanoseconds=500_000_000) == Duration(nanoseconds=1_500_000_000)` |
| `.to_timedelta()` | the explicit lossy projection; raises `ValueError` when the duration has months, sub-microsecond nanoseconds, or lies outside `timedelta`'s ±999 999 999-day range |

### Returned vectors (ADR-0502 §1)

Every returned `VECTOR` — at every depth — is a `relate.Vector`, never a
dict:

| Member | Meaning |
| --- | --- |
| `.coordinate_type` | the canonical coordinate-type name: `INTEGER8`, `INTEGER16`, `INTEGER32`, `INTEGER`, `FLOAT16`, `BFLOAT16`, `FLOAT32` or `FLOAT` |
| `.dimension` / `len()` | the coordinate count |
| `.values` | the coordinates as a new list — `int`s for the integer kinds, `float`s otherwise |
| `str()` | the engine's rendering (`vector([1.0, 2.0], 2, FLOAT32 NOT NULL)`) |
| `==` | equal coordinate type and coordinates |
| `hash()` | consistent with `==` — equal vectors hash equal, so a `Vector` works as a dict key or set member |

A returned `Vector` binds back as a parameter unchanged. `Vector` has no
Python constructor: build a new vector in Cypher, e.g.
`vector($values, 3, FLOAT32)` — the engine's constructor owns coordinate
validation (range, precision narrowing).

### Returned temporal values

Temporal results keep the engine's full fidelity. A value arrives as its stdlib
type whenever that type carries it losslessly:

| Engine value | Python |
| --- | --- |
| `DATE` | `datetime.date` |
| `LOCAL_TIME` / `TIME` | `datetime.time` (aware when the value has an offset) |
| `LOCAL_DATETIME` | naive `datetime.datetime` |
| `DATETIME` with an offset only | aware `datetime.datetime` with a fixed offset |
| `DURATION` | `relate.Duration`, always (see above) |

Otherwise — sub-microsecond nanoseconds (stdlib temporals hold whole
microseconds), or an IANA zone name (stdlib `datetime` holds a fixed offset
only) — the value arrives as `relate.Temporal`:

| Member | Meaning |
| --- | --- |
| `.text` | the engine's canonical ISO-8601 rendering, verbatim; also what `str()` returns |
| `.kind` | the engine's kind name: `"datetime"`, `"localdatetime"` or `"time"` |
| `.nanosecond` | sub-second nanoseconds, 0–999 999 999 |
| `.zone_name` | the IANA zone name (e.g. `"Europe/Stockholm"`), or `None` |
| `.offset_seconds` | the UTC offset in seconds, or `None` for an offset-free value |
| `.to_native()` | the stdlib value, floored to microseconds and (for a named zone) reduced to its offset — the lossy projection this type exists to avoid |

`repr()` shows the kind and the canonical text; `==` and `hash()` use the
canonical text, which is the engine's own equality for these kinds (a named
zone never equals the offset it denotes, while `Z` equals `+00:00` — both
render `Z`). `Temporal` is a result type: to pass a temporal as a parameter,
use the stdlib types above.

```python
import relate

conn = relate.connect(":memory:")
row = conn.execute(
    "RETURN datetime('2015-06-01T12:34:56.123456789Z') AS instant,"
    "       datetime('2020-10-25T02:30+02:00[Europe/Stockholm]') AS zoned"
).single()
row["instant"].text        # '2015-06-01T12:34:56.123456789Z'
row["instant"].nanosecond  # 123456789
row["zoned"].zone_name     # 'Europe/Stockholm'
row["zoned"].offset_seconds  # 7200
```

Parameter-shape errors are DB-API-worded and arrive as
`relate.ProgrammingError`: a non-dict `params` raises "query parameters
must be a dict …" and a non-string key (at any nesting level) raises "…
keys must be strings …" naming the offending Python type. An unconvertible
value type is also a `ProgrammingError`; an out-of-range `int` is a
`DataError`.

Every parameter a statement references must be in `params` (`None` is a
valid value). A missing one fails the statement before it runs — it is
never bound as `NULL` — with `ProgrammingError` (native `kind` and
`tck_class` `ParameterMissing`, `neo4j_code`
`Neo.ClientError.Statement.ParameterMissing`, `gql_status` `42N02`) and the
message `semantic error: ParameterMissing: Expected parameter(s): <names>`
(ADR-0308).

## Session namespace (ADR-0117 §4)

`relate.connect(database=":memory:", namespace="neo4j")` and
`relate.Connection(database, namespace=...)` select the session namespace
the connection's statements run in when they carry no `USE` clause:
`"neo4j"` (default), `"system"` or `"system_ontologies"`, matched
case-insensitively. Any other name fails with `OperationalError` whose message
ends with `Graph not found: <name>` (ADR-0146). `system_ontologies` is
visible only under `RELATE_PREVIEW=ontology` (ADR-0593): without the
opt-in, connecting to it or switching to it fails with the same
`Graph not found` rejection, indistinguishable from an unknown name.

- `Connection.namespace` — read-only property returning the current session
  namespace.
- `Connection.set_namespace(name)` — change the session namespace between
  transactions. With a transaction open it fails with
  `Cannot change the namespace while a transaction is open.`
  (`OperationalError`).

The parameter is named `namespace`, not `database`, because `database` already
names the store path.

## QueryResult metadata (ADR-0285)

The native `QueryResult` returned by `Database.execute()` carries these
metadata fields alongside `columns`, `data` and `stats` (namespace and
query type are construction-time stamps; `elapsed_ms` and its phase split
are stamped at the end of execution):

- `QueryResult.namespace` (`str`) — the statement's *resolved* namespace
  (ADR-0117 §7): the `USE` target when the statement carries a `USE` clause,
  else the session namespace. One of `"neo4j"`, `"system"` or
  `"system_ontologies"`. This is per-statement: a `USE`-routed statement
  reports its target without changing the session namespace
  (`Connection.namespace`).
- `QueryResult.query_type` (`str`) — the statement's plan-time classification
  (ADR-0162) as its Neo4j Query API v2 wire code: `"r"` (read), `"w"` (a
  write with no returned columns), `"rw"` (a write with returned columns)
  or `"s"` (schema), the split per ADR-0332. EXPLAIN and PROFILE report
  the wrapped statement's type without executing it.
- `QueryResult.bookmark` (`str | None`) — the statement's bookmark
  (ADR-0502 §7): an auto-commit write's position in the engine's commit
  sequence, spelled `relate:<n>` (ADR-0310), with `n` monotonic per
  store, so two sequential writes return increasing bookmarks. `None` when
  the statement committed nothing: a read, or any statement inside an
  explicit transaction — whose bookmark `Database.commit_transaction()` /
  `Connection.commit()` returns instead.
- `QueryResult.elapsed_ms` (`float`) — the engine-reported wall-clock
  milliseconds for the statement (ADR-0221): parse, plan and run, excluding
  client-side row handling.
- `QueryResult.parse_ms` / `QueryResult.plan_ms` / `QueryResult.execute_ms`
  (`float` each) — the statement's phase split (ADR-0491), three disjoint
  sub-windows of `elapsed_ms`, measured always-on by the engine (no PROFILE
  opt-in): `parse_ms` is the front-end parse and lowering, `plan_ms` the
  binding, logical planning and admission classification, `execute_ms` the
  dispatch — or the EXPLAIN plan render for an `EXPLAIN` statement. Their
  sum never exceeds `elapsed_ms` (the inter-stage gaps stay in the total
  alone); the total keeps its ADR-0221 meaning.
- `QueryResult.column_types` (`list[str]`) — the engine-reported per-column
  type names (ADR-0224): one of the 18 canonical names (`STRING`, `INTEGER`,
  `FLOAT`, `BOOLEAN`, `NODE`, `RELATIONSHIP`, `PATH`, `LIST`, `MAP`, `POINT`,
  `DATETIME`, `DATE`, `TIME`, `DURATION`, `VECTOR`, `UUID`, `BINARY`, `ANY`),
  same length and order as `columns`, derived from the FIRST row; a column
  whose first-row value is `Null`, or a result with no rows, stamps `ANY`.
  The name describes the first row only — Cypher columns are heterogeneous.
  **Byte arrays (ADR-0327).** The engine's statement-scoped byte array enters only through Bolt or a Rust `Value::Bytes` parameter, never through this surface's own parameters. A result value holding one comes back as a `list` of `int` 0–255 (the `value_to_cypher` crossing) and its column stamps `BINARY`; byte arrays cannot be stored as properties.
- `QueryResult.notifications` (`list[dict]`) — the query notifications
  derived with the plan tree (ADR-0363): `EXPLAIN` and `PROFILE` populate
  the list, a plain run leaves it empty. Each entry carries the
  engine-native keys `code`, `gql_status`, `severity`, `category`,
  `title`, `description` and `remedy`, plus `operator_id` (the raising
  plan operator's id) on operator-scoped kinds. The list is deterministic:
  operator-scoped entries follow the plan's pre-order and identical
  `(kind, description)` pairs collapse to the first. There is no `position`
  key in this form.
- `QueryResult.plan` (`dict | None`) — the statement's structured plan as a
  `relate.plan/v1` document (LABS-370, ADR-0381), or `None` when the
  statement carried no plan (a plain run). The document is
  `{schema, profiled, statement, root}`: `schema` is the literal
  `"relate.plan/v1"`, `profiled` says whether the tree carries runtime
  metrics, `statement` is the echoed statement text (the `EXPLAIN`/`PROFILE`
  preamble removed), and `root` is the operator tree — each node
  `{operator, details, identifiers, estimated_rows, actual, children,
  warnings}`. `actual` is `None` on an `EXPLAIN` tree and otherwise
  `{rows, time_ms, total_time_ms, db_hits, memory_bytes, expression_built_bytes,
  hops, neighbor_walks, node_reads, seek_ms, walk_ms, loops, batched,
  zone_skips, start}`
  (`batched` says whether the operator ran the ADR-0468 batch
  scan/filter/project chain — `False` on every operator the chain
  declined; `zone_skips` counts that chain's filter's zone skips —
  conjunct-batch evaluations the chunk's derived per-column min/max
  provably replaced with the correct empty survivor set, `0` when it
  skipped nothing) where
  `start` is `{element, backward, reason}` (the walk's chosen
  start element index, its direction, and why: `bound-pivot`, `cheapest`,
  `seek-anchored` or `index-0`). `seek_ms`/`walk_ms` are the phase times
  (the candidate resolution and the relationship traversal each spent,
  inclusive of a multi-element pattern's nested phases — zero when the
  clause does no pattern work); `loops` is the input clause's rows (the
  operator ran once per input row; `None` at the root, the seed row). `warnings` are the engine's
  remedy-bearing notifications (ADR-0363) for that operator — the same
  entries as `QueryResult.notifications`, matched by operator id — so a
  warning carries its `code`, `severity` and `remedy` beside the operator
  that raised it. Every field is always present; an absent fact is `None`.

`Cursor.description`'s `type_code` comes from those engine-stamped names
(ADR-0224), mapped onto the PEP 249 type-comparison objects:
`STRING` → `relate.STRING`, `BINARY` → `relate.BINARY`,
`INTEGER`/`FLOAT`/`BOOLEAN` → `relate.NUMBER`, and
`DATETIME`/`DATE`/`TIME` → `relate.DATETIME`. Every other canonical
name — graph entities (`NODE`, `RELATIONSHIP`, `PATH`), spatial, list, map
and duration types, and `ANY` (the empty-result / `Null` stamp) — has no
DB-API counterpart and reports `None`. A pre-filled cursor (one constructed
directly with `columns`/`rows`, no statement executed) carries no engine
types and reports `None` for every column.

## Result rows and driver-style conveniences (ADR-0213)

Every row a `Cursor` yields — `fetchone` / `fetchmany` / `fetchall` /
iteration — is a `relate.Record`: a `dict` subclass, so the DB-API
contract (rows are mappings) is unchanged, with the Neo4j driver's row
conveniences on top: attribute access (`row.name`), `row.keys()` (column
order, not insertion order; a copy — mutating it never touches the row),
and `row.data()` (a plain-data copy, converted all the way down — see
below). Attribute access is read-only:
`row.name = x` raises `AttributeError`, like the driver's `Record`;
`row["name"] = x` still works because a `Record` is a `dict`.

`row.data()` follows the Neo4j driver's `Record.data()`: a `Node` becomes the
`dict` of its properties, a `Relationship` likewise, a `Path` a list that
alternates node property dicts with relationship types
(`[{"name": "a"}, "KNOWS", {"name": "b"}]`), and lists and maps are converted
recursively. Ids, labels and endpoints are not part of that form: use
`to_dict()` (below) to keep them. `Node.data()`, `Relationship.data()` and `Path.data()`
return the same forms on their own (a relationship alone yields only its
properties: it holds endpoint ids, not endpoint nodes). `Point`, `Duration`,
temporal and `Vector` values are returned unchanged. `data()` always returns
a copy, so mutating it never touches the row.

`row.to_dict()` is the lossless, JSON-ready counterpart (Relate-specific, not
in the driver): same recursion, but every graph value keeps its structure.

| Value | `to_dict()` shape |
| --- | --- |
| `Node` | `{"id", "element_id", "labels", "properties"}` |
| `Relationship` | `{"id", "element_id", "type", "start_node", "end_node", "start_node_element_id", "end_node_element_id", "properties"}` |
| `Path` | `{"nodes": [<node dict>, …], "relationships": [<relationship dict>, …]}` |

`dict(node)` and `dict(relationship)` (also `**node`) give just the property
dict, as in the Neo4j driver: a `Node` and a `Relationship` expose `keys()`,
`items()` and `[]` over their properties.

`Node.to_dict()`, `Relationship.to_dict()` and `Path.to_dict()` return the
same shapes on their own. Properties always sit under `"properties"`, so a
property named `id` or `type` never collides. `Point`, `Duration`, temporal
and `Vector` values are returned unchanged; every call returns a copy.

Cursor-level conveniences (same semantics as the driver's `Result`):

| Method | Behavior |
| --- | --- |
| `cursor.data()` | all remaining rows as `Record`s (consumes) |
| `cursor.fetch(n)` | alias of `fetchmany(n)` |
| `cursor.peek()` | the next row without advancing; `None` at the end |
| `cursor.single()` | the one remaining row; raises `ResultNotSingleError` (a `ProgrammingError`) on zero or more than one |
| `cursor.keys()` | alias of `.columns` |
| `cursor.to_df()` | remaining rows as a `pandas.DataFrame` (consumes); pandas is an optional extra — `pip install relate-db[pandas]` — and a missing install raises `ImportError` with that hint, like `relate.viz` |
| `cursor.to_arrow()` | remaining rows as a `pyarrow.Table` (consumes); pyarrow is an optional extra — `pip install relate-db[arrow]`; `.to_pandas()` and `pl.from_arrow()` both work on the result, so it is the dependency-light bulk-conversion path |
| `cursor.graph()` | the result's unique graph entities as a `ResultGraph` (ADR-0214) |
| `cursor.consume()` | discards remaining rows and returns a `ResultSummary` (ADR-0215) |

`to_df()` builds the frame from the already-materialized rows with pandas'
own from-rows constructor; `to_arrow()` does the same through
`pyarrow.Table.from_pylist` (an empty result keeps the column shape with
null-typed columns). Neither re-runs the query. For a single bulk conversion
of a large result use the native `Database.execute_arrow()` instead — it
returns Arrow IPC stream bytes (one serialized conversion from engine rows).

The package ships `py.typed` (PEP 561): type checkers and IDEs see the
binding's annotations in the installed wheel. It also bundles a library
skill ([library-skills.io](https://library-skills.io/)) at
`relate/.agents/skills/relate/SKILL.md` — an Agent Skills-format
guide to the package that `uvx library-skills` installs as a symlink for
AI agents; it rides along in the wheel and stays version-synced with the
package.

See [`examples/driver_ergonomics.py`](examples/driver_ergonomics.py).

## Result graph extraction (ADR-0214)

`cursor.graph()` returns a `relate.ResultGraph`:

- `nodes` — the unique `Node` entities in the result, ordered by id (immutable tuple)
- `relationships` — the unique `Relationship` entities, ordered by id (immutable tuple)
- `len(graph)` — node + relationship count

Every `Node`, `Relationship` and `Path` the rows carry — as a column value
or nested in a list, tuple, set or map — is collected, de-duplicated by
entity id (first occurrence wins). A path contributes all of its nodes and
relationships. Extraction walks **all** rows of the result: rows are
materialized at execute time, so the fetch position is irrelevant (the
driver's `Result.graph()` builds from consumed records — there is nothing
to consume here). A result with no graph entities returns an empty
`ResultGraph`, not an error. The same walk backs
`relate.viz.to_visualization_graph`, so the two cannot drift.

`Node` and `Relationship` also expose driver-parity members for
neo4j-viz-style consumers: `element_id` and `items()` (the property pairs).
`element_id` is exactly what Cypher `elementId()` returns for the entity
(ADR-0502 §6) — `4:rl-id:n<id>` for a node, `5:rl-id:r<id>` for a
relationship in the `neo4j` namespace (`4:rl-onto:n<id>`
in `system_ontologies`; the result's resolved namespace decides) — so
`WHERE elementId(n) = $id` round-trips and node 0 never collides with
relationship 0. A relationship also carries `start_node_element_id` /
`end_node_element_id`, its endpoints' `elementId()`. All three are always
strings: a namespace whose elements have no element ids (`system`) fails
the conversion with `RuntimeError: elements of this namespace have no
element ids` — the same refusal Cypher's `elementId()` itself gives —
rather than returning `None`. `Node.id`, `Relationship.id` and
`Relationship.start_node`/`end_node` remain the integer ids, with `r.start`
/ `r.end` as short aliases for edge-list-style consumers.

See [`examples/result_graph.py`](examples/result_graph.py).

## Result summary (ADR-0215)

`cursor.consume()` discards any remaining rows and returns a
`relate.ResultSummary`: `counters` (the 11 mutation counters, as on
`.stats`, with Neo4j's counting, ADR-0309: labels per node, one
`properties_set` per property write, schema DDL in the index and
constraint counters), `namespace` (resolved, ADR-0285), `query_type` (`r`/`w`/`rw`/`s`),
`elapsed_ms` — the engine-reported wall-clock milliseconds for the
statement (ADR-0221): parse, plan and run, stamped once in the engine and
excluding client-side row handling — with its phase split
`parse_ms`/`plan_ms`/`execute_ms` (ADR-0491: the PARSE / PLAN / EXECUTE
sub-windows of the total, sum never exceeding `elapsed_ms`) — and
`notifications`, the same
engine-native list the native `QueryResult` carries (ADR-0363; empty on a
plain run, populated by `EXPLAIN`/`PROFILE`), and `plan`, the same
`relate.plan/v1` document the native `QueryResult.plan` carries
(`None` on a plain run), and `bookmark`, the same value as the native
`QueryResult.bookmark` (`relate:<n>` after an auto-commit write, else
`None`).
`consume()` is idempotent (a second call returns an equal summary).
`executemany` reports the final execution's summary.

The summary's properties are read-only. Its constructor is keyword-only —
every field above is passed by name (`counters=…`, `bookmark=…`), never as
a positional list; `consume()` is the usual source of a summary. The DB-API
`Cursor` does not
surface the native `QueryResult` itself; the summary carries its
statement metadata.

## Threading

`relate.threadsafety` is 1: the module may be shared between threads, a
connection may not. A `Connection` (and the native `Database` it wraps) is
**thread-affine**: the opening thread is recorded and every engine access from
any other thread raises `ProgrammingError` (`kind = "WrongThread"`) — the
`sqlite3` behavior, catchable by `except Exception` / `except relate.Error`
where the previous design leaked a pyo3 `PanicException` (GH #352 / LABS-700).
The refusal is not fatal: the owning thread keeps the connection usable.

Dropping the last reference on a foreign thread is safe. `close()` refuses on a
foreign thread with the same typed error, and the deferred cleanup — the native
`Drop`-time checkpoint — then runs from that thread; no unraisable exception is
emitted.

The GIL is released around the engine call (`execute` / `execute_arrow`), so
other Python threads make progress while a query runs; the constraint is
affinity, not serialization. One connection per thread works for `:memory:`; a
second writable open of a file-backed store is refused by the writer lease
(ADR-0136), so a shared file store means one owning connection per process (or
a read-only handle beside a writer).

## Transactions and bookmarks (ADR-0502 §7)

Connections default to autocommit. With `connection.autocommit = False`, the
first statement opens a transaction and `Connection.commit()` ends it,
returning the transaction's bookmark — `relate:<n>`, the engine's commit
sequence, monotonic per store — or `None` when nothing was committed
(autocommit mode, no open transaction, or a transaction that wrote nothing).
The native `Database.commit_transaction()` returns the same value. An
auto-commit write's bookmark is on its result (`QueryResult.bookmark`,
`cursor.consume().bookmark`).

## Error classification (ADR-0502 §2)

Every engine failure carries the engine's own classification, from the one
source `relate::error_info` — the binding holds no kind-to-code table.
The attributes sit on the native `RelateNativeError` and are copied
verbatim onto the DB-API exception it maps to (`relate.Error` and every
subclass declare them):

| Attribute | Meaning |
| --- | --- |
| `kind` / `tck_class` | the binding vocabulary the DB-API mapping reads (below) |
| `neo4j_code` | the Neo4j status code, e.g. `Neo.ClientError.Statement.SyntaxError` (an internal failure carries the genericized `UnknownError` code, ADR-0260) |
| `gql_status` | the GQLSTATUS, e.g. `42N02` |
| `stage` | `"compile"` (the statement was rejected before any work), `"runtime"` (discovered while producing rows) or `"internal"` (not the statement's fault), ADR-0319 |
| `retryable` | `True` exactly when retrying the transaction can succeed — a write-write conflict, a retryable processing cap; `False` otherwise |

All four engine facts are `None` on a failure the engine never classified: a
binding-level refusal (`WrongThread`, `ConnectionClosed`, an Arrow
conversion `InternalError`) or an error the Python layer raises itself (a
closed connection, a parameter-shape mistake).

```python
try:
    conn.execute("RETURN 1 +")
except relate.ProgrammingError as e:
    e.neo4j_code  # 'Neo.ClientError.Statement.SyntaxError'
    e.stage       # 'compile'
    e.retryable   # False
```

## Error mapping for namespace errors

The fixed-namespace error kinds map onto the DB-API tree as follows
(`relate.exceptions`):

| Native `kind` | DB-API class |
| --- | --- |
| `DatabaseNotFound` | `OperationalError` |
| `WriteOnReadOnlyAccessDatabase` | `OperationalError` |
| `UnsupportedAdministrationCommand` | `NotSupportedError` |
| `TransactionError` | `OperationalError` |

The binding-level thread-affinity refusal carries `kind = "WrongThread"` (no
TCK class) and maps to `ProgrammingError`.

The capacity family — the `--logical-graph-size-limit` graph cap, the
per-allocation expression cap (ADR-0463), and the per-record 64 MiB WAL
payload cap (ADR-0494) — carries `kind = "MemoryLimitExceeded"` and
`tck_class = "ArgumentError"` and maps to `DataError`, the same DB-API
class for every capacity refusal. The WAL payload cap moved from the
`StorageError` kind to this memory-limit kind in ADR-0494, while no
external clients existed; its message names the WAL record and the cap.

## Visualization (`relate.viz`, optional)

`relate.viz.to_visualization_graph(result)` turns one query result into a `neo4j_viz.VisualizationGraph` for interactive rendering ([neo4j-viz docs](https://neo4j.com/docs/python-graph-visualization/current/)). `result` is anything whose rows are mappings: a DB-API `Cursor` from `connect()` (`fetchall()`), a native `QueryResult` (`.data`), or a plain iterable of dicts.

`neo4j_viz.neo4j.from_neo4j` accepts only real `neo4j` driver objects (`isinstance` checks), so a Relate cursor cannot be passed there directly; `to_visualization_graph` above is the supported path.

Every `Node`, `Relationship` and `Path` the result carries — as a top-level column value or nested inside a list or map — becomes part of the visualization; a path contributes all of its nodes and relationships, and entities are deduplicated by id. Following neo4j-viz's own Neo4j-driver integration:

- node captions default to the first of the `name` / `title` / `display_name` properties a node carries, falling back to `":".join(labels)`; pass `node_caption={"Question": "title", "Tag": "name", ...}` to map a label to its caption property (missing properties fall back per node; values are stringified);
- relationship captions stay the relationship `type`;
- every entity property is kept under `properties`, plus a `labels` entry on nodes and a `type` entry on relationships;
- nodes are colored by their labels with the Neo4j discrete palette when there are at most 12 distinct label sets.

**neo4j-viz is an optional dependency**, imported lazily by the call. When it is missing, the call raises `ImportError` with install instructions (`pip install neo4j-viz` — there is no `[notebook]` extra; the Jupyter deps are core since 1.11); nothing else in the package requires it. A result with no graph entities at all raises `ValueError` naming the expected shapes (`MATCH (a)-[r]->(b) RETURN a, r, b`).

The returned `VisualizationGraph` is styled and displayed with neo4j-viz's own methods: `render()` returns a standalone HTML document (right for exports and static viewers), `render_widget()` returns an interactive anywidget (right for JupyterLab, Notebook 7, VS Code and Colab; the VS Code and PyCharm widget rendering, and the Marimo page-restyle fix, landed in neo4j-viz 1.11.0). Use `neo4j-viz>=1.12.0` — 1.12 supports Python 3.15 and drops 3.10 (`Requires-Python >=3.11`); on 3.10 pip resolves 1.11, which this module also supports. `render()`'s standalone document does not paint inside JupyterLab's sandboxed HTML output iframe — in Jupyter, use `render_widget()`.

`relate.viz.render_html(vg, **kwargs)` is the multiple-renders-on-one-page fix for the standalone HTML: neo4j-viz's template ships each graph's data in a `<script id="neo4j-viz-data">` block and reads it back with `document.getElementById` — a page-global fixed id, so two `render()` outputs on one page (two notebook cells) both paint the first cell's graph. `render_html` renames the id and its lookup together per call, so every cell paints its own graph. Keyword arguments pass through to `vg.render()` (`initial_zoom`, `width`, `height`, ...).

## Examples

Runnable examples live in [`examples/`](examples/):
[`create_match.py`](examples/create_match.py),
[`parameters.py`](examples/parameters.py) (uuid, bytes, tuple, explicit
points, parameter errors),
[`transaction.py`](examples/transaction.py),
[`snapshot_json.py`](examples/snapshot_json.py),
[`namespace.py`](examples/namespace.py) (session namespace selection),
[`thread_affinity.py`](examples/thread_affinity.py) (thread affinity: the
typed cross-thread refusal, per-thread connections, foreign-thread drop),
[`result_metadata.py`](examples/result_metadata.py) (QueryResult `namespace` /
`query_type` and the query `notifications` list),
[`result_graph.py`](examples/result_graph.py) (`cursor.graph()` extraction),
[`engine_facts.py`](examples/engine_facts.py) (error classification,
bookmarks, lossless `Duration`, typed `Vector`, `elementId()` element ids),
[`temporal_fidelity.py`](examples/temporal_fidelity.py) (nanoseconds and
IANA zone names) and
[`visualization.py`](examples/visualization.py) (building a
`VisualizationGraph` from two queries; needs `neo4j-viz`).
