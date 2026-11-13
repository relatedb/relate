# ADR-0433: Reinstate the Swift surface for iOS and macOS

Status: Proposed
Claim: 71e4363e046303fb89ee1174f735c09a9b6509b1
Later references: the §6 one-release-commit shape is split by ADR-0572 into
a bump commit (the release cut) and a Swift stamp commit (the tag target);
the checksum contract itself is unchanged. ADR-0596 adds managed
asynchronous access, typed records and parameters, Foundation bridges and
renames four public types; it supersedes this ADR's design record on two
points ("synchronous core API; apps call it from their own actor", and the
deferral of `Codable` row decoding). The concurrency contract in item 3
stands.

## Context

ADR-0065 withdrew the Swift surface (root `Package.swift`, `bindings/swift`,
`examples/ios-snotel`, the XCFramework scripts) because no CI gate built it.
Its reinstatement clause: Swift may return "under a new ADR that commits to a
CI gate from day one — … a Swift package built against `cypherlite-ffi`".
ADR-0084 reinstated WASM the same way. This ADR is that new ADR for Swift.

Three things changed since the withdrawal:

- The withdrawn wrapper bound the v1 JSON handle API (`cypherlite_open` /
  `cypherlite_execute`), which the typed C ABI v2 removed (ADR-0502). The
  JS, Java and Go clients bind the v2 tabular API
  (`cypherlite_database_*`, `cypherlite_connect*`, `cypherlite_query_params`,
  `cypherlite_result_*`) and the v2 value walker, which carries the result metadata AGENTS.md §2
  keeps aligned across surfaces: query type (ADR-0162), resolved namespace
  (ADR-0117), stats (ADR-0216), notifications (ADR-0363), plan (ADR-0381),
  column types (ADR-0224), elapsed time (ADR-0221).
- Nodes and relationships carry a `kind` discriminator on the wire
  (ADR-0223), so a typed Swift value can be decoded without shape-sniffing.
- `cypherlite-ffi`, Parquet included, cross-compiles unchanged for
  `aarch64-apple-ios`, `aarch64-apple-ios-sim` and `x86_64-apple-ios` on the
  pinned 1.88 toolchain.

The design work is in `docs/design/swift-apple-platforms.md`.

## Decision

Reinstate Swift as a supported client surface of the tabular C ABI.

1. **Package.** One SwiftPM package, product `CypherLite`, manifest at the
   repository root (SwiftPM resolves a git dependency from the root), sources
   and tests in `clients/swift`. Swift 6 language mode; platforms iOS 16 and
   macOS 13. The wrapper is hand-written Swift over the C ABI. It is not
   generated, and it adds no protocol of its own.
2. **Binary.** `CypherLiteFFI.xcframework` holds static `libcypherlite_ffi.a`
   slices for `ios-arm64`, `ios-arm64_x86_64-simulator` and
   `macos-arm64_x86_64`. Each slice carries the one C header
   (`surfaces/cypherlite-ffi/include/cypherlite.h`, staged, never copied into
   the tree) and a `CCypherLite` module map under `Headers/CCypherLite/`.
   `scripts/build-xcframework.sh` is the only build entry. CI, `make`, the
   release workflow and the example all call it.
3. **Concurrency contract.** `Database` is `Sendable` and `Connection` is
   not. That is the C ABI's rule ("share a database across threads; do not
   share a connection"), enforced by the Swift 6 compiler. The wrapper adds
   no lock. Results are copied into a `Sendable` `QueryResult` before the C
   result is destroyed. A statement failure is read from the result
   (`cypherlite_result_is_error`, `cypherlite_result_error_json`); every
   other failure is read from the call's trailing `char **error_json`
   out-param (ADR-0502 §3). Both are read in the same synchronous frame as
   the failing call, and the wrapper copies the error into a Swift error
   before the C result is destroyed or the out-param is freed. No
   thread-local error slot is consulted.
4. **Values.** `CypherValue` is built by the C ABI v2 value walker
   (`cypherlite_value_at`, then `cypherlite_value_kind` and the per-kind
   accessors), not by decoding JSON. The kind enum decides each case at
   every depth, so nodes and relationships are recognized by kind, never by
   shape. Temporals, points, durations and vectors arrive as their typed
   kinds, and paths as `PATH`. Nodes, relationships and non-finite floats
   are refused as parameters with a typed error.
5. **CI gate.** A `swift` job on `macos-latest` in
   `.github/workflows/ci.yml` builds the three-slice XCFramework, runs
   `swift test` for the package, runs the `docs/api/swift/examples`, runs
   the `examples/ios-snotel` core tests, and builds the example app for the
   iOS Simulator. A failure blocks the merge.
6. **Distribution.** The release workflow's `version` job runs on macOS,
   builds the XCFramework, zips it, and stamps the release tag and the zip's
   SwiftPM checksum into `Package.swift`
   (`scripts/stamp-swift-package.sh`). All of that lands in the one release
   commit the `tag` job tags. The same zip is attached to the GitHub
   release. `Package.swift` uses `binaryTarget(url:checksum:)` for a
   consumer, and `binaryTarget(path:)` on the locally built
   `target/apple/CypherLiteFFI.xcframework` when
   `CYPHERLITE_LOCAL_XCFRAMEWORK=1` is set (or before the first release).
   The switch is an environment variable because SwiftPM caches manifest
   evaluation by manifest content and environment, so a file-existence check
   goes stale.
7. **Privacy manifest.** The `CypherLite` target ships
   `PrivacyInfo.xcprivacy`. It declares no data collection and no tracking,
   and declares the file-timestamp required-reason API category (reason
   `C617.1`), because the engine `stat`s files in its own store. That is the
   only required-reason category left in a dead-stripped release link.
8. **Docs.** `docs/api/swift/{spec.md,description.md,examples/}` and the
   skill reference `references/client-swift.md` with its SKILL.md row,
   per AGENTS.md §2.

## Alternatives considered

- **UniFFI-generated bindings.** Rejected: that is a second, generated ABI
  beside the C ABI the other clients use. AGENTS.md has clients wrap the C
  ABI only, with no parallel protocol.
- **Bind the JSON handle API again.** Rejected: it lacks the result metadata
  every other client exposes, which breaks the §2 alignment rule.
- **A separate distribution repository** (Mozilla's
  `rust-components-swift` pattern). Rejected for now: it splits the CI gate
  from the code it gates and adds a cross-repo release step. The cost of the
  root manifest is that SwiftPM clones the whole repository (~40 MB pack).
  Revisit if that becomes a complaint.
- **A dynamic framework.** Rejected: every app would have to embed and sign
  it, with no benefit for a single-consumer library.
- **Rebuild the XCFramework deterministically after the version commit**
  (the withdrawn `prepare-swift-release.sh` approach). Rejected: it needs
  byte-reproducible rustc static libraries and Info.plist canonicalization
  across runners. Building once, inside the job that writes the commit,
  makes the stamped checksum true by construction.
- **Move iOS onto the macOS `F_BARRIERFSYNC` commit barrier.** Rejected
  here: on iOS `sync_all` is `F_FULLFSYNC`, which is stronger, and whether
  the commit barrier should be the weaker variant at all is ADR-0077's open
  question. iOS stays on `F_FULLFSYNC` until that question is decided.

## Consequences

- The supported-surface invariant from ADR-0065 holds: the surface is in the
  tree because CI builds it. Deleting the `swift` job supersedes this ADR.
- CI pays for one macOS job on ready-for-review runs; drafts skip it, like
  the other heavy lanes.
- A release's `version` job moves to `macos-latest` and grows by the
  five-target XCFramework build.
- Between releases, `Package.swift` points at the previous release's binary.
  A checkout that builds from source runs `scripts/build-xcframework.sh`
  and sets `CYPHERLITE_LOCAL_XCFRAMEWORK=1`. CI and the Makefile targets
  always do.
- The XCFramework is not code-signed. Signing needs an Apple Developer
  identity in release secrets and is a follow-up. Xcode accepts unsigned
  binary dependencies but cannot verify their origin.
- The polygon geometry functions (`spatial.fromWKT`, `spatial.within`, …)
  are absent from the executor, so the iOS example materializes basin
  containment in Swift. That is an engine gap, not part of this decision.
- ADR-0065 gets a *Later references* note pointing here.
