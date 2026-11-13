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

The concurrency model is the C ABI's, checked by the Swift 6 compiler: a
`Database` may be shared across tasks and actors, a `Connection` may not. The
idiomatic shape is one actor that owns a connection and exposes app-level
queries, with the `@MainActor` view model awaiting it — the pattern of the
iOS example in [`examples/ios-snotel`](../../../examples/ios-snotel/).

For iOS apps, keep the store in Application Support (not Documents), give the
directory `FileProtectionType.completeUntilFirstUserAuthentication` if the app
writes while locked, and call `checkpoint()` after bulk loads so the next
launch opens from a snapshot instead of replaying the WAL. An app and its
extensions that share a store put it in their App Group container; only one
process may hold the store writable at a time.
