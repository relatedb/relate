---
name: relate
description: Relate is an embedded openCypher graph database, and this is its Node.js/TypeScript package. Use this skill when writing JavaScript or TypeScript that creates, queries, or embeds a local graph database with Cypher — connecting, parameters, transactions, result handling (rows, counters, summaries, graph extraction), and read-only access.
---

# Relate (Node.js / TypeScript)

Relate embeds a full openCypher engine in-process (Rust core over the
typed C ABI v2; no server, no subprocess). `npm install
@relate/relate` — `require("@relate/relate")` /
`import { Database } from "@relate/relate"`. `query` is
synchronous (the engine runs in-process); `queryAsync` is the
Promise-returning form. Databases are a local directory, `":memory:"`, or
a `.rl` file. One writer process per database.

## Connect and query

```js
const { Database } = require("@relate/relate");

const db = Database.open("./mydata");        // or ":memory:" / "./task.rl"
const conn = db.connect();                   // optional namespace: "system"...
const result = conn.query("CREATE (p:Person {name: $name})", { name: "Alice" });
const read = conn.query("MATCH (p:Person) RETURN p.name AS name");
for (const row of read.rows) console.log(row.name);
```

Parameters are `$name`, passed as a plain object lowered into the typed
params builder. Values: `null`, booleans, strings, numbers
(integer-valued binds INTEGER, otherwise FLOAT — `NaN`/`±Infinity` bind),
full-range `bigint`s, arrays, plain objects, and the typed classes
results decode to (`CypherDate`, `CypherTime`, `CypherLocalTime`,
`CypherDateTime`, `CypherLocalDateTime`, `CypherDuration`, `CypherPoint`,
`CypherVector`) — a decoded value binds back unchanged. `float(1)` binds
a FLOAT-typed 1.0. Transactions: `conn.begin()` / `conn.commit()` (it
returns the transaction's bookmark, `string | null`) /
`conn.rollback()`. Read-only access: `Database.open(path, { readOnly:
true })` — the store must exist; any number of readers, no writes or
checkpoints.

`conn.query(cypher, params?, txMetadata?)` takes a third argument
(ADR-0610): the per-transaction metadata map, a plain object through the
same builder, merged with every earlier seed (later keys win) and
serialized into the commit's record at the next transaction boundary —
where the value-provenance audit surface (`provenance()`, `history()`,
`CALL db.transactions`) reads it. Gated by
`RELATE_PREVIEW=value-provenance` (read once, at open): without the
opt-in a call carrying it throws the authored preview error instead of
silently dropping the map; a call without it is unchanged.

`conn.query` also doubles as a template tag — interpolated values become
auto-named `$p0, $p1, …` parameters (injection-safe by construction;
never concatenate values into statement text):

```js
const name = "Alice";
conn.query`MATCH (p:Person {name: ${name}}) RETURN p`;
```

Equal primitives dedup to one parameter (objects by identity); nested
arrays/plain objects are values; unsupported values (`undefined`,
functions, symbols, cyclic structures, non-plain objects — a JS `Date` is
ambiguous, construct a `CypherDateTime`) throw `TypeError` at any depth;
a shared (acyclic) reference binds at each place it appears.
No fragment composition — values are always data.

## Results

Rows are objects keyed by column name; `result.columns` is the RETURN
order. Every cell is the typed value walker's JS shape (ADR-0502 §1) —
there is no per-column type array; discriminate per value: `null`,
booleans, strings (interior NULs preserved), numbers (NaN/±Infinity as
themselves), int64 as `number` inside ±2^53-1 and `bigint` beyond
(`RETURN 9007199254740993` is exactly `9007199254740993n`), nodes
`{kind: "node", elementId, id, labels, properties}`, relationships
`{kind: "relationship", elementId, id, type, startElementId,
endElementId, startId, endId, properties}`, paths as alternating arrays,
`CypherPoint`, `CypherDate`,
`CypherTime`, `CypherLocalTime`, `CypherDateTime`, `CypherLocalDateTime`,
`CypherDuration` (four lossless components — `PT36H` ≠ `P1DT12H`),
`CypherVector`. `result.stats` carries the 11 mutation counters
(snake_case, zeros included). `result.queryType` is `"r"` / `"w"` /
`"rw"` / `"s"` and `result.namespace` the resolved namespace
(`"neo4j"`, `"system"`, `"system_ontologies"`). `result.bookmark` is the
auto-commit bookmark (`"relate:<n>"`) or null.
`result.notifications` is the engine-native query
notification list (ADR-0363) — always an array: `[]` on a plain run, the
plan-time list on `EXPLAIN` and `PROFILE` (`PROFILE` adds the
`EstimationMiss` kind). Each entry carries
`code`, `gql_status`, `severity`, `category`, `title`, `description`
(the diagnosis) and `remedy` (the action), plus `operator_id` on
operator-scoped kinds.

```js
const one = conn.query("MATCH (p:Person {name: $n}) RETURN p.name AS name", { n: "Alice" }).single();
const summary = conn.query("MATCH (p:Legacy) DELETE p").consume();
console.log(summary.counters.nodes_deleted, summary.queryType, summary.elapsedMs);
```

`single()` throws `ResultNotSingleError` on zero or more than one row.
`consume()` returns a `ResultSummary` — counters, resolved namespace,
query type, `elapsedMs` (engine-reported statement elapsed, ADR-0221:
parse, plan and run), its phase split `parseMs` / `planMs` / `executeMs`
(ADR-0491 — three disjoint windows whose sum never exceeds the total),
the `bookmark`, and the same `notifications` list.

## Errors

Every failure throws `RelateError` with the engine's own
classification: `code` (the Neo4j status code), `gqlStatus`, `stage`
(`"compile"` / `"runtime"` / `"internal"`), and `retryable` (also
`isRetriable`) — the retry decision. Retry only `retryable === true` (the
write-write conflict, the retryable workspace cap); never blind-retry.
`stage === "internal"` (a poisoned handle, a caught panic): close and
reopen instead of retrying.

```js
try {
  await conn.queryAsync("RETURN 1/0");
} catch (e) {
  console.log(e.code, e.retryable); // Neo.ClientError.Statement.ArithmeticError, false
}
```

`conn.queryAsync(cypher, params?, txMetadata?)` (and the tag form) runs
the call on koffi's worker thread; the error rides the result handle
back, so the rejected promise is fully classified. The metadata seed
runs inside the serialized task, in call order. Overlapping calls on one
connection are serialized; any sync call (`query`, `begin`, `commit`,
`rollback`, `setNamespace`) while an async query is in flight on the
same connection refuses.

## Graph extraction

`result.graph()` returns the unique graph entities the rows carry (nodes,
relationships, paths — nested values walked), de-duplicated by element
id, in first-seen order, as a `ResultGraph`:

```js
const g = conn.query("MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN a, r, b").graph();
g.nodes            // [{kind: "node", elementId, id, labels, properties}, ...]
g.relationships   // [{kind: "relationship", elementId, id, type, startElementId, endElementId, startId, endId, properties}, ...]
g.size             // entity count
```

Dependency-free conversion to Cytoscape.js or Sigma follows directly from
those shapes (`kind`, element ids, labels, type, endpoints, properties).

## Gotchas

- A result map key containing an interior NUL is refused with an error (the C ABI looks keys up NUL-terminated); string values keep their NULs.
- `":memory:"` vanishes when the process exits — use a path for durable work.
- One writer per database; open `{ readOnly: true }` for concurrent readers.
- Node and relationship values arrive as plain objects keyed by
  `elementId` (the engine's `elementId()` spelling, so
  `WHERE elementId(n) = $id` round-trips), `labels`/`properties` and
  `type`/`startElementId`/`endElementId`; every entity also carries the
  deprecated numeric ids (ADR-0506) — `id` is exactly Cypher `id()`, a
  relationship's `startId`/`endId` are `id(startNode(r))`/`id(endNode(r))`,
  all `number`-or-`bigint` by value — prefer `elementId` for identity;
  temporal values are component
  classes (`CypherDate` &c.), not ISO strings — render to text yourself.
- Int64 beyond ±2^53-1 is a `bigint` — pass big integers as `bigint`
  parameters, never through `Number` (LABS-901).
- Full TypeScript types ship in the package (`index.d.ts`).

Full spec: https://github.com/relatedb/relate — `docs/api/javascript/`.
