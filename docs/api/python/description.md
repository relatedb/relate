# Python — description

The product Python binding is the native extension in `surfaces/relate-python`.
Wheels publish to GitHub Releases and to PyPI as `relate-db` on tagged
releases when trusted publishing is configured (`import relate`).

The DB-API connection selects its session namespace at connect time
(`relate.connect(database=":memory:", namespace="neo4j")`) and switches it
between transactions with `Connection.set_namespace(name)`; the fixed
namespaces are `neo4j`, `system` and `system_ontologies` (ADR-0117 §4).

Every native `QueryResult` also reports *its own* statement metadata
(ADR-0285): `result.namespace` is the statement's resolved namespace — the
`USE` target when the statement carries one, else the session namespace — and
`result.query_type` is the plan-time classification as its Neo4j Query API v2
wire code (`"r"`, `"w"`, `"rw"` or `"s"`), so a caller can tell reads from
writes and learn which namespace a `USE`-routed statement actually ran in.
Every result also carries `column_types` (ADR-0224) — one canonical
engine type name per column, stamped from the first row — which is the
source for `Cursor.description`'s PEP 249 `type_code` (no client-side type
inference: `STRING`/`BINARY` map to themselves, `INTEGER`/`FLOAT`/`BOOLEAN`
fold into `NUMBER`, `DATETIME`/`DATE`/`TIME` fold into `DATETIME`, and the
graph/spatial/list/map/duration names and `ANY` report `None`). And it
carries `notifications` (ADR-0363): the plan-time query-notification list,
empty on a plain run and populated by `EXPLAIN`/`PROFILE`, in the
engine-native shape (`code`, `gql_status`, `severity`, `category`, `title`,
`description`, `remedy`, plus `operator_id` on operator-scoped kinds).

Result rows are `relate.Record`s — `dict`s with driver-style conveniences
(attribute access, `.data()`, `.keys()` in column order) — and cursors carry
the Neo4j driver's `Result` conveniences: `data()`, `fetch(n)`, `peek()`,
`single()` (raising `ResultNotSingleError`), `keys()`, `to_df()` for a
pandas DataFrame (optional `relate-db[pandas]` extra, ADR-0213) and
`to_arrow()` for a `pyarrow.Table` (optional `relate-db[arrow]`
extra — the dependency-light bulk path; `.to_pandas()` and
`pl.from_arrow()` both work on it). The wheel ships `py.typed`, so type
checkers and IDEs see the annotations.

`cursor.consume()` (ADR-0215) returns a `ResultSummary` — counters,
resolved namespace, query type, `elapsed_ms` — engine-reported
wall-clock milliseconds for the statement (ADR-0221) — its
`parse_ms`/`plan_ms`/`execute_ms` phase split (ADR-0491), which answers
*where* a slow statement spends its time, and
`notifications` (ADR-0363). `cursor.graph()` (ADR-0214) extracts the result's graph entities — every
`Node`, `Relationship` and `Path` the rows carry, de-duplicated by id — as
a `relate.ResultGraph` (`nodes` / `relationships`, id-ordered). `Node`
and `Relationship` expose the driver-parity `element_id` and `items()`
members, so neo4j-viz-style consumers can map them directly. The
`element_id` is exactly Cypher's `elementId()` (`4:rl-id:n0`,
`5:rl-id:r0`), relationships carry their endpoints' too, so an id read
from a result finds the same entity again with `WHERE elementId(n) = $id`.

The binding carries the engine's facts rather than re-deriving them
(ADR-0502). Every error — the native `RelateNativeError` and the DB-API
exception it maps to — carries the engine's `neo4j_code`, `gql_status`,
`stage` and `retryable` flag, so a caller (or a driver-style retry loop) can
tell a retryable write-write conflict from a syntax error without parsing
messages. Writes return bookmarks: an auto-commit write's
`cursor.consume().bookmark` and an explicit transaction's
`Connection.commit()` are the engine's commit sequence (`relate:<n>`,
increasing per store). Durations arrive as `relate.Duration` — months,
days, seconds and nanoseconds, exactly as Cypher keeps them, so `PT36H` and
`P1DT12H` stay different values — and vectors as `relate.Vector`, at
every depth, both binding back as parameters without loss.

Temporal results keep the engine's precision. Values that a stdlib type carries
losslessly still arrive as one (`datetime.date`, naive/aware `datetime`); a
value with sub-microsecond
nanoseconds or an IANA zone name arrives as `relate.Temporal`, which
carries the engine's canonical rendering (`.text`, also `str()`), the
`.nanosecond`, `.zone_name` and `.offset_seconds` the stdlib type drops, and a
`.to_native()` escape hatch that returns the lossy stdlib equivalent. So a
command-line user and a Python user see the same instant, to the nanosecond and
with the same zone.

For quick visual exploration, the optional `relate.viz` module builds a
`neo4j_viz.VisualizationGraph` directly from a query result's graph elements —
nodes, relationships, and paths (`to_visualization_graph`); neo4j-viz itself
stays an optional dependency, with a helpful `ImportError` when it is absent.

Query parameters (ADR-0219) cover the Python types an application actually
holds: beyond the scalars, temporals, lists and nested dicts, `uuid.UUID`
crosses as its canonical string, `bytes`/`bytearray` (the PEP 249 `Binary`
constructor) cross as a list of byte values, tuples convert like lists, and
returned maps keep their key order. Points are explicit-only — a plain dict
with `srid` and coordinate keys is an ordinary map, never silently
reinterpreted; build a point with `relate.Point(...)` or `point($coords)`.
Parameter-shape mistakes (a non-dict `params`, a non-string key) raise
`relate.ProgrammingError` with wording that says so.
