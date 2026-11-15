# Changelog

All notable changes to Relate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.2] — 2026-11-14

### Added

- Swift: managed asynchronous access. `try await db.read { tx in … }` and
  `try await db.write { tx in … }` run a synchronous body off the caller's
  thread on connections the database owns (one writer, up to
  `Options.maxReaders` readers), with single-statement forms and
  `checkpointInBackground()`. A poisoned pooled connection is replaced.
- Swift: `Database.setTraceHandler` reports every statement with its
  timings and parameter names, not values (its statement text holds any
  literal spliced into it, and its error may quote a value), and every
  statement is an `os_signpost` interval. `Database.commits()` is an
  `AsyncStream` of
  committed writes with their bookmark, summed counters and the tags the
  writer attached (`db.write(tags:)`, `transaction(tags:)`).
- Swift: `Database(url:readOnly:)`, `connect(namespace:)` and
  `setNamespace(_:)` taking the typed namespace, and error predicates on
  `EngineError` (`isConstraintViolation`, `isSyntaxError`,
  `isMissingParameter`, `isWriteConflict`, `classification`) plus
  `RelateError.isRetryable`.
- Swift: bridges between `CypherValue` and platform types.
  `Foundation.Date` to and from `DATETIME`, `LOCAL DATETIME` and `DATE`;
  `Swift.Duration`; `[Float]` and Core ML arrays to `FLOAT32` vectors;
  points from a latitude and longitude or a `CLLocationCoordinate2D`.
  The bridges check their input and refuse, with `invalidParameter`, a date
  outside the years -999,999,999 through 999,999,999, a UTC offset beyond
  18 hours, date components that are not a calendar day, a duration beyond
  `Int64` seconds, a latitude or longitude off the globe, and a vector with
  a non-finite coordinate or more than 4,096 coordinates.
- Swift: `CypherValueConvertible`, so parameter dictionaries take Swift
  values (strings, numbers, optionals, arrays, dates, raw-value enums)
  without wrapping. A `QueryResult` is a collection of `Record`s with strict
  typed accessors (`record.get("title", as: String.self)`), plus `single()`
  and `scalar(as:)`. `QueryResult` is `Equatable`.
- Swift: `Statement`, a value holding statement text and parameters. As a
  string literal it binds interpolated values as parameters
  (`try conn.run("MATCH (b {key: \(key)})")`), with `\(identifier:)` and
  `\(raw:)` for what is not a value.
- Swift: `CypherValueEncoder` and `CypherValueDecoder`. An `Encodable`
  model binds as a map; rows, nodes and relationships decode into
  `Decodable` types (`result.decode(Book.self)`).
- Swift: `Connection.transaction(retries:_:)` re-runs its body on a
  retryable conflict; `Connection.read(_:)` runs a consistent snapshot that
  is always rolled back; `runInTransaction(_:retries:)` runs a list of
  statements all-or-none; `Connection.isPoisoned()` probes for a poisoned
  handle.

### Changed

- `fs-to-graph` commits each rows record as its own transaction instead of
  one transaction around the whole import (ADR-0602): an import whose
  total payload exceeded the 64 MB WAL record cap (a large checkout at
  default flags) no longer fails at commit with nothing landed — every
  bounded batch commits as it lands. A failed import now keeps the
  batches that already committed, and re-running the same command
  completes it; a session's bounds are the per-call caps (1000 rows /
  8 MB per rows record) and the store's live-graph limit — the whole-run
  stream cap applies to whole-import framing only (ADR-0598).
- A `MATCH`-clause `WHERE` that reads the path (`length(p)`, `relationships(p)`,
  a relationship variable) now filters the selected paths afterwards for the
  keyword shortest selectors (`SHORTEST 1`, `ANY SHORTEST`, `ALL SHORTEST`,
  `SHORTEST 1 GROUPS`), as Neo4j does; it no longer promotes a longer path
  that satisfies it. `shortestPath()`, `allShortestPaths()` and inline
  `WHERE`s are unchanged (ADR-0600).
- The `system_ontologies` namespace is now visible only under
  `RELATE_PREVIEW=ontology` (ADR-0593), the same preview row that gates the
  `db.ontology.*` procedures (ADR-0378). Without the opt-in the namespace
  is invisible: `SHOW DATABASES` lists the two Community Edition rows, and
  `USE system_ontologies`, session connect (`--namespace`, the client
  `connect`/`setNamespace` APIs, Bolt's `db` routing) and
  `ALTER DATABASE system_ontologies` all fail with the site's ordinary
  unknown-database rejection (`Graph not found: system_ontologies`,
  42N00) — never the authored preview error. With the opt-in set, the
  namespace is visible and behaves exactly as before (ADR-0229: not
  queryable, only the administration commands run from it). Stored
  databases are unaffected: the durable catalog row stays and the listing
  returns when the opt-in is set.
- Swift: the result-metadata types are renamed so they no longer collide
  with Apple's. `Notification` is now `QueryNotification` and `Namespace`
  is now `DatabaseNamespace`; both old names are removed, a source-breaking
  change for code that spelled them. `Severity` and `Category` are now
  `NotificationSeverity` and `NotificationCategory`, with deprecated aliases.
- Swift: calls that can only fail with the package's own error are
  `throws(RelateError)`.
- Swift: `QueryResult.records` returns `[Record]` instead of
  `[[String: CypherValue]]`. Reading a record by column name
  (`record["name"]`) is unchanged; `record.dictionary` is the old form.
- Swift: `RelateError` has two new cases, `unexpectedResult` and `misuse`;
  an exhaustive `switch` over it needs them. `QueryResult.scalar()` now
  throws `unexpectedResult`, rather than `decoding`, for a result that is
  not one row and one column.
- Swift: a `transaction(_:)` body receives a `Transaction` instead of the
  `Connection`. A body that only runs statements on its parameter compiles
  unchanged; one that called `begin`, `commit`, `rollback` or `setNamespace`
  on it does not.

### Deprecated

- Swift: `connect(namespace:)` and `setNamespace(_:)` taking a namespace
  name as a `String`. The namespaces are fixed; pass a
  `DatabaseNamespace`.

## [1.0.1] — 2026-11-12

### Added

- `SECURITY.md` at the repository root: the vulnerability reporting
  channel, the supported-surface scope, and the security allowances the
  project accepts.

## [1.0.0] — 2026-11-11

### Changed

- Opening an `s3://` URI reports that object storage belongs to clients
  implementing embedded Relate.

### Removed

- CDC replication slots. `register_cdc_slot`, `ack_cdc_slot`,
  `drop_cdc_slot`, `cdc_slots`, `cdc_records_after`, and
  `set_apply_only` / `apply_only` are gone, along with `cdc/slots.json`.
  Checkpoint recycle no longer waits on a consumer ack (ADR-0574).

## [0.1.3] — 2026-11-11

## [0.1.2] — 2026-11-09

## [0.1.1] — 2026-11-09

## [0.1.0] — 2026-11-09

### Initial release

First release of Relate — the embedded, in-memory property graph engine
with an openCypher query contract, WAL + snapshot durability, and Bolt
serving. See the README for the full surface list.

[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.2...HEAD

[1.0.2]: https://github.com/relatedb/relate/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/relatedb/relate/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/relatedb/relate/compare/v0.1.3...v1.0.0
[0.1.3]: https://github.com/relatedb/relate/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/relatedb/relate/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/relatedb/relate/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0

[Unreleased]: https://github.com/relatedb/relate
