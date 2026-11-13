# Java client — spec

Artifact: `ai.relatedb:relate` (`clients/java`). The client speaks
the typed C ABI v2 (ADR-0502) through JNA: a library that is not v2 fails
at first use with `incompatible librelate_ffi …` — never at a missing
walker symbol later.

```java
try (Database db = Database.open(":memory:");        // AutoCloseable
     Database.Connection conn = db.connect()) {       // or db.connect("system")
    Result result = conn.query("RETURN $n AS n", java.util.Map.of("n", 1));
    result.columns();      // List<String>
    result.rows();         // List<List<Object>> — typed values (see below)
    result.getQueryType(); // QueryType: READ | WRITE | READ_WRITE | SCHEMA
    result.getNamespace(); // Namespace: NEO4J | SYSTEM | SYSTEM_ONTOLOGIES
    result.getBookmark();  // "relate:<n>" after an auto-commit write, else null
    result.getNotifications(); // List<Notification>, [] on a plain run
} // close() is real: a file store reopens in the same JVM afterwards
```

`Database.openReadOnly(uri)` loads once and refuses writes and checkpoints
(ADR-0136) — a missing store is an error. `Database.open(uri)` stays
writable and creates a missing store at open.

`begin()` / `commit()` / `rollback()` are an explicit transaction (ADR-0028).
`commit()` returns the transaction's bookmark (`"relate:<n>"`, or `null`
when the transaction committed nothing). A write-write conflict throws
`TransientException` with `isRetryable() == true` — the retry verdict comes
from the engine's classification, never from message text (ADR-0017,
ADR-0502 §3). See [Errors](#errors-adr-0502-2). The native library is
bundled in the jar — see [Packaging and native loading](#packaging-and-native-loading-adr-0490).

## Lifecycle

`Database` and `Database.Connection` implement `AutoCloseable`:
`close()` disconnects / closes the handle once (idempotent — later calls
are no-ops) and nulls it, so a use after close fails loudly with
`"connection is closed"` / `"database is closed"` instead of touching a
freed handle. Without `close()` a file store cannot be reopened in the
same JVM; with it, the store reopens and reads the committed data
(`examples/Transaction.java`, `examples/Parameters.java`).

## Typed values (ADR-0502 §1)

Result cells decode through the ABI's typed value walker — one accessor
family per kind, no JSON. Every Cypher kind hydrates into a Java value:

| Cypher kind | Java value |
| --- | --- |
| `null` | `null` |
| `BOOLEAN` | `Boolean` |
| `INTEGER` | `Long` (the full int64 range, exactly — `RETURN 9007199254740993` hydrates as that `Long`) |
| `FLOAT` | `Double` (NaN/±Inf as themselves) |
| `STRING` | `String` (interior `\u0000` survives: the pointer+length read) |
| node | `Node` record — `elementId`, `id` (`long`), `labels` (`List<String>`), `properties` (`Map<String,Object>`, engine order) |
| relationship | `Relationship` record — `elementId`, `id`, `startElementId`, `startId`, `endElementId`, `endId`, `type`, `properties` |
| path | `Path` record — `elements` alternating node, relationship, …, node |
| `LIST` | `List<Object>` |
| `MAP` | `Map<String,Object>` (engine order; a user map shaped like a node is still a `MAP`). A key holding an interior `\u0000` throws `RelateException` — `relate_value_map_get` takes a NUL-terminated key, and the client refuses the read rather than return the value of whatever key the truncation lands on (string *values* keep their interior NULs) |
| `POINT` | `Point` record — `srid`, `x`, `y`, `z` (`null` on a 2D point) |
| `DATE` / `LOCAL_TIME` / `LOCAL_DATETIME` | `java.time.LocalDate` / `LocalTime` / `LocalDateTime` |
| `TIME` | `java.time.OffsetTime` (a TIME keeps no zone id, only its offset) |
| `DATETIME` | `java.time.ZonedDateTime` when the value keeps a zone id, `java.time.OffsetDateTime` when it is offset-only |
| `DURATION` | `DurationValue` record — `months`, `days`, `seconds`, `nanos` (`duration('P1Y2M')` reads `months = 14`) |
| `VECTOR` | `VectorValue` record — `dtype` (the engine's canonical name) and `coordinates` (`Long`s for `INTEGER*` dtypes, `Double`s for `FLOAT*`) |

Element ids are spelled exactly as Cypher's `elementId(n)` returns them,
so `WHERE elementId(n) = $id` round-trips through a bound parameter. The
`id` / `startId` / `endId` longs are the entities' legacy numeric ids
(ADR-0506) — the values `id(n)`, `id(r)`, `id(startNode(r))` and
`id(endNode(r))` return for the same entity; `elementId` stays the
primary identity, the numeric `id` is the deprecated form. A
value kind a later ABI adds (an enumerator this client does not know)
throws `RelateException("unsupported value kind <n>")` — defined and
reportable, never a silent `null`.

## Parameters (ADR-0502 §4)

`Connection.query(String cypher, Map<String, ?> params)` binds named
parameters through the typed params builder — one putter per Java type,
no JSON on the way:

```java
conn.query("MATCH (p:Person {name: $name, born: $born}) RETURN p",
        Map.of("name", "Ada", "born", LocalDate.of(1815, 12, 10)));
```

Bindable Java types: `null`, `Boolean`, `Byte`/`Short`/`Integer`/`Long`
(all bind as INTEGER), `Float`/`Double`, `String` (UTF-8, interior
`\u0000` survives), `java.time.LocalDate`, `LocalTime`, `OffsetTime`,
`LocalDateTime`, `OffsetDateTime`, `ZonedDateTime` (keeps its IANA zone; a
`ZoneOffset`-zoned one crosses as the offset form), `DurationValue` and
`java.time.Duration` (zero months/days), `Point`, `VectorValue`, `List`
(appends in order) and `Map` (named entries, nested to any depth). A Java
type outside the set throws `IllegalArgumentException` naming the type —
never a silent coercion. A putter refusal (an out-of-range component, an
unknown zone, a coordinate the dtype cannot hold) crosses the ABI's error
channel as a typed `RelateException` with stage `internal`. See
`examples/Parameters.java`.

## Errors (ADR-0502 §2)

Every failure crosses as one classified shape and throws a typed
exception — the engine's single classification table drives the class,
the client never sniffs message text:

| exception | when |
| --- | --- |
| `TransientException` | `retryable` is true (the write-write conflict `Neo.TransientError.Transaction.Outdated`, the retryable workspace cap) — the retry loop's cue |
| `ClientException` | `Neo.ClientError.*` (syntax, type, arithmetic, unknown namespace, a write against a read-only store) |
| `DatabaseException` | `Neo.DatabaseError.*`, including the boundary's `internal` stage (a poisoned handle, a caught panic, an undecodable argument) |
| `RelateException` (base) | a code outside the Neo4j classifications, or a client-side failure (native loading, a malformed ABI payload) — `getCode()` is then null |

All four carry `getCode()`, `getGqlStatus()`, `getStage()` and
`isRetryable()`. `getStage()` returns the closed enum
`RelateException.Stage` — `Stage.COMPILE` (a parse or plan failure),
`Stage.RUNTIME` (an execution failure) or `Stage.INTERNAL` (a failure the
engine never saw: a poisoned handle, a caught panic, an undecodable
argument); the ABI's wire names `"compile"` / `"runtime"` / `"internal"`
decode onto them. A stage value outside the three is an ABI contract
violation and throws at decode, never a lenient string. A conflict
can surface on either channel — the statement's error result (auto-commit)
or `commit()` (explicit transaction) — so a managed-transaction retry loop
catches `TransientException` in both places. A failed commit ends the
transaction (a later `rollback()` reports `no transaction`). After a
`Stage.INTERNAL` failure the handle may be poisoned: close it and open a
new one; `close()` still works on a poisoned handle.

## Bookmarks (ADR-0502 §7)

A bookmark is the engine's commit sequence, `"relate:<n>"` — `n` grows
monotonically across every connection and surface of the store, so a later
commit's bookmark sorts after an earlier one's. Compare the numeric `n`,
never the strings: the form is not lexicographically ordered
(`"relate:10"` sorts *before* `"relate:9"`).

- auto-commit write: `Result.getBookmark()` — `null` when the statement
  committed nothing (a read, an `EXPLAIN`, an empty row pipeline, any
  statement inside an explicit transaction).
- explicit transaction: `String Connection.commit()` — the bookmark, or
  `null` when the transaction committed nothing (no bookmark is
  fabricated). A failed commit throws instead.

## Packaging and native loading (ADR-0490)

Java 17+. Six jars per version, one Maven POM (dependencies: JNA,
jackson-databind):

| Coordinate | Natives carried |
| --- | --- |
| `ai.relatedb:relate:<ver>` | all five platforms (the fat jar, ~40 MB) |
| `ai.relatedb:relate:<ver>:osx-aarch_64` | `darwin-arm64` only |
| `ai.relatedb:relate:<ver>:osx-x86_64` | `darwin-x64` only |
| `ai.relatedb:relate:<ver>:linux-aarch_64` | `linux-arm64` only (glibc) |
| `ai.relatedb:relate:<ver>:linux-x86_64` | `linux-x64` only (glibc) |
| `ai.relatedb:relate:<ver>:windows-x86_64` | `win32-x64` only |

Every jar carries the same classes and the library skill; depend on the
fat jar **or** one classifier jar, not both. The classifiers are the
os-maven-plugin / Gradle osdetector vocabulary:

```xml
<!-- Maven: zero configuration -->
<dependency>
  <groupId>ai.relatedb</groupId>
  <artifactId>relate</artifactId>
  <version>0.1.0</version>
</dependency>

<!-- Maven: one platform, picked at build time -->
<build>
  <extensions>
    <extension>
      <groupId>kr.motd.maven</groupId>
      <artifactId>os-maven-plugin</artifactId>
      <version>1.7.1</version>
    </extension>
  </extensions>
</build>
<dependency>
  <groupId>ai.relatedb</groupId>
  <artifactId>relate</artifactId>
  <version>0.1.0</version>
  <classifier>${os.detected.classifier}</classifier>
</dependency>
```

```kotlin
// Gradle (Kotlin DSL)
dependencies { implementation("ai.relatedb:relate:0.1.0") }

// or one platform, via the osdetector plugin
plugins { id("com.google.osdetector") version "1.7.3" }
dependencies {
    implementation("ai.relatedb:relate:0.1.0:${osdetector.classifier}")
}
```

A deployable built for one target platform (a container image, say)
names that classifier literally instead of the build host's.

**Resource layout.** Each native sits at the classpath resource
`native/<key>/<library>`:

| `os.name` / `os.arch` | key | library | classifier |
| --- | --- | --- | --- |
| `Mac OS X` / `aarch64` | `darwin-arm64` | `librelate_ffi.dylib` | `osx-aarch_64` |
| `Mac OS X` / `x86_64` | `darwin-x64` | `librelate_ffi.dylib` | `osx-x86_64` |
| `Linux` / `aarch64` | `linux-arm64` | `librelate_ffi.so` | `linux-aarch_64` |
| `Linux` / `amd64` | `linux-x64` | `librelate_ffi.so` | `linux-x86_64` |
| `Windows*` / `amd64` | `win32-x64` | `relate_ffi.dll` | `windows-x86_64` |

`arm64` and `x86_64` are accepted as `os.arch` spellings of the same
architectures. Any other combination is unsupported.

**Load order.** The first use of `Database` resolves the library once per
JVM:

1. `RELATE_LIB`, when set and non-empty, is passed to JNA unchanged.
   A **development escape hatch** for a self-built library — not a
   deployment mechanism.
2. Otherwise the bundled resource for the running platform is extracted
   and loaded by absolute path.
3. Otherwise the first database call fails with a loud `RelateException`
   (the library resolves lazily, so the exception reaches every caller
   unwrapped, not just the first — no `ExceptionInInitializerError`):

   ```
   relate: no bundled native library for platform <key> (os.name=<os.name>, os.arch=<os.arch>): the classpath has no /native/<key>/<library>. Depend on the all-platform jar ai.relatedb:relate:<ver> or add the classifier artifact ai.relatedb:relate:<ver>:<classifier>. (RELATE_LIB=<path to a relate_ffi you built> overrides the bundled library for development only.)
   ```

   An unsupported platform fails earlier with
   `relate: unsupported platform os.name=<os.name>, os.arch=<os.arch>; bundled natives exist for darwin-arm64, darwin-x64, linux-arm64, linux-x64, win32-x64 only. …`
   (the key list is derived from the platform table itself).

The system library path, `java.library.path` and `jna.library.path` are
never searched for `relate_ffi`.

**Extraction cache.** The bundled library is written once to
`<cache>/<ver>/<key>-<first 16 hex of its SHA-256>/<library>` and reused
by every later JVM (the digest keeps a `-SNAPSHOT` rebuild apart from the
previous build). The write goes to a temp file in that directory, is
flushed and is renamed into place atomically, so concurrent JVMs never
load a partial file. `<cache>` is:

| Platform | Default cache root |
| --- | --- |
| macOS | `~/Library/Caches/relate/native` |
| Linux | `$XDG_CACHE_HOME/relate/native` (absolute `XDG_CACHE_HOME`), else `~/.cache/relate/native` |
| Windows | `%LOCALAPPDATA%\relate\native` |

`-Drelate.native.cache=<dir>` replaces the default root (read-only
home directories, `noexec` mounts). An unknown `user.home`, an unset
`LOCALAPPDATA` or an unwritable root fails with a `RelateException`
naming the property. Old versions' directories are never pruned;
deleting the cache directory is always safe.

**Java 24+.** JNA loads its dispatch library through JNI, so JEP 472
prints `WARNING: A restricted method in java.lang.System has been called`
on first use. Pass `--enable-native-access=ALL-UNNAMED` to the JVM to
acknowledge native access and silence it.

**Library skill.** Every jar carries the Agent Skills document
`.agents/skills/relate/SKILL.md` (the Python wheel's and npm
package's path), version-synced with the jar.
[library-skills.io](https://library-skills.io/) scans npm and PyPI only
today, so a JVM project copies it out of the jar by hand
(`unzip -p relate-<ver>.jar .agents/skills/relate/SKILL.md`).

**Building the jars.** `mvn -f clients/java/pom.xml package` from a
checkout:

| Property | Default | Meaning |
| --- | --- | --- |
| `relate.natives.dir` | unset | A directory holding `<key>/<library>` for all five keys (release / CI). Set: the fat jar carries all five and five classifier jars are built; the build fails unless all five files exist. |
| `relate.ffi.dir` | `../../target/release` | Without `relate.natives.dir`: the directory holding the build host's library (`cargo build -p relate-ffi --release`). The fat jar and the host's classifier jar carry it; the build fails when it is missing. |

`mvn verify` adds `PackagedJarIT`, which loads the built jars in child
JVMs with `RELATE_LIB`, `java.library.path`, `DYLD_LIBRARY_PATH` and
`LD_LIBRARY_PATH` removed, and runs
[`examples/BundledNative.java`](examples/BundledNative.java) and the
end-to-end example
[`examples/RelateE2E.java`](examples/RelateE2E.java). No workflow
publishes the jars to Maven Central yet.

**Measured jar sizes** (0.1.0, release natives): one native is ~24 MB
uncompressed and DEFLATE brings a jar to ~8.4 MB (darwin-arm64) / ~9.2 MB
(linux-arm64); the five-platform fat jar is ~40-42 MB. The jar carries the
engine only — the `relate_ffi` library links `relate` (engine +
parquet), `relate-model`, `relate-core` and `serde_json`; no CLI or
server-surface code rides along.

**JBang quick start** (until publishing exists, install the artifact into
the local Maven repository first — `cargo build -p relate-ffi
--release`, then `mvn -f clients/java/pom.xml install`):

```sh
jbang docs/api/java/examples/RelateE2E.java
```

The example resolves `ai.relatedb:relate` through JBang, loads the
bundled native for the running platform and runs queries, typed graph
entities, a `java.time` date property, a parameterised query and an
explicit transaction with its bookmark — with `RELATE_LIB` unset (it
refuses to run with it set, to keep proving the bundled loader).

**Cross-OS validation from a non-Linux host (Docker).** The same example,
built and run entirely in containers (validated 2026-11-03 on darwin-arm64
against linux-arm64, Temurin 17):

```sh
# 1. the engine library for the container's platform
docker run --rm -v "$PWD":/workspace -w /workspace \
  rust:1.88-bookworm cargo build --release -p relate-ffi
# 2. the jar, installed into a container-local Maven repository
docker run --rm -v "$PWD":/workspace -v "$HOME/.m2-docker":/root/.m2 \
  -w /workspace/clients/java \
  -e RELATE_LIB=/workspace/target/release/librelate_ffi.so \
  maven:3.9-eclipse-temurin-17 mvn -q clean install
# 3. the JBang example against that repository
docker run --rm -v "$HOME/.m2-docker":/root/.m2 -v "$PWD":/work -w /work \
  -e HOME=/root maven:3.9-eclipse-temurin-17 \
  bash -c 'curl -Ls https://sh.jbang.dev | bash -s - app setup >/dev/null
           && export PATH="$HOME/.jbang/bin:$PATH" && jbang /work/docs/api/java/examples/RelateE2E.java'
```

(Step 3 mounts the checkout at `/work` and points JBang at the repository
from step 2. `quay.io/jbangdev/jbang-action` does not work for this: it
runs linux/amd64 with a pre-17 JDK and cannot read the Java 17 class
files.)

## Result metadata (ADR-0285)

Every `Result` carries two plan-time stamps, read from the C ABI while the
native result is alive (`relate_result_query_type` /
`relate_result_namespace`; result-owned strings, copied by JNA):

- `Result.getQueryType()` — the statement's plan-time classification
  (ADR-0162): exactly `"r"` (read), `"w"` (a write with no returned
  columns), `"rw"` (a write with returned columns) or `"s"` (schema), the
  split per ADR-0332. `EXPLAIN` and `PROFILE` report the wrapped statement's
  type without executing it, so `EXPLAIN CREATE (n)` returns `"w"` and
  creates nothing. Any other value from the ABI is a contract violation: a
  `RelateException` reading `ABI contract violated: query type ...`.
- `Result.getNamespace()` — the statement's *resolved* namespace: exactly
  `"neo4j"`, `"system"` or `"system_ontologies"`. This is the `USE` target
  when the statement carries a `USE` clause, otherwise the session
  namespace; it never reflects a session-namespace change by itself. A value
  outside the three is a `RelateException` reading
  `ABI contract violated: namespace ...`.

Both refusals echo the offending value sanitized: control and other
non-printable characters appear as escapes and the value is bounded, so a
hostile native library cannot inject log lines (CWE-117). The same holds for
the notification severity/category and error-stage refusals, whose messages
also echo the payload sanitized.

See `examples/ResultMetadata.java`.

**Byte arrays (ADR-0327).** The engine's statement-scoped byte array enters only through Bolt or a Rust `Value::Bytes` parameter, never through this surface's own parameters. A result value holding one hydrates as a `List` of `Long`s 0–255 (the typed walker crosses a byte array as a LIST of INT; there are no column type stamps to read); byte arrays cannot be stored as properties.

## The structured plan (LABS-418, ADR-0381)

`Result.getPlan()` returns the statement's `relate.plan/v1` document as
typed values — the top-level `ai.relatedb` classes `PlanDocument`
(`schema`, `profiled`, `statement`, `root`),
`PlanNode` (`operator`, `details`, `identifiers`, `estimatedRows`, `actual`,
`children`, `warnings` — the operator's remedy-bearing notifications,
ADR-0363, joined by operator id), `PlanActual` (the PROFILE metrics) and
`PlanStart` — or `null` on a plain run. Read from
`relate_result_plan_json` while the result is alive. The plan family
is a public-field struct the lenient plan mapper binds (unknown keys
tolerated, LABS-836); `PlanActual` models every key of the wire `actual` object:
`rows`, `timeMs`, `totalTimeMs`, `dbHits`, `seekMs`, `walkMs`, `loops`
(`Long`, null at the root), `memoryBytes`, `expressionBuiltBytes`, `hops`,
`neighborWalks`, `nodeReads`, `batched`, `zoneSkips`, `frontier`
(`PlanFrontier`: `edgeVisits`, `levels`, `endpoints`, `cells`, `memoHits`,
`memoMisses`, `representativeProofs`, `searchProven`, `searchDisproven`,
`declinedExhausted`, `budgetFraction`), `frontierDecline` (nullable
`String`), `start` and `kernel` (`PlanKernel`: `rung`, `hops` — a `Long`, present for the chain
rung only and null otherwise; nullable).

## Query notifications (ADR-0363)

`Result.getNotifications()` returns the statement's query notifications as a
`List<Notification>` (the top-level `ai.relatedb.Notification` class),
read from `relate_result_notifications` (a
result-owned JSON array string, copied by JNA) while the native result is
alive. The list is always present and empty on a plain run: derivation is
opt-in with the plan tree, so `EXPLAIN` (and `PROFILE`) populate it while
`RETURN 1` does not.

Each `Notification` carries the engine-native fields — the
same keys the JSON handle API's result envelope uses under `notifications`:

| getter | native key | meaning |
| --- | --- | --- |
| `getCode()` | `code` | the kind's code, e.g. `Neo.ClientNotification.Statement.CartesianProduct` |
| `getGqlStatus()` | `gql_status` | the GQLSTATUS value, e.g. `03N90` |
| `getSeverity()` | `severity` | `Severity` enum: `INFORMATION` or `WARNING` (wire name = constant name; any other value throws `RelateException`) |
| `getCategory()` | `category` | `Category` enum: `PERFORMANCE`, `UNRECOGNIZED`, `DEPRECATION`, `SCHEMA` or `HINT` (any other value throws `RelateException`) |
| `getTitle()` | `title` | the kind's short title, e.g. `Cartesian product` |
| `getDescription()` | `description` | the instance diagnosis (unknown name, index text, …) |
| `getRemedy()` | `remedy` | the action that clears the diagnostic |
| `getOperatorId()` | `operator_id` | the raising plan operator's id (`Long`), `null` on statement-scoped kinds |

The list is deterministic: operator-scoped entries follow the plan's
pre-order, then statement-scoped entries in table order, and identical
entries collapse to the first. It never carries a `position` key. A
malformed value from the ABI raises `RelateException` rather than
dropping the entry.

```java
Result explained = conn.query("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b");
for (Notification n : explained.getNotifications()) {
    System.out.println(n.getSeverity() + " " + n.getGqlStatus() + " "
            + n.getCode() + ": " + n.getDescription() + " Remedy: " + n.getRemedy());
}
conn.query("RETURN 1 AS n").getNotifications(); // empty
```

See `examples/ResultMetadata.java`.

## Namespaces (ADR-0117)

Every store has three fixed namespaces: `neo4j`, `system` and
`system_ontologies` (ADR-0116). The namespace is the session default for
statements that carry no `USE` clause. `system_ontologies` is visible only
under `RELATE_PREVIEW=ontology` (ADR-0593): without the opt-in
`connect`/`setNamespace` with it fail with the same `Graph not found`
rejection as an unknown name, and `SHOW DATABASES` lists the two CE rows.

```java
Connection conn = db.connect("system"); // connect() == connect("neo4j")
conn.setNamespace("NEO4J");             // names match case-insensitively
```

- `Database.connect(String namespace)` → `relate_connect_namespace`;
  `Connection.setNamespace(String)` → `relate_conn_set_namespace`.
- An unknown name throws `RelateException` with a message ending
  `Graph not found: <name>` (ADR-0146).
- `setNamespace` is refused while an explicit transaction is open, with a
  message ending `Cannot change the namespace while a transaction is open.`
