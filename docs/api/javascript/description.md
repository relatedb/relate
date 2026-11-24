# JavaScript client — description

The `@relate/relate` Node.js package wraps `librelate_ffi`
with koffi on the **typed C ABI v2** (ADR-0502) — `npm install
@relate/relate`. It is for embedding in Node processes, not for
talking Bolt. `bin/fraud-loadtest.js` and `bin/identity-loadtest.js` run
the dataset load-test mix through this client
([`docs/datasets.md`](../../datasets.md)).

Results follow the Neo4j driver's ergonomics (ADR-0216): rows are
objects keyed by column name, `result.stats` carries the 11 mutation
counters, `single()` / `consume()` (a `ResultSummary` with counters,
namespace, query type and engine-reported `elapsedMs`, ADR-0221, with its
`parseMs` / `planMs` / `executeMs` phase split, ADR-0491) and `graph()` (the
result's unique entities as a `ResultGraph`) sit on every result, and
the package ships full TypeScript types plus a bundled library skill for
AI agents.

Every cell decodes through the ABI's typed value walker — one kind per
value, structural at every depth — so integers are lossless beyond 2^53
(LABS-901: a `number` inside the safe window, a `bigint` beyond it),
strings keep interior NULs, durations stay four lossless components, and
the typed classes the values decode to (`CypherDate`, `CypherDuration`,
`CypherPoint`, `CypherVector`, …) bind back as parameters unchanged —
along with `NaN`/±Infinity and full-range `bigint`s, none of which a JSON
encoding can carry. `float(1)` binds a FLOAT-typed 1.0. Every value
carries its own kind — there is no per-column type array.

Failures throw one typed `RelateError` carrying the engine's own
classification — the Neo4j `code`, GQLSTATUS, `stage` and the `retryable`
flag (the retry decision) — never a message sniff. `queryAsync` is the
Promise-returning query form: the call runs on koffi's worker thread, the
error rides the result handle back from that thread, and overlapping
calls on one connection are serialized. Commits
are bookmarked (`"relate:<n>"`, the engine's commit sequence) —
`result.bookmark` for auto-commit, `commit()`'s return value for explicit
transactions.

Connections run in a session namespace (ADR-0117): `db.connect(namespace)`
picks one of the fixed namespaces (`"neo4j"`, `"system"`,
`"system_ontologies"`, case-insensitive) and `conn.setNamespace(name)`
switches it between transactions. Omitting the argument keeps `"neo4j"`.

For callers who prefer interpolation over a separate params object,
`conn.query` doubles as a template tag (ADR-0222):
``conn.query`MATCH (p {name: ${name}}) RETURN p` `` — each interpolated
value becomes an auto-named `$p0` parameter, injection-safe by
construction, with equal primitives de-duplicated to one parameter.

Every query result also reports *its own* statement metadata (ADR-0285):
`result.namespace` is the statement's resolved namespace — the `USE` target
when the statement carries one, else the session namespace — and
`result.queryType` is the plan-time classification as its Neo4j Query API v2
wire code (`"r"`, `"w"`, `"rw"` or `"s"`), so a caller can tell reads from
writes and learn which namespace a `USE`-routed statement actually ran in.

`result.consume().elapsedMs` is the engine-reported wall-clock elapsed
for the statement (ADR-0221), in milliseconds — parse, plan and run,
stamped by the engine rather than measured at the client edge. Its phase
split `parseMs` / `planMs` / `executeMs` (ADR-0491) answers the follow-up
question directly: which of the three phases the time went to (their sum
never exceeds the total).

`result.notifications` is the statement's query-notification list
(ADR-0363) in the engine-native shape: always present, `[]` on a plain
run, populated by `EXPLAIN` and `PROFILE` with entries carrying
the notification code, GQLSTATUS, severity, category, diagnosis and
remedy. `result.consume().notifications` carries the same list.

`conn.query(cypher, params?, txMetadata?)` carries per-transaction
metadata (ADR-0610): the third argument seeds the connection's
metadata map — lowered through the same typed params builder parameters
use — and the next transaction boundary serializes it into the commit's
record, where the value-provenance audit surface (`provenance()`,
`history()`, `CALL db.transactions`) reads it. Later seeds and
`CALL tx.setMetaData` merge in call order; the boundary clears the map
and a rollback discards it. The machinery is the `value-provenance`
preview opt-in's (`RELATE_PREVIEW`, read once at open): a call carrying
the argument on a store opened without it throws the authored preview
error rather than silently dropping the map, and calls without it are
byte-for-byte today's.
