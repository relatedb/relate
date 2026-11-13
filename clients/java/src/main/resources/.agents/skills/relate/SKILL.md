---
name: relate
description: Relate is an embedded openCypher graph database, and this is its Java (JVM) library. Use this skill when writing Java, Kotlin or other JVM code that creates, queries, or embeds a local graph database with Cypher — adding the Maven/Gradle dependency, opening a database, running statements, binding typed parameters, transactions, namespaces, typed result rows and metadata, query notifications and plans, and read-only access.
---

# Relate (Java)

Relate embeds a full openCypher engine in-process (Rust core over the C
ABI, bound through JNA; no server, no subprocess). Every call is
synchronous. Databases are a local directory, `":memory:"`, or a `.rl`
file. One writer process per database. Java 17+.

## Dependency

The plain artifact bundles the native library for all five supported
platforms (darwin-arm64, darwin-x64, linux-arm64, linux-x64, win32-x64) —
no environment variable, no library path:

```xml
<dependency>
  <groupId>ai.relatedb</groupId>
  <artifactId>relate</artifactId>
  <version>${relate.version}</version>
</dependency>
```

For a smaller deployable, depend on one platform's classifier jar instead
(`osx-aarch_64`, `osx-x86_64`, `linux-aarch_64`, `linux-x86_64`,
`windows-x86_64` — the os-maven-plugin `${os.detected.classifier}` /
Gradle osdetector vocabulary). A classpath without the running platform's
native fails at first use with a `RelateException` naming the platform
and the classifier to add.

## Connect and query

```java
import ai.relatedb.Database;

try (Database db = Database.open("./mydata");      // or ":memory:" / "./task.rl"
     Database.Connection conn = db.connect()) {    // or db.connect("system")
    conn.query("CREATE (p:Person {name: 'Alice'})");
    Database.Result result = conn.query(
            "MATCH (p:Person) WHERE p.name = $name RETURN p.name AS name",
            java.util.Map.of("name", "Alice"));
    result.columns();   // List<String>: ["name"]
    result.rows();      // List<List<Object>>: [["Alice"]]
}
```

`query(cypher, params)` binds named parameters typed (one putter per Java
type — see Results below for the bindable types). Transactions:
`conn.begin()` / `conn.commit()` / `conn.rollback()`; `commit()` returns
the transaction's bookmark (`"relate:<n>"`, or null when the
transaction committed nothing). Read-only access:
`Database.openReadOnly(path)` — the store must exist; no writes or
checkpoints. `db.checkpoint()` folds the WAL into a checkpoint. `Database`
and `Connection` are `AutoCloseable` — close them (a file store cannot be
reopened in the same JVM otherwise).

## Results

Rows are positional lists in `columns()` order; cells hydrate typed through
the value walker (ABI v2, ADR-0502) — no JSON decode:

- Scalars: `Long` (every Cypher integer, the full int64 range), `Double`,
  `Boolean`, `String` (an interior `\u0000` survives), `null`.
- Graph entities: `Node` (elementId, id, labels, properties),
  `Relationship` (elementId, id, start/end element ids and their numeric
  start/end ids, type, properties), `Path` (alternating
  node, relationship, …, node) — records, not kind-tagged maps.

A map *key* holding an interior `\u0000` throws `RelateException` —
the C ABI's map lookup takes a NUL-terminated key, and the client refuses
the read rather than return the wrong key's value (string values keep
theirs).
- Temporals: `java.time` `LocalDate` / `LocalTime` / `OffsetTime` /
  `LocalDateTime` / `OffsetDateTime` / `ZonedDateTime` (zoned when the
  value keeps a zone, offset-only otherwise); durations as
  `DurationValue(months, days, seconds, nanos)`; points and vectors as
  `Point` / `VectorValue`.

`elementId` is the primary identity (`WHERE elementId(n) = $id`
round-trips); the `id` / `startId` / `endId` longs are the deprecated
legacy numeric ids (ADR-0506) — the values `id(n)` / `id(r)` /
`id(startNode(r))` / `id(endNode(r))` return.

The same types bind as parameters, plus `List` and `Map` (nested to any
depth); an unsupported Java type throws `IllegalArgumentException`.

- `getQueryType()` — `QueryType` enum `READ`, `WRITE`, `READ_WRITE` or `SCHEMA` (plan-time; `EXPLAIN`
  reports the wrapped statement's type without running it).
- `getNamespace()` — the resolved namespace: `Namespace` enum `NEO4J`, `SYSTEM` or
  `SYSTEM_ONTOLOGIES`.
- `getBookmark()` — `"relate:<n>"` after an auto-commit write, `null`
  when the statement committed nothing. Order bookmarks by the numeric
  `n`, not string order (`"relate:10"` sorts before `"relate:9"`).
- `getNotifications()` — typed `Notification`s (code, GQLSTATUS,
  `Severity`/`Category` enums, title, description, remedy, operator id); empty on a
  plain run, populated by `EXPLAIN` / `PROFILE`.
- `getPlan()` — the `relate.plan/v1` document for `EXPLAIN` /
  `PROFILE`, `null` on a plain run.

## Gotchas

- **Parameter binding is typed** — bind values with
  `query(cypher, Map.of(...))`; never concatenate untrusted input into the
  statement text.
- Errors throw typed exceptions: `TransientException` exactly when the
  engine marked the failure retryable (catch it to retry; a failed commit
  ends the transaction), `ClientException` for `Neo.ClientError.*`,
  `DatabaseException` for `Neo.DatabaseError.*`. Every one carries
  `getCode()`, `getGqlStatus()`, `getStage()` (the closed enum
  `RelateException.Stage`: `COMPILE` / `RUNTIME` / `INTERNAL`) and
  `isRetryable()` — classify on those, never on message text.
- `":memory:"` vanishes when the JVM exits — use a path for durable work.
- The bundled native is extracted once per version into the user cache
  (`~/Library/Caches/relate`, `$XDG_CACHE_HOME`/`~/.cache/relate`,
  `%LOCALAPPDATA%\relate`); relocate it with
  `-Drelate.native.cache=<dir>` (read-only home, `noexec` mounts).
- `RELATE_LIB` overrides the bundled native — a development escape
  hatch for a self-built library, not a deployment mechanism.
- JDK 24+ prints a JNI "restricted method" warning (JEP 472); silence it
  with `--enable-native-access=ALL-UNNAMED`.

Full spec: https://github.com/relatedb/relate — `docs/api/java/`.
