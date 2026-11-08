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

let db = try Database(path: storeURL.path)       // or Database.inMemory()
let conn = try db.connect()                      // connect(namespace: "system")
let r = try conn.query("MATCH (n:Person {name: $name}) RETURN n", ["name": "Ada"])
r.columns; r.rows; r.records                     // [String], [[CypherValue]], [[String: CypherValue]]
r.queryType      // .read | .write | .readWrite | .schema   (ADR-0162)
r.namespace      // .neo4j | .system | .systemOntologies, resolved (ADR-0117)
r.stats          // QueryStats, the 11 counters
r.notifications  // [] on a plain run; EXPLAIN / PROFILE populate it
r.plan           // PlanDocument? (relate.plan/v1)
r.bookmark       // String?, the auto-commit write's bookmark (ADR-0502)
r.elapsedMs      // engine time; r.parseMs / r.planMs / r.executeMs — its
                // phase split (ADR-0491), sum never exceeding the total
try conn.transaction { tx in try tx.query("CREATE (:A)") }  // rollback on throw (body or commit)
try db.checkpoint()
```

`CypherValue` is an enum — `null bool int float string list map node
relationship path point date time localTime dateTime localDateTime duration
vector` — one case per ABI value kind; every value carries its kind
(`value.kind`), so there is no per-column type stamp. Literals convert
implicitly, variables via `CypherValue(x)`; the temporal / point / vector
cases construct from their components (`.date(.init(year: 2026, month: 1,
day: 2))`, `.point(.init(srid: 4326, x: -105.0, y: 40.0, z: nil))`).

## Gotchas

- **`queryType` / `namespace` are closed enums** — a value outside the
  vocabulary throws `RelateError.decoding` (`ABI contract violated: ...`).
- A result map key containing an interior NUL is refused with an error (the C ABI looks keys up NUL-terminated); string values keep their NULs.
- **`Connection` is not `Sendable`** — the compiler stops you sharing one
  across actors. Own it inside one actor; `Database` is shareable, open one
  connection per concurrent worker.
- **Blocking calls** — queries run synchronously; never call them on the
  `MainActor`.
- **Every value is typed** — nodes and relationships carry element ids
  (`"4:relate:n0"`, `elementId(n) = $id` round-trips) plus the
  deprecated numeric ids Cypher's `id()` returns (`node.id`,
  `rel.id`/`rel.startId`/`rel.endId`, ADR-0506 — use them for entity
  equality, not lookups), temporals arrive
  as components (not ISO strings — render in the app), points and vectors as
  their structs; non-finite floats stay `.float`. A map that looks like a
  node stays a `.map`.
- **Parameters are typed, not JSON** — the params builder binds temporals,
  points, vectors, durations, `NaN`/`±Infinity` and full-range integers
  directly; nodes, relationships, paths and NUL-bearing names are refused
  (`RelateError.invalidParameter`).
- **Errors are typed** — every throw carries `code`, `gqlStatus`, `stage`
  (`compile`/`runtime`/`internal`) and `retryable` (`error.engineError`);
  retry exactly when `retryable`, never on message text.
- **One writer per store** — a second writable `Database(path:)` on the same
  directory throws; use `Database(readOnlyPath:)` for extra readers.
- **iOS storage** — keep the store in Application Support with
  `completeUntilFirstUserAuthentication` protection; checkpoint after bulk
  loads.
- **No polygon geometry support** — the engine's geospatial surface is the
  point: `point.distance` and `point.withinBBox` are native, and line/polygon
  work belongs in Swift (`MapKit`, `shapely`-style geometry) over properties.

## Spec

`docs/api/swift/spec.md` (types, value mapping, lifecycle, build flags);
runnable examples in `docs/api/swift/examples`; full iOS app in
`examples/ios-snotel`.
