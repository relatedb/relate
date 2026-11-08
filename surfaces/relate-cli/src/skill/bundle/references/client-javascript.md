# JavaScript (Node.js) client: @relate/relate

## Install

```sh
npm install @relate/relate
```

The npm packages are pending their first publish (the docs/PUBLISHING.md
runbook); until then use the client from a repo checkout
(`clients/javascript`). Requires Node 18+; wraps the typed C ABI v2
(ADR-0502) via koffi — no install scripts. The client checks
`relate_abi_version() === 2` at `require` time and refuses anything
else with an "incompatible librelate_ffi" error.

## Minimal usage

```js
const { Database } = require("@relate/relate");

const db = Database.open(":memory:"); // or "./mydata" / "./task.rl"
const conn = db.connect();
const result = conn.query("RETURN 1 AS n");
// result.columns: string[]  // ["n"]
// result.rows: object[]     // [{ n: 1 }] — keyed by column name; every
//                           // cell is the typed value walker's JS shape
//                           // (ADR-0502 §1): int64 lossless (number inside
//                           // ±2^53-1, bigint beyond — LABS-901), nodes
//                           // {kind, elementId, id, labels, properties},
//                           // relationships {kind, elementId, id, type,
//                           // startElementId, endElementId, startId, endId,
//                           // properties} (numeric ids = Cypher id() /
//                           // id(startNode(r)) / id(endNode(r)), ADR-0506),
//                           // temporals as component classes
// result.stats: object      // the 11 mutation counters, zeros included
// result.bookmark: string | null // "relate:<n>" after an auto-commit
//                           // write; null when it committed nothing
// result.queryType: "r" | "w" | "rw" | "s"   (ADR-0162 wire code)
// result.namespace: "neo4j" | "system" | "system_ontologies" (resolved)
// result.plan: object | null — the relate.plan/v1 document (ADR-0381)
// result.notifications: object[] — engine-native query notifications
//                            (ADR-0363); [] on a plain run, populated by
//                            EXPLAIN and PROFILE (PROFILE adds the
//                            PROFILE-only EstimationMiss)
// result.single() / result.consume() / result.graph()   (ADR-0216)
// result.consume().elapsedMs: number — engine-reported milliseconds (ADR-0221)
// result.consume().parseMs / planMs / executeMs: number — the phase split of
//                            elapsedMs (ADR-0491); sum never exceeds the total
```

`conn.query(cypher, params?)` — `params` is a plain object lowered into the
typed params builder: every `$name` the statement references must be in it
(`null` is a value), or the query throws the engine's parameter-missing
error. Values: `null`, booleans, strings, numbers (integer-valued binds
INTEGER, otherwise FLOAT — **NaN and ±Infinity bind**), full-range
`bigint`s, arrays, plain objects, and the typed classes results decode to
(`CypherDate`, `CypherTime`, `CypherLocalTime`, `CypherDateTime`,
`CypherLocalDateTime`, `CypherDuration`, `CypherPoint`, `CypherVector`) —
a decoded value binds back unchanged. `float(1)` binds a FLOAT-typed 1.0.
Auto-commit unless `begin()` is open; `begin()` / `commit()` /
`rollback()` are an explicit transaction and `commit()` returns the
transaction's bookmark (`string | null`).

`conn.query` also works as a template tag (ADR-0222):
``conn.query`MATCH (p {name: ${name}}) RETURN p` `` — each interpolated
value becomes an auto-named `$p0` parameter, injection-safe by
construction; equal primitives dedup to one parameter (objects by
identity); unsupported values (`undefined`, functions, symbols,
non-plain objects, cyclic structures) throw `TypeError` at any depth; a
shared (acyclic) reference binds at each place it appears.

## Gotchas

- A result map key containing an interior NUL is refused with an error (the C ABI looks keys up NUL-terminated); string values keep their NULs.
- **Native library resolution** — the platform optional-dependency
  (`@relate/<platform>-<arch>`) is the normal path;
  `RELATE_LIB` (an explicit library path) overrides it when set; a repo
  checkout falls back to `target/debug` and `target/release` (build
  `relate-ffi` first). A missing library throws at `require` time and
  names all three locations. A library with the wrong ABI version (or none
  at all) also throws at `require` time (the version check).
- **Five platforms only** — `linux-x64`, `linux-arm64`, `darwin-x64`,
  `darwin-arm64`, `win32-x64`. Any other os/arch has no platform package;
  point `RELATE_LIB` at a self-built `librelate_ffi`.
- **Node 18+** — older runtimes are unsupported (`engines: node >=18`).
- **No per-column types** (ADR-0502) — every cell carries its own kind
  through the typed value walker, so discriminate per value (`typeof`,
  `kind`, `instanceof`), not per column. Byte arrays cross as a list of integers 0–255 and UUIDs as
  strings; byte arrays cannot be stored as properties.
- **Errors are typed, retry by flag** — every failure throws
  `RelateError` carrying the engine's own classification: `code` (the
  Neo4j status code), `gqlStatus`, `stage` (`"compile"` / `"runtime"` /
  `"internal"`) and `retryable` (also `isRetriable`). Retry only
  `retryable === true` — the write-write conflict and the retryable
  workspace cap; never blind-retry. `stage === "internal"` (a poisoned
  handle, a caught panic): close and reopen instead of retrying.
- **Int64 beyond 2^53 is a bigint** — `RETURN 9007199254740993` is exactly
  `9007199254740993n`; pass big integers as `bigint` parameters, never
  through `Number` (LABS-901).
- **Async queries** — `conn.queryAsync(cypher, params?)` (and the tag
  form) runs the call on koffi's worker thread and rejects with the same
  typed `RelateError`. Overlapping calls on one connection are serialized;
  any sync call (`query`, `begin`, `commit`, `rollback`, `setNamespace`)
  while an async query is in flight on the same connection refuses.
- **Result ergonomics (ADR-0216)** — rows are column-keyed objects;
  `result.stats` carries the 11 mutation counters (snake_case, zeros
  included; Neo4j's counting: labels per node, every property write); `single()` throws `ResultNotSingleError` on zero or >1 rows;
  `consume()` returns `{ counters, namespace, queryType, elapsedMs,
  parseMs, planMs, executeMs, bookmark }` (`elapsedMs` is engine-reported — parse,
  plan and run, ADR-0221; `parseMs`/`planMs`/`executeMs` are its phase
  split, ADR-0491 — three disjoint windows whose sum never exceeds the
  total);
  `graph()` returns the result's unique entities
  (`{ nodes, relationships }`, first-seen order, dedup'd by element id).
  Entity values are the decoded shapes `{kind, elementId, id, labels,
  properties}` / `{kind, elementId, id, type, startElementId,
  endElementId, startId, endId, properties}` — the numeric ids are the
  deprecated Cypher `id()` / `id(startNode(r))` / `id(endNode(r))`
  (ADR-0506, number-or-bigint by value; prefer `elementId`), and the
  `kind` tag (`"node"`/`"relationship"`) is the
  dispatch key, never key-presence guessing. `queryType` is the plan-time
  classification (`"r"`/`"w"`/`"rw"`/`"s"`; EXPLAIN reports the wrapped
  statement's type without executing it) and `namespace` is the
  statement's *resolved* namespace — the `USE` target when the statement
  carries one, else the session namespace (ADR-0285). A value outside
  either vocabulary throws `ABI contract violated: ...` (as in Go).
- **Query notifications (ADR-0363)** — `result.notifications` and
  `result.consume().notifications` are always an array: `[]` on a plain
  run (derivation is opt-in with the plan tree), the plan-time list on
  `EXPLAIN` and `PROFILE` (`PROFILE` adds the `EstimationMiss` kind). Each
  entry carries `code`,
  `gql_status`, `severity` (`"INFORMATION"` / `"WARNING"`), `category`
  (`"PERFORMANCE"` / `"UNRECOGNIZED"` / `"DEPRECATION"` / `"SCHEMA"` / `"HINT"`), `title`,
  `description` (the diagnosis) and `remedy` (the action to take), plus
  `operator_id` on operator-scoped kinds. There is no `position` key —
  render the remedy rather than jumping to source.
- **One writer process per database** — the same single-writer rule as the
  CLI applies; `:memory:` is ephemeral.

Full contract: `docs/api/javascript/spec.md` (types, errors, lifecycle,
namespaces).
