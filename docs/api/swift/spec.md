# Swift client — spec

Package: `Relate` (SwiftPM, repository root `Package.swift`, sources in
`clients/swift`), ADR-0433. Platforms: iOS 16+, macOS 13+. Swift 6 language
mode. Wraps the tabular C ABI v2 ([`../c/spec.md`](../c/spec.md)) only: the
typed value walker, the typed params builder, the error channel that
travels with the call, and bookmarks.

```swift
// Package.swift of a consumer
// pin a release tag that carries the Swift binary (releaseTag in Package.swift is non-empty)
.package(url: "https://github.com/relatedb/relate", from: "<release>")
// target dependency
.product(name: "Relate", package: "relate")
```

## Types

```swift
public final class Database: @unchecked Sendable {
    public init(path: String) throws            // ":memory:" or a store directory (created if missing)
    public init(readOnlyPath path: String) throws // existing store, loaded once; writes/checkpoint refused (ADR-0136)
    public static func inMemory() throws -> Database
    public let path: String
    public let isReadOnly: Bool
    public func connect() throws -> Connection                    // namespace "neo4j"
    public func connect(namespace: String?) throws -> Connection  // "neo4j" | "system" | "system_ontologies" (ADR-0117)
    public func checkpoint() throws
}

public final class Connection {                  // NOT Sendable
    public let database: Database
    public func query(_ cypher: String, _ parameters: [String: CypherValue] = [:]) throws -> QueryResult
    public func setNamespace(_ name: String) throws   // only between transactions
    public func begin() throws
    @discardableResult public func commit() throws -> String?  // the transaction's bookmark; nil when it committed nothing
    public func rollback() throws
    public func transaction<T>(_ body: (Connection) throws -> T) throws -> T
}

public struct QueryResult: Sendable {
    public let columns: [String]
    public let rows: [[CypherValue]]
    public let bookmark: String?                 // ADR-0502 §7: the auto-commit write's bookmark ("relate:<n>");
                                                 // nil for a read, an empty write pipeline, EXPLAIN, a statement
                                                 // inside an explicit transaction, or an error result
    public let queryType: QueryType              // ADR-0162: .read "r" | .write "w" | .readWrite "rw" | .schema "s"
    public let namespace: Namespace              // ADR-0117: .neo4j | .system | .systemOntologies (USE target, else session)
    public let stats: QueryStats                 // ADR-0216: the canonical 11 counters
    public let notifications: [Notification]     // ADR-0363: [] on a plain run
    public let plan: PlanDocument?               // ADR-0381: relate.plan/v1; nil on a plain run
    public let elapsedMs: Double                 // ADR-0221: engine parse + plan + execute
    public let parseMs: Double                   // ADR-0491: parse + lowering window
    public let planMs: Double                    // ADR-0491: binding + planning window
    public let executeMs: Double                 // ADR-0491: dispatch (or EXPLAIN render) window
                                                 // parseMs + planMs + executeMs <= elapsedMs
    public var records: [[String: CypherValue]] { get }
    public func scalar() throws -> CypherValue   // exactly one row and one column
}

public enum CypherValue: Sendable, Hashable, CustomStringConvertible {
    case null, bool(Bool), int(Int64), float(Double), string(String)
    case list([CypherValue]), map([String: CypherValue])
    case node(Node), relationship(Relationship), path(Path)
    case point(Point), date(Date), time(Time), localTime(LocalTime)
    case dateTime(DateTime), localDateTime(LocalDateTime)
    case duration(Duration), vector(Vector)
}

public enum CypherValueKind: UInt32, Sendable, Hashable, CaseIterable {
    case null, bool, int, float, string, node, relationship, path, list, map
    case point, date, time, localTime, dateTime, localDateTime, duration, vector
}

public enum RelateError: LocalizedError, Sendable, Equatable {
    case engine(EngineError)       // the typed error shape (below)
    case invalidParameter(String)  // refused before reaching the engine
    case decoding(String)          // the C ABI broke its documented contract
}

public struct EngineError: Sendable, Hashable {
    public let code: String        // "Neo.ClientError.Statement.ArithmeticError" — pass through unchanged
    public let gqlStatus: String   // the GQLSTATUS, e.g. "22G03"
    public let stage: Stage        // .compile | .runtime | .internal (a boundary failure)
    public let retryable: Bool     // the drivers' transient test: retry exactly when true
    public let message: String
    public enum Stage: String, Sendable, Hashable { case compile, runtime, internal }
}
```

`CypherValue` conforms to every `ExpressibleBy…Literal` protocol, so literal
parameters need no wrapping (`["n": 1, "tags": ["a"], "m": ["k": true]]`);
variables use `CypherValue(x)` (`Int`, `Double`, `String`, `Bool`) or the
typed struct initializers for the temporal / point / vector cases. Accessors:
`boolValue`, `intValue`, `doubleValue` (int widened), `stringValue`,
`listValue`, `mapValue`, `nodeValue`, `relationshipValue`, `pathValue`,
`pointValue`, `dateValue`, `timeValue`, `localTimeValue`, `dateTimeValue`,
`localDateTimeValue`, `durationValue`, `vectorValue`, `kind`, `isNull`, and
`subscript(key:)` over maps and node/relationship properties.

`Node` is `{id, elementId, labels, properties}` and `Relationship` is
`{id, elementId, type, startId, endId, startElementId, endElementId,
properties}` — the element ids are the engine's own spelling
(`"4:relate-neo4j:n0"`), exactly what `elementId(n)` returns, so
`WHERE elementId(n) = $id` round-trips through a string parameter. The
numeric `id` / `startId` / `endId` (`Int64`) are the deprecated numeric
entity ids (ADR-0506): exactly the INTEGERs Cypher's `id()` /
`id(startNode(r))` / `id(endNode(r))` return for the same entities — read
them for driver-style entity equality, keep `elementId()`-based lookups on
the element ids. `Path` is `{nodes, relationships}`
(`nodes.count == relationships.count + 1`, in walk order). `Point` is
`{srid, x, y, z?}` (`z` only for the 3D srids 9157/4979; `x`/`y` are
longitude/latitude for the geographic srids 4326/4979). `Date`, `LocalTime`,
`LocalDateTime` carry their components; `Time` adds `offsetSeconds` (a TIME
keeps no zone id — the ABI's contract); `DateTime` adds `offsetSeconds` and
`zone: String?` (`nil` for offset-only values). `Duration` is
`{months, days, seconds, nanoseconds}` — the four lossless components
(`PT36H` stays 36 hours, not 1½ days). `Vector` is
`case floats(dtype:values:)` / `case ints(dtype:values:)` with the eight
canonical dtype names (`INTEGER8`…`FLOAT`) as a closed `DType` enum.
`QueryStats`, `Notification`, `PlanDocument`, `PlanNode`, `PlanActual` and
`PlanStart` carry the same fields as the Go client's types, in camelCase.

## Value decoding

Cells decode through the C ABI's typed value walker (ADR-0502 §1:
`relate_value_at` + the `relate_value_*` accessors) — there is no
wire JSON and no per-column type stamp. Every value carries its kind at every
depth: a map that merely looks like a node stays `.map`, a path nested in a
list stays `.path`, and `value.kind` (``CypherValueKind``) reads it back. An
enumerator this client does not know (a later, additive ABI) is a defined
`.decoding` error, never a silent null. The result metadata is closed the same
way: a `query_type` outside `r`/`w`/`rw`/`s` or a `namespace` outside the three
fixed names is a `.decoding` error reading `ABI contract violated: ...`.

| ABI kind | Swift value |
| --- | --- |
| NULL / BOOL / INT / FLOAT / STRING | `.null` / `.bool` / `.int` / `.float` / `.string` (`1.0` stays `.float`; NaN and ±∞ stay `.float`) |
| NODE / RELATIONSHIP / PATH | `.node` / `.relationship` / `.path` |
| LIST / MAP | `.list` / `.map` — a map key holding an interior `\u{0}` throws `.decoding`: `relate_value_map_get` takes a NUL-terminated key, so the client refuses rather than return another key's value (string values keep their NULs) |
| POINT / DATE / TIME / LOCAL_TIME / DATETIME / LOCAL_DATETIME / DURATION / VECTOR | the typed structs above |

Strings and map keys carry a byte length, so interior NULs cross whole
(`'a\u0000b'` is `.string("a\u{0}b")`); a present key holding `null`
(`.map(["a": .null])`) is distinct from an absent key. UUIDs cross as
`STRING`, byte arrays as `LIST` of `INT`.

## Parameters

`query(_:_:)` binds `parameters` through the C ABI's typed params builder
(ADR-0502 §4) — one putter per value, no JSON, so temporals, points,
vectors, durations, non-finite floats and full-range integers all bind
directly (`["d": .date(...)]`, `["v": .vector(.ints(dtype: .integer8,
values: [1, 2, 3]))]`, `["f": .float(.nan)]`); an empty dictionary binds
none. `.node`, `.relationship` and `.path` values are refused with
`RelateError.invalidParameter` (pass their element id or properties), as
are a query, path, namespace, parameter name or map key containing a NUL
byte (C strings would truncate it). A putter refusal the engine's builder
makes (an impossible calendar date, an unknown zone, a srid/dimension
mismatch) surfaces as the typed `.engine` error with stage `internal`.

## Errors

Every failure the C ABI reports crosses as one typed shape
(``EngineError``): `code`, `gqlStatus`, `stage`, `retryable`, `message` —
classified by the engine's own single table, so the client never sniffs
message text. Branch on `code` / `stage`; retry exactly when `retryable`
(today: the write-write conflict and the retryable workspace cap). A
statement failure rides the result (`RETURN 1/0` throws
`.engine(EngineError(code: "Neo.ClientError.Statement.ArithmeticError",
gqlStatus: "22G03", stage: .runtime, retryable: false, …))`); every other
call failure fills that call's own `error_json` out-param. `stage ==
.internal` means a boundary failure (a NULL or poisoned handle, a caught
panic, an argument that failed to decode): do not retry on it — release the
handle and open a new one.

## Lifecycle and concurrency

- A `Database` closes when its last reference is released; every
  `Connection` holds its database, so the store outlives its connections.
  A `Connection` disconnects when released.
- `Database` is `Sendable`: share one across tasks and actors. `Connection` is
  not: Swift 6 rejects sending it across isolation domains, which is the C
  ABI's "do not share a connection across threads" rule. Own each connection
  in one actor; open more connections for concurrent work.
- Calls are synchronous and block while the engine runs. Call them from a
  background actor, not the `MainActor`.
- Errors travel with the call that failed (the v2 error channel — ADR-0502
  §2): a query run from any task reports its own error to whoever reads its
  throw. There is no thread-local slot, so nothing depends on the calling
  thread.
- A panic inside the engine poisons the handle (C ABI contract): later calls
  throw `.engine` with stage `internal`; release it and open a new one.
- `transaction(_:)` commits when the body returns and rolls back, then
  rethrows the body's error, when it throws; a throwing `commit()` inside it
  likewise rolls back and rethrows the commit's error. `commit()` returns the
  transaction's bookmark, or `nil` when the transaction committed nothing; a
  `retryable` conflict can surface on a statement's error result
  (auto-commit) or on `commit()` — a retry loop must check both.

## Building and linking

The binary target `RelateFFI` is a static XCFramework
(`ios-arm64`, `ios-arm64_x86_64-simulator`, `macos-arm64_x86_64`) carrying
the C header and a `CRelate` module map. Its module map links `iconv` and
`CoreFoundation`. The `Relate` target ships `PrivacyInfo.xcprivacy`
(no collection, no tracking; file-timestamp API reason `C617.1`).

| Binary source | When |
| --- | --- |
| `binaryTarget(url:checksum:)` — the release asset `RelateFFI.xcframework.zip`, tag and checksum stamped by the release workflow | a consumer resolving a release tag |
| `binaryTarget(path: "target/apple/RelateFFI.xcframework")` | `RELATE_LOCAL_XCFRAMEWORK=1`, or no release stamped yet |

### `scripts/build-xcframework.sh`

```sh
scripts/build-xcframework.sh                 # all slices
scripts/build-xcframework.sh --slices macos  # macOS only (swift test)
scripts/build-xcframework.sh --zip           # also target/apple/RelateFFI.xcframework.zip
```

| Flag / env | Default | Meaning |
| --- | --- | --- |
| `--slices all\|macos` | `all` | `all`: iOS device, iOS simulator (arm64 + x86_64), macOS (arm64 + x86_64); `macos`: the macOS slice only |
| `--zip` | off | Write the release zip next to the XCFramework (`ditto -c -k --keepParent`) |
| `-h`, `--help` | — | Print usage |
| `IPHONEOS_DEPLOYMENT_TARGET` | `16.0` | iOS floor the static libraries are built for; matches `Package.swift` |
| `MACOSX_DEPLOYMENT_TARGET` | `13.0` | macOS floor; matches `Package.swift` |
| `XCFRAMEWORK_CARGO_PROFILE` | `release` | Cargo profile name for `relate-ffi` (not the Makefile's `CARGO_PROFILE` flag) |

Requires the Rust targets `aarch64-apple-darwin x86_64-apple-darwin
aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios` (macOS: the first
two); the script names the missing ones.

### `scripts/stamp-swift-package.sh`

```sh
scripts/stamp-swift-package.sh v0.14.0 target/apple/RelateFFI.xcframework.zip
```

| Positional | Meaning |
| --- | --- |
| `TAG` | Release tag (`vMAJOR.MINOR.PATCH[-pre]`) written to `releaseTag` |
| `ZIP` | Release zip whose `swift package compute-checksum` is written to `releaseChecksum` |

Fails if either constant is not declared exactly once in `Package.swift`.
Release-workflow use only.

### Make targets

| Target | Runs |
| --- | --- |
| `make build-xcframework` | `scripts/build-xcframework.sh` |
| `make test-swift` | the XCFramework build, `swift test`, the examples, the release-stamp harness (`scripts/tests/test_stamp_swift_package.sh`), the iOS example's core tests and its iOS Simulator build — the CI `swift` job |
| `make swift-examples` | `swift run` of each `docs/api/swift/examples` executable |

> **Note:** a fresh `swift test` compile fails with
> `plugin for module 'TestingMacros' not found` on hosts running only
> CommandLineTools (no full Xcode) — the Swift Testing macro plugin is not
> auto-loaded there. Once the suite has compiled once (e.g. via
> `make test-swift` on a full-Xcode host), `swift test --skip-build` runs it
> against the already-compiled bundle.

## Examples

[`examples/`](examples/) is a SwiftPM package; each executable asserts its
output (`quickstart`, `transactions`, `result-metadata`, `namespaces`,
`persistence`). The iOS app [`examples/ios-snotel`](../../../examples/ios-snotel/)
is the end-to-end consumer.
