# JavaScript client — spec

Package: `@relate/relate` on npm (`clients/javascript`). Requires
Node 18+.

```sh
npm install @relate/relate
```

The native library normally arrives via the platform optional-dependency
for the current os/arch (`@relate/<platform>-<arch>`,
ADR-0278) — no install scripts. In a repo checkout, or on a platform
without a published package, it is found via `RELATE_LIB` or the repo
`target/{debug,release}` directories (the dev fallback).

The client wraps the **typed C ABI v2** (ADR-0502) through koffi. At
`require` time it resolves `relate_abi_version()` and refuses to load
anything but `2` — a library without the symbol or one with a later
incompatible version fails immediately with an
"incompatible librelate_ffi" error, naming the problem instead of a
missing walker symbol surfacing at the first query.

```js
const { Database } = require("@relate/relate");
const db = Database.open(":memory:");
const reader = Database.open(":memory:", { readOnly: true });
const conn = db.connect();
const result = conn.query("RETURN 1 AS n");
// result.columns: string[]            ["n"]
// result.rows: Row[]                  [{ n: 1 }] — objects keyed by column
//                                     name; cells are the typed walker's JS
//                                     values (the table below)
// result.stats: Stats                 the 11 mutation counters, zeros included
// result.queryType: "r" | "w" | "rw" | "s"
// result.namespace: "neo4j" | "system" | "system_ontologies"
// result.bookmark: string | null      "relate:<n>" after an auto-commit
//                                     write, null when it committed nothing
// result.plan: PlanDocument | null   the relate.plan/v1 document
// result.notifications: Notification[]  [] on a plain run; the plan-time
//                                     list on EXPLAIN (ADR-0363)
// result.consume().elapsedMs: number (engine-reported milliseconds, ADR-0221)
// result.consume().parseMs / planMs / executeMs: number — the phase split of
//                                     elapsedMs (ADR-0491); their sum never
//                                     exceeds elapsedMs
```

`Database.open(uri, { readOnly: true })` loads once and refuses writes
and checkpoints (ADR-0136) — a missing store is an error. Omitting the
options keeps the writable open, which creates a missing store at open.

`query(cypher, params?)` — `params` is a plain object lowered into the
typed params builder (the parameters section below). Auto-commit unless
`begin()` is open. `begin()` / `commit()` / `rollback()` are an explicit
transaction (ADR-0028); `commit()` returns the transaction's bookmark
(bookmarks section below). Failing calls throw `RelateError`
(errors section below).

## Result values (the typed value walker, ADR-0502 §1)

Every cell crosses through the C ABI's typed value walker — one kind per
value, structural at every depth — and decodes to exactly one JS shape.
There is **no per-column type array**: each value carries its own kind,
so a column can mix shapes freely.

| Cypher kind | JS value |
| --- | --- |
| null | `null` |
| BOOLEAN | `boolean` |
| INTEGER | `Int64` — see below |
| FLOAT | `number`, NaN/±Infinity as themselves |
| STRING | `string`, interior NULs (`\u0000`) preserved |
| NODE | `{ kind: "node", elementId, id, labels, properties }` |
| RELATIONSHIP | `{ kind: "relationship", elementId, id, type, startElementId, endElementId, startId, endId, properties }` |
| PATH | array alternating `[node, rel, …, node]` |
| LIST | `Array` |
| MAP | plain object, engine key order. A key holding an interior `\u0000` throws — `relate_value_map_get` takes a NUL-terminated key, so the client refuses rather than return the value of the key the truncation lands on (string *values* keep their NULs) |
| POINT | `CypherPoint` (`srid`, `x`, `y`, `z` — `z` undefined on 2D) |
| DATE | `CypherDate` (`year`, `month`, `day`) |
| TIME | `CypherTime` (`hour`, `minute`, `second`, `nanosecond`, `offsetSeconds`) |
| LOCAL_TIME | `CypherLocalTime` |
| DATETIME | `CypherDateTime` (… `offsetSeconds`, `zone` — IANA id or `null` when offset-only) |
| LOCAL_DATETIME | `CypherLocalDateTime` |
| DURATION | `CypherDuration` (`months`, `days`, `seconds`, `nanos` — lossless) |
| VECTOR | `CypherVector` (`dtype`, `values`) |
| anything else | a defined `unsupported value kind N` error — never a crash or a silent `null` |

**Int64 (LABS-901).** A Cypher INTEGER is a JS `number` when exactly
representable (within ±2^53-1) and a `bigint` beyond it — deterministic by
value, never a silently rounded Number: `RETURN 9007199254740993` is
exactly `9007199254740993n`, never `9007199254740992`. The same policy
applies at every depth and to `CypherVector`'s INTEGER* coordinates and
the temporal components.

**Entities.** `elementId` is the engine's `elementId()` spelling
(`"4:relate:n0"`), so `WHERE elementId(n) = $id` round-trips; a
relationship's endpoints are `startElementId` / `endElementId`. Every
entity also carries the deprecated numeric ids (ADR-0506): `id` is
exactly what Cypher `id()` returns for the same entity, and a
relationship's `startId` / `endId` are `id(startNode(r))` /
`id(endNode(r))` — all under the `Int64` policy. Prefer `elementId`
(new code, stable identity); the numeric ids exist for driver providers
that decide entity equality by `id()`. Byte
arrays cross as a LIST of integers 0–255 and UUIDs as STRING (the walker
has no separate kinds for them).

**Durations are components, not ISO text.** `duration('PT36H')` is
`{seconds: 129600, days: 0}` and `duration('P1DT12H')` is
`{days: 1, seconds: 43200}` — distinct, where a milliseconds projection
would collapse them. Rendering to ISO text is the caller's choice.

## Parameters (the typed params builder, ADR-0502 §4)

`params` is a plain object; Cypher references the values as `$name`. Each
value lowers to one putter call — the binding is the validation, so an
unsupported leaf at any depth rejects with a `TypeError` naming its path
(`query params: "v".nested cannot be a Cypher parameter`) instead of being
silently dropped:

- `null`, booleans, strings (interior NULs preserved) — direct
- `number` — integer-valued binds as INTEGER (a `bigint` is the exact form
  beyond ±2^53-1), otherwise FLOAT; **NaN and ±Infinity bind** (the JSON
  era could not carry them)
- `bigint` — INTEGER, full int64 range exact; outside int64 rejects
- arrays and plain objects — nested LIST / MAP; cyclic structures reject
  (a container that contains itself), while a shared reference — the same
  object under two keys or list slots — binds at each place
- the typed classes — `CypherDate`, `CypherTime`, `CypherLocalTime`,
  `CypherDateTime` (a `zone` keeps the zone; `null` binds offset-only),
  `CypherLocalDateTime`, `CypherDuration`, `CypherPoint`, `CypherVector`:
  the same classes result values decode to, so a decoded value binds back
  unchanged
- `undefined`, functions, symbols, and non-plain objects (`Map`, `Set`,
  class instances) reject — a JS `Date` is deliberately not a datetime
  (its UTC/local reading is ambiguous; construct a `CypherDateTime`)

**A float-typed 1.** `float(1)` (or `new CypherFloat(1)`) binds the FLOAT
1.0, not the INTEGER 1 — the thing the JSON era could not send. Cypher's
`=` compares numerically across INT and FLOAT (`1 = 1.0` is true either
way), so the type is observable in the rendering: `toString($f)` is
`"1.0"` for `float(1)` and `"1"` for `1`.

Every `$name` the statement references must be in `params` (`null` is a
value), or the query throws the engine's parameter-missing error.

## Tagged-template queries (ADR-0222)

`query` doubles as a template tag — the string form above stays the
documented primary:

```js
const name = "Alice";
conn.query`MATCH (p:Person {name: ${name}}) RETURN p`;
```

Each interpolated value is replaced by an auto-named `$p0, $p1, …`
parameter and the tag runs the same params path — injection-safe by
construction: values never enter the statement text, so a value
containing `}` or Cypher text stays data. The same primitive value
interpolated twice maps to one parameter (SameValueZero equality);
objects dedup by identity. The value policy is the params builder's
(the section above). There is no fragment composition — interpolated
values are always data; build a string and use the primary form for
dynamic clauses.

See [`examples/tagged_templates.js`](examples/tagged_templates.js).

## Errors (the one error shape, ADR-0502 §2)

Every failure throws a `RelateError` carrying the engine's own
classification — parsed once from the ABI's error JSON, never re-derived
from message text:

- `code` — the Neo4j status code
  (`"Neo.ClientError.Statement.ArithmeticError"`)
- `gqlStatus` — the GQLSTATUS value (`"22G03"`)
- `stage` — `"compile"` (parse/plan), `"runtime"` (execution) or
  `"internal"` (a boundary failure: a NULL or poisoned handle, a caught
  panic, an argument that failed to decode)
- `retryable` — the drivers' transient test, also spelled `isRetriable`;
  **the retry decision**: `true` exactly when re-running the same work
  may succeed (today the write-write conflict and the retryable workspace
  cap). Everything else fails loudly — inspect it, never blind-retry.
- `message` — the engine's message

The shape is validated, never defaulted: every field present and
correctly typed, the stage in the vocabulary. A malformed or partial
error shape — a library this client cannot trust — throws an
`ABI contract violated: ...` error naming the problem, never a
`RelateError` with fabricated null fields (a missing `retryable`
would silently decode as `false` and a retry loop would decline to
retry). The Go and Swift clients hold the same contract. The one
client-side `RelateError` — the sync-call refusal while an async
query is in flight — carries no engine classification (`code` /
`gqlStatus` / `stage` `null`, `retryable` `false`).

`stage === "internal"` means the handle may be poisoned: close it and
open a new one instead of retrying. A write-write conflict can surface on
a statement (`Neo.TransientError.Transaction.Outdated`,
`retryable: true`) or on `commit()` — a retry loop must check both.

See [`examples/typed_values.js`](examples/typed_values.js).

## Async queries

`queryAsync(cypher, params?)` (and the template-tag form) is the
Promise-returning twin of `query`: the FFI call runs on koffi's worker
thread, the walker decode runs on the main thread when the callback
lands, and the resolved value is the same `Result`.

- **Errors survive the worker thread.** A statement failure rides the
  result handle and a connection failure the call's error out-param; both
  reject the promise as a `RelateError` with the code.
- **One call at a time per connection** is the ABI's connection contract.
  Overlapping `queryAsync` calls on one connection are serialized through
  a per-connection queue; distinct connections run concurrently. Every
  synchronous call on a connection — `query`, `begin`, `commit`,
  `rollback`, `setNamespace` — while an async query is in flight on it
  refuses with a defined `RelateError` instead of breaking the
  contract.

## Bookmarks (ADR-0502 §7)

A bookmark is the engine's commit sequence, `"relate:<n>"` — `n` grows
monotonically across every connection and surface of a store, so a later
commit's bookmark sorts after an earlier one's (compare the `n`, not the
string).

- Auto-commit write: `result.bookmark` (also `result.consume().bookmark`).
  `null` when the statement committed nothing — a read, an `EXPLAIN`, a
  statement inside an explicit transaction, or an error result.
- Explicit transaction: `conn.commit()` **returns** the bookmark, or
  `null` when the transaction committed nothing (no bookmark is
  fabricated; the caller's last bookmark stays current). A failed commit
  throws `RelateError` through the same status/error channel as every
  other call (ADR-0510): the ABI status alone tells a failure from an
  empty commit, so a failure never surfaces as `null`.

## Namespaces (ADR-0117)

A connection's session namespace is the default graph for statements
without a `USE` clause. Fixed namespaces: `"neo4j"` (default), `"system"`,
`"system_ontologies"`, matched case-insensitively.

- `db.connect(namespace?)` — omitting `namespace` keeps `"neo4j"` (same as
  the zero-argument `connect()` before ADR-0117). Both forms wrap the
  canonical `relate_connect_namespace` (ADR-0220): an omitted namespace
  reaches the ABI as a NULL name, which selects `"neo4j"`.
- `conn.setNamespace(name)` — changes the session namespace between
  transactions. Wraps `relate_conn_set_namespace`.

Errors throw `RelateError`:

- Unknown name: message ends `Graph not found: <name>` (ADR-0146)
- `setNamespace` while `begin()` is open: message ends
  `Cannot change the namespace while a transaction is open.`

See [`examples/namespace.js`](examples/namespace.js).

## Result metadata (ADR-0285)

The object `query()` returns carries the statement metadata
alongside `columns` and `rows`, read off the result before it is
destroyed (the underlying C ABI pointers are result-owned; koffi copies
them during marshalling):

- `result.namespace` (`string`) — the statement's *resolved* namespace
  (ADR-0117 §7): the `USE` target when the statement carries a `USE`
  clause, else the session namespace. One of `"neo4j"`, `"system"` or
  `"system_ontologies"` (any other value is an `ABI contract violated: namespace ...`
  error, like Go). This is per-statement: a `USE`-routed statement
  reports its target without changing the connection's session
  namespace.
- `result.queryType` (`string`) — the statement's plan-time
  classification (ADR-0162) as its Neo4j Query API v2 wire code: `"r"`
  (read), `"w"` (a write with no returned columns), `"rw"` (a write with
  returned columns) or `"s"` (schema), the split per ADR-0332; any other value
  is an `ABI contract violated: query type ...` error. EXPLAIN
  and PROFILE report the wrapped statement's type without executing it.
- `result.consume().elapsedMs` (`number`) — the engine-reported statement
  elapsed in wall-clock milliseconds (ADR-0221): parse, plan and run,
  measured once in the engine, excluding client-side marshalling.
- `result.consume().parseMs` / `.planMs` / `.executeMs` (`number`) — the
  phase split of `elapsedMs` (ADR-0491), read off the live C result via
  `relate_result_parse_ms` / `_plan_ms` / `_execute_ms`: `parseMs` is
  the front-end parse and lowering window, `planMs` the binding, logical
  planning and admission classification window, `executeMs` the dispatch
  window (the EXPLAIN plan render for an EXPLAIN statement). Measured
  always-on; three disjoint sub-windows whose sum never exceeds
  `elapsedMs` — the inter-stage gaps stay in the total alone.
- `result.bookmark` (`string | null`) — the auto-commit bookmark
  (bookmarks section above).
- `result.notifications` (`Notification[]`) — the statement's query
  notifications (ADR-0363), read off the live C result via
  `relate_result_notifications` (a JSON array string in the
  engine-native shape). The array is always present: `[]` on a plain run,
  because derivation is opt-in with the plan tree and only `EXPLAIN` and
  `PROFILE` populate it (`PROFILE` adds the `EstimationMiss` kind). Each
  entry is a plain object with `code`,
  `gql_status` (the GQLSTATUS value, e.g. `"03N90"`), `severity`
  (`"INFORMATION"` or `"WARNING"`), `category` (`"PERFORMANCE"`,
  `"UNRECOGNIZED"`, `"DEPRECATION"`, `"SCHEMA"` or `"HINT"`), `title`, `description` (the
  diagnosis) and `remedy` (the action that resolves it), plus `operator_id`
  (the raising plan operator's id) on operator-scoped kinds. There is no
  `position` key. For example an `EXPLAIN MATCH (a:Person), (b:City) RETURN
  a, b` on a store holding both labels carries
  `Neo.ClientNotification.Statement.CartesianProduct`. The same list is on
  `result.consume().notifications`.

See [`examples/result_metadata.js`](examples/result_metadata.js).

## Result ergonomics (ADR-0216)

`query()` returns a `Result` (rows materialized at query time — there is
no fetch position):

| Member | Behavior |
| --- | --- |
| `result.rows` | rows as objects keyed by column name; cells are the typed walker's JS values (the table above) |
| `result.stats` | the canonical 11 mutation counters, snake_case, zeros included; Neo4j's counting (ADR-0309): labels per node, one `properties_set` per property write, schema DDL in the index and constraint counters |
| `result.single()` | the one row; throws `ResultNotSingleError` on zero or more than one |
| `result.notifications` | the statement's query notifications (ADR-0363) in the engine-native shape, `[]` on a plain run; also on `consume()` |
| `result.plan` | the statement's structured plan (LABS-418, ADR-0381) as a `relate.plan/v1` document, `null` on a plain run; also on `consume()`. Each node's `warnings` carry the operator's remedy-bearing notifications (ADR-0363), joined by operator id |
| `result.bookmark` | the auto-commit bookmark (ADR-0502 §7), `null` when the statement committed nothing; also on `consume()` |
| `result.consume()` | a `ResultSummary`: `counters`, `namespace`, `queryType`, `elapsedMs` (engine-reported statement elapsed, ADR-0221), `parseMs` / `planMs` / `executeMs` (its phase split, ADR-0491), `notifications` (ADR-0363), `bookmark`; idempotent |
| `result.graph()` | the result's unique graph entities as a `ResultGraph`: `nodes` / `relationships` in first-seen order, de-duplicated by element id; every node, relationship and path the rows carry (nested values walked) contributes; empty — not an error — when the rows carry none |

Entity values are the decoded shapes (the table above). What makes a
value an entity is structural: the decoder stamps each NODE /
RELATIONSHIP with a non-enumerable module-local
`Symbol("relate.decoded")` marker (its value the kind), and
`graph()` dispatches on that marker alone — a user MAP carrying
`kind: "node"` and every entity key stays a map and is never collected.
The marker is module-local on purpose, never in the global symbol
registry: no user code can forge or probe entity-ness through
`Symbol.for`, and only the decoder's own constructor can stamp one. `graph()`'s
dedup key is the element id, and the ordering is first-seen (element ids
are opaque strings — `"<db>:<store>:n<n>"` — whose lexicographic order
misleads across digit-count boundaries). `peek()`/`fetch(n)` are
deliberately absent: `rows` is already an array, so `rows[0]` /
`rows.slice(0, n)` say it directly.

The package ships hand-written TypeScript types (`index.d.ts`) and a
library skill (library-skills.io) at
`.agents/skills/relate/SKILL.md` — `npx library-skills` installs it
for AI agents, version-synced with the package.

See [`examples/result_ergonomics.js`](examples/result_ergonomics.js),
[`examples/typed_values.js`](examples/typed_values.js) (the value table,
errors, bookmarks and async in one run) and
[`examples/transaction.js`](examples/transaction.js).

Fraud and identity load-test CLIs (`bin/fraud-loadtest.js`,
`bin/identity-loadtest.js`) wrap this client. Flags, mix, and report JSON
are documented in [`docs/datasets.md`](../../datasets.md).
