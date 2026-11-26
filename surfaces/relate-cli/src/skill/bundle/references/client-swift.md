# Swift client (iOS / macOS): Relate package

## Install

SwiftPM, from the repository root manifest (product `Relate`; iOS 16+,
macOS 13+, Swift 6):

```swift
.package(url: "https://github.com/relatedb/relate", from: "<release>")
// target: .product(name: "Relate", package: "relate")
```

A release tag resolves a prebuilt static XCFramework (checksum-pinned in the
tagged `Package.swift`). Until the first release that stamps one, or when
building from a checkout, run `scripts/build-xcframework.sh` and set
`RELATE_LOCAL_XCFRAMEWORK=1` (Xcode: quit it, then `open -a Xcode --env RELATE_LOCAL_XCFRAMEWORK=1 <project>`).

## Minimal usage

```swift
import Relate

let db = try Database(url: storeURL)             // or Database.inMemory(); readOnly: true for an existing store
let conn = try db.connect()                      // connect(namespace: .system)
let r = try conn.query("MATCH (n:Person {name: $name}) RETURN n", ["name": name])  // Swift values bind as they are
for record in r { let n: CypherValue.Node = try record.get("n") }  // typed, strict; Optional accepts null
try r.single(); try r.scalar(as: Int.self)       // shape-checked
try conn.run("MATCH (b:Book {key: \(key)}) SET b += \(try CypherValue(encoding: model))")  // Statement: \(value) is BOUND ($_p0)
let books = try r.decode(Book.self)              // Decodable by column; decode(_:column:) for a node
r.columns; r.rows; r.records                     // [String], [[CypherValue]], [Record] (record.dictionary for a plain map)
r.queryType      // .read | .write | .readWrite | .schema   (ADR-0162)
r.namespace      // .neo4j | .system | .systemOntologies, resolved (ADR-0117)
r.stats          // QueryStats, the 11 counters
r.notifications  // [] on a plain run; EXPLAIN / PROFILE populate it
r.plan           // PlanDocument? (relate.plan/v1)
                 // a seek-started MATCH nests display-only fused children (ADR-0622) — ordinary `children` entries; the wire plan marks them with `FusedInto`; this document adds `fused_into`/`fused_metrics` (null on a real operator)
r.bookmark       // String?, the auto-commit write's bookmark (ADR-0502)
r.elapsedMs      // engine time; r.parseMs / r.planMs / r.executeMs — its
                // phase split (ADR-0491), sum never exceeding the total
try conn.transaction(retries: 2) { tx in try tx.query("CREATE (:A)") }  // rollback on throw; retries re-run on a retryable conflict
let n = try conn.read { tx in try tx.query("MATCH (n) RETURN count(n)").scalar(as: Int.self) }  // one snapshot; always rolled back
try db.checkpoint()

// Async, from any actor: the database manages one writer + a few readers.
let n = try await db.read { tx in try tx.run("MATCH (n) RETURN count(n)").scalar(as: Int.self) }
try await db.write { tx in try tx.run("CREATE (:A {name: \(name)})") }   // one transaction
try await db.write(try "CREATE (:A {name: \(name)})")                    // single statement

db.setTraceHandler { t in log(t.statement, t.wallMs) }                    // every statement; parameter names, not values. t.statement holds any literal spliced into it and t.error may quote a value: log those two only when they carry no user data
for await commit in db.commits() where commit.tags.contains("notes") { reload() }  // writes via db.write(tags: ["notes"])
for try await n in db.values({ tx in try tx.run("MATCH (n) RETURN count(n)").scalar(as: Int.self) }) { show(n) }  // now, then after each commit that changes it

// SwiftUI: the view is redrawn when a commit changes the answer. Root: ContentView().relateDatabase(db)
@LiveQuery("MATCH (b:Book) RETURN b.title AS title ORDER BY title") private var books: [Book]   // $books.isLoading, $books.error
```

`CypherValue` is an enum — `null bool int float string list map node
relationship path point date time localTime dateTime localDateTime duration
vector` — one case per ABI value kind; every value carries its kind
(`value.kind`), so there is no per-column type stamp. Literals convert
implicitly, variables via `CypherValue(x)`; the temporal / point / vector
cases construct from their components (`.date(.init(year: 2026, month: 1,
day: 2))`) or from platform types:

```swift
try CypherValue(Date())                               // UTC DATETIME, microsecond precision
value.dateTimeValue?.foundationDate                   // back to a Foundation.Date (optional)
try CypherValue(vector: floats)                       // [Float] → FLOAT32 vector; finite, at most 4,096 coordinates
value.vectorValue?.float32Values                      // back to [Float], exact
try CypherValue.Point(latitude: 40.0, longitude: -105.0)  // srid 4326; refuses a coordinate off the globe
try CypherValue(CLLocationCoordinate2D(...)); point.coordinate
try CypherValue.Vector(mlMultiArray)                  // a Core ML output, any shape
```

## Gotchas

- **`queryType` / `namespace` and notification `severity` / `category` are
  closed enums** — a value outside the vocabulary throws
  `RelateError.decoding` (`ABI contract violated: ...`). The offending value
  echoed into the message is sanitized: non-printable characters arrive escaped
  and the value is bounded.
- A result map key containing an interior NUL is refused with an error (the C ABI looks keys up NUL-terminated); string values keep their NULs.
- **Prefer `db.read` / `db.write` in apps** — bodies are synchronous
  closures run off the caller's thread on pooled connections; safe to await
  from the `MainActor`. Reads are snapshots that never commit (a write in one
  throws `misuse`); writes run one at a time. The body and its result must be
  `Sendable`; the `Transaction` does not leave the body. A running statement
  cannot be cancelled.
- **Observation is in-process** — `commits()`, `values` and `@LiveQuery`
  and the trace handler see only this `Database` object's writes and
  statements, not another process's. The engine does not say which labels a
  commit touched: tag writes (`tags:`) to route them.
- **Live values re-read on every commit by default** — `values` and
  `@LiveQuery` run their read again after any commit, and (for an
  `Equatable` value) skip a result equal to the last. Pass `tags:` only
  when every writer of that data tags its writes; an untagged write then
  leaves the value stale. A `@LiveQuery` that depends on view input is
  built in the view's `init`
  (`_books = LiveQuery(try "… \(search) …")`).
- **`Connection` is not `Sendable`** — the compiler stops you sharing one
  across actors. If you use `connect()` directly, own the connection inside
  one actor; `Database` is shareable.
- **Blocking calls** — `Connection` calls run synchronously; never call them
  on the `MainActor`.
- **Every value is typed** — nodes and relationships carry element ids
  (`"4:rl-id:n0"`, `elementId(n) = $id` round-trips) plus the
  deprecated numeric ids Cypher's `id()` returns (`node.id`,
  `rel.id`/`rel.startId`/`rel.endId`, ADR-0506 — use them for entity
  equality, not lookups), temporals arrive
  as components (not ISO strings — bridge with `foundationDate`, or render
  in the app), points and vectors as their structs; non-finite floats stay `.float`. A map that looks like a
  node stays a `.map`.
- **A transaction body gets a `Transaction`** — it runs statements only;
  it is invalid once the body returns. A write inside `read` throws
  `RelateError.misuse` and is not kept. A body passed with `retries` may run
  more than once.
- **Poison** — after an error with stage `internal`, `conn.isPoisoned()`
  tells whether the connection must be replaced (`db.connect()` again).
  `db.read` / `db.write` do this themselves.
- **`run` binds, `query` does not** — an interpolated literal passed to
  `run(_:)` is a `Statement` and its values become parameters; the same
  literal passed to `query(_:_:)` is a plain `String` with the values in the
  text. Use `\(identifier: label)` for labels and property keys and
  `\(raw: text)` only for text the program wrote.
- **Typed reads are strict** — `record.get(_:as:)` widens an integer to a
  float and converts nothing else (a string is never parsed into a number,
  `null` needs an `Optional`). A mismatch throws
  `RelateError.unexpectedResult` with the column, expected type and found
  kind. `[Float]` binds as a list; a vector is `CypherValue(vector:)`.
- **Parameters are typed, not JSON** — the params builder binds temporals,
  points, vectors, durations, `NaN`/`±Infinity` and full-range integers
  directly; nodes, relationships, paths and NUL-bearing names are refused
  (`RelateError.invalidParameter`).
- **Errors are typed** — calls are `throws(RelateError)`, so a `catch`
  block's `error` needs no cast. Every engine error carries `code`,
  `gqlStatus`, `stage` (`compile`/`runtime`/`internal`) and `retryable`
  (`error.engineError`); branch on `isConstraintViolation`, `isSyntaxError`,
  `isMissingParameter`, `isWriteConflict` or `classification`, and retry
  exactly when `retryable`, never on message text.
- **Metadata type names** — `QueryNotification`, `NotificationSeverity`,
  `NotificationCategory`, `DatabaseNamespace` (not `Notification` /
  `Namespace`, which collide with Foundation and SwiftUI).
- **One writer per store** — a second writable `Database` on the same
  store throws; use `Database(url:readOnly: true)` for extra readers.
- **iOS storage** — `Database.defaultStoreURL(named:)` (Application
  Support); `Database.provision(at:from:)` copies a bundled store there on
  first launch (never open a store inside the app bundle); `Migrator` for
  idempotent schema plus versioned steps; `checkpointInBackground()` after
  bulk loads and on backgrounding.
- **No polygon geometry support** — the engine's geospatial surface is the
  point: `point.distance` and `point.withinBBox` are native, and line/polygon
  work belongs in Swift (`MapKit`, `shapely`-style geometry) over properties.
- **Statement timeout (ADR-0692):** `try conn.setStatementTimeout(.seconds(5))` bounds each statement on the connection (`nil` clears; the default is none). A statement past it throws `.engine` with code `…TransactionTimedOutClientConfiguration`; the connection stays usable.

## Spec

`docs/api/swift/spec.md` (types, value mapping, lifecycle, build flags);
runnable examples in `docs/api/swift/examples`; the iOS task guide is
`docs/guides/ios-apps.md`; full iOS app in `examples/ios-snotel`.
