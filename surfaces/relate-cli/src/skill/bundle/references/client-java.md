# Java client: com.relate:relate

## Install

```xml
<dependency>
  <groupId>com.relate</groupId>
  <artifactId>relate</artifactId>
  <version>0.1.0</version>
</dependency>
```

Gradle: `implementation("com.relate:relate:0.1.0")`. Java 17+;
wraps the C ABI via JNA. The jar bundles the native library for all five
supported platforms — no environment variable, no library path.

The jars are not on Maven Central yet; until then build them from a repo
checkout: `cargo build -p relate-ffi --release`, then
`mvn -f clients/java/pom.xml install` (bundles the build host's library).

## Minimal usage

```java
import com.relate.Database;

try (Database db = Database.open(":memory:");          // or "./mydata" / "./task.rl"
     Database.Connection conn = db.connect()) {         // or db.connect("system")
    Database.Result result = conn.query("RETURN $n AS n", java.util.Map.of("n", 1));
    // result.columns(): List<String>        // ["n"]
    // result.rows(): List<List<Object>>     // [[1L]] — positional, typed values
    // result.getQueryType(): QueryType READ | WRITE | READ_WRITE | SCHEMA (ADR-0162)
    // result.getNamespace(): Namespace NEO4J | SYSTEM | SYSTEM_ONTOLOGIES (resolved)
    // result.getBookmark(): "relate:<n>" after an auto-commit write, else null
    // result.getNotifications(): List<Notification> — [] on a plain run
    // result.getPlan(): PlanDocument | null — EXPLAIN / PROFILE only
}
```

Cells hydrate typed (the value walker of ABI v2, ADR-0502): `Long` /
`Double` / `Boolean` / `String` / `null` / `List` / `Map`, nodes and
relationships as `Node` / `Relationship` records (element id — the primary
identity — plus the deprecated legacy numeric `id` / `startId` / `endId`
longs of ADR-0506, labels/type, properties), paths as `Path`, temporals
as `java.time` values, durations
as `DurationValue` (months, days, seconds, nanos), points and vectors as
`Point` / `VectorValue`. `conn.begin()` / `commit()` / `rollback()` are an
explicit transaction; `commit()` returns its bookmark (or null when the
transaction committed nothing). `Database.openReadOnly(path)` refuses
writes and checkpoints (the store must exist). Errors throw typed
exceptions — see gotchas.

## Gotchas

- **Metadata vocabularies are closed enums** — a `getQueryType()` /
  `getNamespace()` / `Notification` severity and category value outside the documented set throws
  `RelateException` (`ABI contract violated: ...`), like the Go client. The
  offending value echoed into the message is sanitized — non-printable
  characters arrive escaped and the value is bounded.
- **Native library resolution** — the bundled resource
  `native/<key>/<library>` for the running platform is extracted once per
  version into the user cache (`~/Library/Caches/relate`,
  `~/.cache/relate` or `$XDG_CACHE_HOME`, `%LOCALAPPDATA%\relate`)
  and loaded from there. `-Drelate.native.cache=<dir>` moves the cache
  (read-only home, `noexec` mounts). `RELATE_LIB` overrides the bundled
  library — a development escape hatch only. The system library path is
  never searched.
- **Five platforms only** — `darwin-arm64`, `darwin-x64`, `linux-arm64`,
  `linux-x64` (glibc), `win32-x64`. Anything else fails at first use with
  `relate: unsupported platform os.name=…, os.arch=…`.
- **Classifier jars for smaller deployables** — `osx-aarch_64`,
  `osx-x86_64`, `linux-aarch_64`, `linux-x86_64`, `windows-x86_64` (the
  os-maven-plugin `${os.detected.classifier}` / Gradle osdetector
  vocabulary). Each carries one platform's native; depend on the fat jar
  **or** one classifier jar. A classpath missing the running platform's
  native fails with `relate: no bundled native library for platform
  <key> … add the classifier artifact com.relate:relate:<ver>:<classifier>`.
- **Java 24+ warning** — JEP 472 prints a "restricted method" warning when
  JNA loads native code; `--enable-native-access=ALL-UNNAMED` silences it.
- **Typed ABI v2 required** — the client checks
  `relate_abi_version()` at first use; an older library fails with
  `incompatible librelate_ffi …`.
- **Interior-NUL map keys are refused** — a `String` value keeps its
  interior `\u0000`, but a map *key* holding one throws
  `RelateException`: the C ABI's `map_get` takes a NUL-terminated key,
  and the client refuses the read rather than silently return the value of
  whatever key the truncation lands on.
- **Bookmarks order numerically** — `"relate:<n>"` is not
  lexicographically ordered (`"relate:10"` sorts before
  `"relate:9"`); compare the numeric `n`.
- **Parameter binding is typed** — `query(cypher, Map<String,?>)` binds
  `Long`, `Double`, `Boolean`, `String`, `null`, `List`, `Map`,
  `java.time` temporals, `DurationValue`, `Point` and `VectorValue` (one
  putter per type). An unsupported Java type throws
  `IllegalArgumentException`; never concatenate untrusted input into the
  statement text.
- **Errors are typed, never message-sniffed** — the engine's code +
  GQLSTATUS drive the class: `TransientException` exactly when
  `isRetryable()` is true (the write-write conflict, the retryable
  workspace cap), `ClientException` for `Neo.ClientError.*`,
  `DatabaseException` for `Neo.DatabaseError.*`; every exception carries
  `getCode()`, `getGqlStatus()`, `getStage()` (the closed enum
  `RelateException.Stage`: `COMPILE` / `RUNTIME` / `INTERNAL`) and
  `isRetryable()`.
- **Close what you open** — `Database` and `Connection` are
  `AutoCloseable`; without `close()` a file store cannot be reopened in
  the same JVM. A failed commit ends the transaction (no rollback needed).
- **Library skill in the jar** — `.agents/skills/relate/SKILL.md`
  ships in every jar; library-skills.io scans npm/PyPI only, so copy it
  out by hand (`unzip -p <jar> .agents/skills/relate/SKILL.md`).
- **One writer process per database** — the CLI's single-writer rule
  applies; `:memory:` is ephemeral.

Full contract: `docs/api/java/spec.md` (packaging, load order, error text,
types, namespaces).
