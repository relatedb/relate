# SNOTEL Explorer (iOS)

A SwiftUI + MapKit app that embeds Relate in-process to explore snowpack
(snow water equivalent, SWE) at ~50 western-US SNOTEL stations, grouped into
river basins. No server, no network: the graph lives in the app's
Application Support directory as a Relate store (WAL + Parquet
checkpoint).

It is the example consumer of the Swift surface (ADR-0433,
[`docs/api/swift`](../../docs/api/swift/spec.md)); CI builds it in the `swift`
job.

## What it shows

- **Live queries** — the views declare what they show and never ask for a
  refresh. The map's stations and basins and the insights counts are
  `@LiveQuery` properties (`MapScreen`, `InsightsView`) over the statements
  in `SNOTELQueries`; Relate reads them when the view appears and again
  after a write commits. Add a station in the Cypher workbench and it is on
  the map when the sheet closes. Changing the filter gives the view a
  different statement, and the live query starts again on it. A read that
  fails (a workbench station without the properties `Station` decodes, say)
  ends its query: the map keeps what it showed, a banner says why, and the
  next filter change starts the query again.
- **Durable embedded store** — first launch loads the bundled CSV/GeoJSON and
  checkpoints; later launches reopen the checkpoint and skip the load. The
  app checkpoints again when it goes to the background.
- **Batched, all-or-nothing load** — one parameterized `UNWIND $rows` per
  dataset, all in the one transaction of a single `database.write([…])`,
  together with the completion marker the next launch checks, so an
  interrupted load never reads as loaded.
- **Graph traversal** — `(:Station)-[:HAS_READING]->(:Reading)` and
  `(:Station)-[:IN_BASIN]->(:Basin)`.
- **Native point functions** — `point.distance` (nearby radius, distance
  column) and `point.withinBBox`, backed by a `POINT INDEX` that a
  `Migrator` declares and creates when the store opens.
- **Tracing** — the Query Log is fed by `Database.setTraceHandler`: every
  statement run once the store is open, the live queries' reads included
  (the schema statements of the open itself come first and are not logged),
  with its engine-reported time, query type (READ / WRITE / SCHEMA), row
  count and the names of its parameters. A trace carries parameter names,
  not values.
- **Swift 6 concurrency** — reads and writes run on connections Relate
  manages (`Database.read`, `Database.write`, and the live queries built on
  them), and the open and its migration run off the `MainActor`. The
  `@MainActor` view model holds only UI state: the filter, the selection
  and where setup stands.
- **Typed values** — rows decode into `Decodable` models
  (`result.decode(Station.self)`), the bundled CSV rows bind as one
  `Encodable` list parameter, and map coordinates bind as typed points.

> **Note** Basin membership (`IN_BASIN`) is computed in Swift at load time
> with a point-in-polygon test, because the Cypher surface is points only:
> `point.distance` and `point.withinBBox` are native, and basin geometry
> travels as a stored GeoJSON property for the map overlay.

## Requirements

- Xcode 16+ (Swift 6 language mode), iOS 17+ simulator or device
- The Rust toolchain pinned by `rust-toolchain.toml`, plus the Apple targets:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios \
  aarch64-apple-darwin x86_64-apple-darwin
```

## Build and run

The app builds against the engine in this checkout, not a released binary,
so build the XCFramework first and set `RELATE_LOCAL_XCFRAMEWORK=1`
(the root `Package.swift` reads it):

```sh
# From the repository root: everything CI runs, including this app
make test-swift

# Or step by step
scripts/build-xcframework.sh
export RELATE_LOCAL_XCFRAMEWORK=1
cd examples/ios-snotel && swift test          # core library tests on the Mac
xcodebuild -project SNOTELExplorer.xcodeproj -scheme SNOTELExplorer \
  -destination 'generic/platform=iOS Simulator' build
```

In Xcode: quit it first (`open --env` only reaches a newly launched
process), then `open -a Xcode --env RELATE_LOCAL_XCFRAMEWORK=1
SNOTELExplorer.xcodeproj` so package resolution sees the variable, and press
**Cmd+R**. Until the first release stamps a binary into `Package.swift`, the
local XCFramework is used either way. The project depends on the repository-root `Relate` package
(`../..`).

## Layout

```
SNOTELExplorer/
  Data/          bundled stations + readings CSV, basins GeoJSON
  Models/        Station, Basin, DatabaseStats, SWELevel, DemoQuery, QueryLogEntry
  Services/      DataLoader (CSV/GeoJSON, point-in-polygon), DatabaseService
                 (open, load, checkpoint, workbench), SNOTELQueries (the reads)
  ViewModels/    MapViewModel (@MainActor, @Observable): UI state only
  Views/         MapScreen (the live queries), map, filter panel, insights,
                 query workbench, query log, the live-query error banner
Tests/           Swift Testing suites for the SNOTELCore library target
Package.swift    SNOTELCore library + tests (everything except Views and the App)
```

## Using Relate from Swift

A view that shows data declares its statement; the root view provides the
database:

```swift
import Relate
import SwiftUI

struct BasinStations: View {
    @LiveQuery private var stations: [StationName]

    init(basin: String) {
        // The interpolated value is bound as a parameter. A different basin
        // is a different statement, so the query starts again on it.
        _stations = LiveQuery(
            try "MATCH (s:Station)-[:IN_BASIN]->(:Basin {name: \(basin)}) RETURN s.name AS name ORDER BY name")
    }

    var body: some View {
        List(stations, id: \.name) { Text($0.name) }
            .overlay { if $stations.isLoading { ProgressView() } }
    }
}

// once, at the root
ContentView().relateDatabase(db)
```

Outside a view, the same reads and writes are `async` calls:

```swift
import Relate

struct StationName: Decodable, Sendable, Equatable { let name: String }

let db = try Database(url: storeURL)            // or Database.inMemory()
let basin = "Pacific Northwest"
let result = try await db.read(
    try "MATCH (s:Station)-[:IN_BASIN]->(:Basin {name: \(basin)}) RETURN s.name AS name")
for station in try result.decode(StationName.self) { print(station.name) }
print(result.queryType, result.elapsedMs, result.stats.nodesCreated)

try await db.write { tx in                      // one transaction
    try tx.run("MATCH (s:Station {name: \("Bogus Basin")}) SET s.visited = true")
}
```

`\(basin)` in a `Statement` literal is bound as a parameter, never spliced
into the text. The synchronous `db.connect()` / `conn.query(_:_:)` API is
there too, for code that wants to own its connection.

Data: the station list and April 1 readings are a bundled sample derived
from USDA NRCS SNOTEL; basin polygons are simplified HUC2-region outlines for
demonstration, not survey-grade boundaries.
