# Swift on iOS and macOS — plan

Status: implemented (2026-10-28) under ADR-0433 (Proposed); §6 has the
deltas from the plan. The ADR file supersedes the §8 draft.

## 1. Where we are

- ADR-0065 withdrew the Swift surface (root `Package.swift`,
  `bindings/swift`, `examples/ios-snotel`, the XCFramework scripts) because no
  CI gate built it. Its reinstatement clause: Swift may return "under a new
  ADR that commits to a CI gate from day one — … a Swift package built
  against `relate-ffi`". ADR-0084 is the precedent (WASM came back the
  same way).
- The withdrawn wrapper bound the **legacy JSON handle API**
  (`relate_open` / `relate_execute` → one JSON envelope). The Go,
  Java and JS clients now bind the **tabular API** (`relate_database_*`,
  `relate_connect*`, `relate_query_params`, `relate_result_*`)
  and expose result metadata — query type, resolved namespace, stats,
  notifications, plan, elapsed — which AGENTS.md §2 requires to stay aligned
  across every surface.
- Feasibility: `cargo +1.88 build -p relate-ffi --release --target
  aarch64-apple-ios{,-sim}` builds cleanly today, parquet/arrow included
  (~74 MB static archive per slice before the app linker dead-strips).
- Storage: `host_durability_barrier` uses `F_BARRIERFSYNC` only under
  `target_os = "macos"`; iOS falls through to `sync_all`, which Rust std
  maps to `F_FULLFSYNC` on Apple targets — durable, but a slower barrier
  than macOS gets (ADR-0077's "cheapest correct barrier" is not met).

## 2. Best practices this plan follows

| Practice | Source / rationale |
| --- | --- |
| Rust → C ABI → hand-written idiomatic Swift (no UniFFI) | AGENTS.md: clients wrap the C ABI only, no parallel protocol; Mozilla application-services and Ferrostar both ship this XCFramework shape |
| One XCFramework of **static** libs: `ios-arm64`, `ios-arm64_x86_64-simulator` (lipo), `macos-arm64_x86_64` (lipo) | Apple's multi-platform binary format; static avoids embed/sign steps in the app |
| Header + `module.modulemap` live *inside* the XCFramework, sourced from `surfaces/relate-ffi/include/relate.h` | the withdrawn layout copied the header into `bindings/swift` — a second copy that drifts (“parallel state is a bug”) |
| `IPHONEOS_DEPLOYMENT_TARGET` / `MACOSX_DEPLOYMENT_TARGET` set at cargo time to the package floor | avoids “built for newer iOS than being linked” linker warnings |
| SwiftPM `binaryTarget(url:checksum:)` for releases; `binaryTarget(path:)` for local dev, toggled by env/file presence | standard SwiftPM distribution; SwiftPM resolves git dependencies from the repo-root `Package.swift` |
| Signed XCFramework + `PrivacyInfo.xcprivacy` (required-reason API audit: file timestamps via `stat`, disk space if any) | App Store third-party-SDK requirements (Xcode 15+ verifies signatures; manifests enforced since May 2024) |
| Swift 6 language mode; `Database` is `Sendable`, `Connection` is **not** | maps the C contract (“share a database across threads; do not share a connection”) onto compile-time checking instead of a runtime lock. `Database` is `@unchecked Sendable` only because it stores an `OpaquePointer`; the C ABI is what makes it thread-safe. No `Mutex` (which would force iOS 18/macOS 15) |
| Synchronous core API; apps call it from their own actor | no fake `async` over blocking FFI; the example's `DatabaseService` actor is the pattern |
| A failure is read from the failing call itself — the result's `relate_result_error_json` or the call's own `error_json` out-param, copied before the next call | ADR-0502 removed the thread-local error slot, so no `await` between call and error read can lose the message |
| Results materialized eagerly into a `Sendable` struct, C result destroyed in `defer` | same shape as the Go client; nothing borrowed outlives `relate_destroy_result` |
| Store in `Application Support`, `FileProtectionType.completeUntilFirstUserAuthentication`, checkpoint on `scenePhase == .background` | iOS can suspend/kill a backgrounded app; App Group container if an extension shares the store |

## 3. Swift API shape (target)

```swift
import Relate

let db = try Database(path: url.path)          // or Database.inMemory(), Database(readOnlyPath:)
let conn = try db.connect()                     // connect(namespace: "system")
let r = try conn.query("MATCH (s:Station {id:$id}) RETURN s", ["id": "1050:CO:SNTL"])
r.columns; r.rows                               // [String], [[CypherValue]]
r.queryType; r.namespace; r.stats; r.notifications; r.plan; r.elapsedMs; r.columnTypes
try conn.transaction { tx in try tx.query("CREATE (:A)") }   // rollback on throw
try db.checkpoint()
```

`CypherValue`: `Sendable, Hashable` enum over the canonical wire JSON
(null, bool, int, float, string, list, map, node, relationship, path, point,
vector, temporal/uuid/binary as their wire strings), with
`ExpressibleBy…Literal` for parameters. Errors: `RelateError` carrying the
`last_error` text.

## 4. Layout

```
Package.swift                      thin root manifest (SwiftPM needs it at repo root)
clients/swift/Sources/Relate/  the wrapper
clients/swift/Tests/RelateTests/
scripts/build-xcframework.sh       the one build entry (make, CI, example all call it)
target/apple/RelateFFI.xcframework   build output (gitignored, under target/)
examples/ios-snotel/               SwiftPM core lib + tests, Xcode app project
docs/api/swift/{spec,description}.md, examples/
surfaces/relate-cli/src/skill/bundle/references/client-swift.md (+ SKILL.md row)
```

## 5. Phases (stacked PRs)

1. **ADR** (§8) — Proposed → Accepted by a human.
2. **Build + wrapper** — `scripts/build-xcframework.sh`, root `Package.swift`,
   `clients/swift` wrapper + `swift test` suite (values, params, metadata,
   transactions, errors, persistence round-trip, read-only), `make
   build-xcframework` / `make test-swift`.
3. **Docs + skill** — `docs/api/swift/*`, runnable examples compiled in CI,
   `client-swift.md` + SKILL.md selection row (BUNDLE_FILES drift guard).
4. **CI gate** — a `swift` job on `macos-latest`: build the macOS slice,
   `swift test`, `swift test` the example core, `xcodebuild build` the
   example for `generic/platform=iOS Simulator`. Full three-slice build on
   ready-for-review only.
5. **iOS example** — `examples/ios-snotel` restored from `c9152549^`, ported
   to the new API, Swift 6 mode, DB in Application Support, Query Log shows
   query type / elapsed / counters from result metadata.
6. **Release** — `ffi-apple` release job: build, codesign, deterministic zip,
   checksum stamped into `Package.swift`, attached to the GitHub release;
   privacy manifest audit.
7. **Engine follow-up** — extend the `F_BARRIERFSYNC` arm to
   `target_vendor = "apple"` (iOS/visionOS/Catalyst) with a test; note in an
   ADR-0077 later-reference.

Deferred: Mac Catalyst and visionOS slices, Swift Package Index listing,
`Codable` row decoding, an async streaming result.

Later: ADR-0596 (an idiomatic layer over the Swift surface) takes up
`Codable` row decoding and replaces "synchronous core API; apps call it from
their own actor" (§2) with managed asynchronous read and write entries on
`Database`. The synchronous API remains underneath.

## 6. Status (2026-10-28)

Implemented, uncommitted, under ADR-0433 (`docs/adr/0433-reinstate-swift-surface.md`;
the number is not minted yet — `scripts/adr_mint.py` needs push access):
phases 1–6. Phase 7 is dropped: iOS keeps `F_FULLFSYNC`, see the ADR's
alternatives (ADR-0077's open question decides the barrier, not this surface).

Two findings changed the design during implementation:

- SwiftPM caches manifest evaluation keyed on the manifest and the
  environment, so the local-vs-release binary switch is the
  `RELATE_LOCAL_XCFRAMEWORK=1` variable, not a file-existence check.
- The release `version` job builds the zip and stamps its checksum into the
  release commit, so no reproducible rebuild is needed.

Engine gap found while porting the example: the geometry types and their
helpers were dropped from the executor in `579592c9` (`eval/spatial.rs`), but
`relate-front`'s function allowlist and `references/spatial.md` still
advertised them until ADR-0497 removed the names from both. The Cypher surface
is points only — `point.distance` and `point.withinBBox` — so the app's
point-in-polygon test runs in Swift, and basin geometry travels as a GeoJSON
property.

## 7. Open questions

- Deployment floor: iOS 16 / macOS 13 proposed (no `Mutex` needed).
- Repo-root `Package.swift` vs a separate `relate-swift` distribution
  repo (Mozilla's choice) — root proposed: one repo, one tag, and the CI gate
  sits next to the code it gates.

## 8. ADR draft — “Reinstate the Swift surface”

Not minted (minting pushes a ledger commit; do it when the file lands).

**Context.** ADR-0065 withdrew Swift for lack of a gate and set the
reinstatement clause. The C ABI has since gained the tabular API and result
metadata the other clients expose. The engine cross-compiles for iOS
unchanged.

**Decision.** Reinstate Swift as a client of the tabular C ABI: a SwiftPM
package (`Relate`, root `Package.swift`, sources in `clients/swift`)
over a static `RelateFFI.xcframework` built only by
`scripts/build-xcframework.sh`; a `swift` CI job on macOS builds it, runs
`swift test`, and builds `examples/ios-snotel` for the iOS Simulator from
day one; the surface ships `docs/api/swift` and `client-swift.md`.

**Alternatives considered.**
- *UniFFI-generated bindings* — rejected: a second, generated ABI next to the
  C ABI the other clients use; AGENTS.md forbids a parallel protocol.
- *Bind the legacy JSON handle API again* — rejected: it lacks the result
  metadata every other client exposes (§2 alignment rule).
- *Separate distribution repo* — rejected for now: splits the gate from the
  code; revisit if repo-root `Package.swift` clone cost becomes a problem.
- *Dynamic framework* — rejected: needs embed + sign in every app for no
  benefit to a single-consumer library.

**Consequences.** macOS CI minutes for the `swift` job; a release job for
the XCFramework is a follow-up ADR; deleting the `swift` job supersedes this
ADR; ADR-0065 gets a Later-references note.
