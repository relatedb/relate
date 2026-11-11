# @relate/relate — Relate for Node.js

**An embedded graph database for Node.js. Query it in Cypher, in-process — no server, no network, nothing to deploy.**

Relate is an in-memory property-graph engine written in Rust. This package
runs it inside your Node process (through the engine's C ABI): a graph that is
pure in-memory, a directory, or a single `.rl` file, made durable by a
fsync'd write-ahead log and Parquet checkpoints.

> An experimental Neo4j Labs project, not part of Neo4j's supported product
> lineup.

## Install

```sh
npm install @relate/relate
```

Node 18+. The native library arrives as an optional dependency for your
platform (`@relate/relate-<os>-<arch>`: macOS arm64/x64, Linux x64/arm64,
Windows x64); there are no install scripts and no Rust toolchain is needed.
Full TypeScript types ship in the package (`index.d.ts`).

## Quick start

```js
const { Database } = require("@relate/relate");

const db = Database.open(":memory:");      // or a path: Database.open("./friends")
const conn = db.connect();

conn.query("CREATE (:Person {name: $name, age: $age})", { name: "Alice", age: 30 });
conn.query("CREATE (:Person {name: $name, age: $age})", { name: "Bob", age: 28 });
conn.query(
  "MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}) " +
    "CREATE (a)-[:FRIENDS_WITH {since: 2020}]->(b)"
);

const result = conn.query(
  "MATCH (p:Person)-[:FRIENDS_WITH]->(f) RETURN p.name AS person, f.name AS friend"
);
console.log(result.columns); // ["person", "friend"]
for (const row of result.rows) console.log(row.person, "->", row.friend);

db.checkpoint(); // flush to disk (a no-op for :memory:)
```

`query` is synchronous (the engine is in-process); `queryAsync` is its
Promise-returning twin and runs the call on a worker thread.

## Persistence

| Store | Open with |
| --- | --- |
| In-memory — gone at exit | `Database.open(":memory:")` |
| Directory — WAL + Parquet shards (default) | `Database.open("./friends")` |
| Single file — WAL, snapshot and catalog in one `.rl` file | `Database.open("./friends.rl")` |
| Read-only — load once, refuse writes and checkpoints | `Database.open("./friends", { readOnly: true })` |

A second *writable* open of the same store is refused (single-writer lease);
use `readOnly` for readers.

## Parameters

Pass a plain object and reference values as `$name`. `null`, booleans,
strings, arrays and plain objects (nested lists and maps) bind as themselves;
an integer-valued `number` binds as INTEGER, any other as FLOAT; a `bigint`
binds as an exact INTEGER over the full int64 range. A value that cannot be a
parameter (`undefined`, functions, `Map`, `Set`, class instances) throws a
`TypeError` naming its path — it is never dropped silently.

```js
const { float, CypherDate, CypherDuration } = require("@relate/relate");

conn.query("RETURN $d AS d, $f AS f, $big AS big, $dur AS dur", {
  d: new CypherDate(2026, 11, 4),
  f: float(1),                  // FLOAT 1.0, not the INTEGER 1
  big: 9007199254740993n,       // exact beyond 2^53
  dur: new CypherDuration(0, 1, 0, 0),
});
```

The same typed classes (`CypherDate`, `CypherTime`, `CypherLocalTime`,
`CypherDateTime`, `CypherLocalDateTime`, `CypherDuration`, `CypherPoint`,
`CypherVector`) are what results decode to, so a value read from a result
binds back unchanged. A JS `Date` is deliberately not a datetime — its
UTC/local reading is ambiguous; build a `CypherDateTime`.

Tagged templates turn interpolations into parameters — injection-safe by
construction, values never enter the statement text:

```js
const name = "Alice";
conn.query`MATCH (p:Person {name: ${name}}) RETURN p.age AS age`;
```

## Results

`result.rows` are plain objects keyed by column name. Each cell decodes to
exactly one JS shape: `null`, `boolean`, `string`, `Array`, plain object
(maps keep the engine's key order), a typed class for temporals/points/vectors,
and for integers a `number` when exact (within ±2^53−1) and a `bigint`
beyond — never a silently rounded number. Nodes and relationships arrive as
`{ kind, elementId, labels|type, properties, … }`; `elementId` equals Cypher's
`elementId()`, so `WHERE elementId(n) = $id` finds the entity again. Paths are
arrays alternating nodes and relationships.

```js
const one = conn.query("MATCH (p:Person {name: 'Alice'}) RETURN p.age AS age").single();
one.age;                                   // 30 — or ResultNotSingleError

const created = conn.query("CREATE (:Person {name: 'Carol'})");
created.stats.nodes_created;               // 1 — all 11 counters, zeros included
created.bookmark;                          // "relate:<n>" after an auto-commit write

const summary = conn.query("MATCH (p:Person) RETURN p").consume();
summary.queryType;                         // "r" | "w" | "rw" | "s"
summary.namespace;                         // the statement's resolved namespace
summary.elapsedMs;                         // engine time; parseMs / planMs / executeMs split it

const graph = conn.query("MATCH (a)-[r]->(b) RETURN a, r, b").graph();
graph.nodes; graph.relationships;          // de-duplicated by elementId
```

Also on a result: `notifications` (plan-time notices, populated by
`EXPLAIN`) and `plan` (the `relate.plan/v1` document).

## Transactions

Statements auto-commit unless a transaction is open.

```js
conn.begin();
try {
  conn.query("CREATE (:Person {name: 'Dan'})");
  conn.query("CREATE (:Person {name: 'Eve'})");
  const bookmark = conn.commit();          // "relate:<n>", or null if nothing committed
} catch (e) {
  conn.rollback();
  throw e;
}
```

## Errors

Every failure throws a `RelateError` carrying the engine's own
classification — `code` (the Neo4j status code), `gqlStatus`, `stage`
(`"compile"`, `"runtime"` or `"internal"`) and `retryable` — so a retry loop
never parses messages:

```js
try {
  conn.query("RETURN 1/0");
} catch (e) {
  console.log(e.code, e.retryable);        // Neo.ClientError.Statement.ArithmeticError false
}
await conn.queryAsync("RETURN 1/0").catch((e) => console.log(e.code));
```

A write-write conflict is the retryable case; it can surface on a statement or
on `commit()`, so check both. `stage === "internal"` means the handle may be
poisoned: close it and open a new one.

## Async

`queryAsync` calls on one connection run one at a time (queued for you);
distinct connections run concurrently. While an async query is in flight, a
synchronous call on the same connection (`query`, `begin`, `commit`,
`rollback`, `setNamespace`) throws instead of breaking the connection's
contract.

## Bigger loads

Send rows in batches with `UNWIND $rows`, a few tens of thousands per
statement, rather than one huge statement or one statement per row.

## Serving it over the network

The same engine runs as a server: start `relate serve --bolt 7687` (see the
[CLI](https://github.com/relatedb/relate/blob/main/docs/api/cli/spec.md)) and
use the stock `neo4j-driver` against it.

## Documentation

- [API spec](https://github.com/relatedb/relate/blob/main/docs/api/javascript/spec.md) — every type, error and lifecycle rule
- [Runnable examples](https://github.com/relatedb/relate/tree/main/docs/api/javascript/examples)
- [Project home](https://github.com/relatedb/relate) · [relatedb.ai](https://relatedb.ai/) · [known limitations](https://github.com/relatedb/relate/blob/main/docs/limitations.md)

The package also ships a library skill for AI agents
(`.agents/skills/relate/SKILL.md`; `npx library-skills`).

## Development (repo checkout)

```sh
cd clients/javascript
npm install
cargo build -p relate-ffi --release   # repo root; `make build` builds only the CLI
npm test                              # node test.js && node loadtest/test.js
```

The loader (koffi) resolves the library in order: `RELATE_LIB` (any path
form), the installed platform package, then the repo's `target/debug` and
`target/release`. A stale pre-ABI-v2 library there fails with an
`abiViolation` error — rebuild the profile the loader picks.

The C header is `surfaces/relate-ffi/include/relate.h`; the client requires
`relate_abi_version() == 2`. `make binding-js` from the repo root builds the
library and packs the client tarball into `target/` (never the source tree).

### Platform packages

Each OS/arch ships its own npm package, `@relate/relate-<os>-<arch>`, with the
native library under `lib/` — no install scripts. Sources live in
[`platforms/`](./platforms) (`darwin-arm64`, `darwin-x64`, `linux-arm64`,
`linux-x64`, `win32-x64`); the release workflow assembles them from the
per-triple FFI builds.

## License

Apache-2.0
