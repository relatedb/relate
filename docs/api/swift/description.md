# Swift client — description

`Relate` is the embed API for iOS and macOS apps (ADR-0433). It links the
engine in-process as a static XCFramework, so an app gets a durable openCypher
graph with no server and no network: point `Database(path:)` at a directory in
the app's container and the store survives relaunches through its WAL and
Parquet checkpoints.

The package is a thin, hand-written Swift layer over the same tabular C ABI v2
the JavaScript, Java and Go clients wrap: the typed value walker decodes every
cell into a typed `CypherValue` (temporals, points, vectors and durations as
Swift values, not strings to re-parse), the typed params builder binds them
back without JSON, and every failure arrives as one typed error shape carrying
the Neo4j code, GQLSTATUS, stage and retryable flag. Every `QueryResult`
carries the shared result metadata — query type, resolved namespace, mutation
counters, notifications, the structured plan, the statement's bookmark and the
engine's elapsed time with its `parseMs` / `planMs` / `executeMs` phase split
(ADR-0491) — so a Swift caller can tell reads from writes, see which namespace
a `USE`-routed statement ran in, show `EXPLAIN`/`PROFILE` plans, and see which
phase a slow statement spent its time in.

Values meet the platform's own types at the edge: a `Foundation.Date` binds
as a `DATETIME` and reads back as the same instant, a `[Float]` embedding
binds as a `FLOAT32` vector, a point is built from a latitude and longitude
or a `CLLocationCoordinate2D`, and a Core ML output array becomes a vector
directly. An app stores what it has without formatting it into text first.

Results read the same way. A `QueryResult` is a collection of records, and a
record's typed accessor (`try record.get("title", as: String.self)`) converts
strictly, naming the column, the type asked for and the kind found when they
disagree. Parameters take Swift values as they are: strings, numbers,
optionals, arrays, dates and an app's own raw-value enums.

A `Statement` is the value form of a query: text plus parameters, built
once, comparable and safe to pass between actors. Written as a string
literal it binds whatever it interpolates as a parameter, so a value can
never become part of the statement text. `Codable` models go both ways: an
`Encodable` value becomes a map parameter, and records, nodes and relationships
decode into `Decodable` types by column or property name.

The concurrency model is the C ABI's, checked by the Swift 6 compiler: a
`Database` may be shared across tasks and actors, a `Connection` may not. An
app does not have to manage that itself. `try await db.read { … }` and
`try await db.write { … }` run a synchronous body on connections the
database owns — one writer and a small set of readers, each on its own
queue — so a view model can await them from the `MainActor`, a list can
keep reading while an import writes, and no transaction is ever held across
a suspension. The synchronous `connect()` API remains for command-line tools,
tests and code that wants to own its connection, as the iOS example in
[`examples/ios-snotel`](../../../examples/ios-snotel/) shows.

For iOS apps, keep the store in Application Support (not Documents), give the
directory `FileProtectionType.completeUntilFirstUserAuthentication` if the app
writes while locked, and call `checkpoint()` after bulk loads so the next
launch opens from a snapshot instead of replaying the WAL. An app and its
extensions that share a store put it in their App Group container; only one
process may hold the store writable at a time.
