# Changelog

All notable changes to Relate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Swift: `Database(url:readOnly:)`, `connect(namespace:)` and
  `setNamespace(_:)` taking the typed namespace, and error predicates on
  `EngineError` (`isConstraintViolation`, `isSyntaxError`,
  `isMissingParameter`, `isWriteConflict`, `classification`) plus
  `RelateError.isRetryable`.

### Changed

- A `MATCH`-clause `WHERE` that reads the path (`length(p)`, `relationships(p)`,
  a relationship variable) now filters the selected paths afterwards for the
  keyword shortest selectors (`SHORTEST 1`, `ANY SHORTEST`, `ALL SHORTEST`,
  `SHORTEST 1 GROUPS`), as Neo4j does; it no longer promotes a longer path
  that satisfies it. `shortestPath()`, `allShortestPaths()` and inline
  `WHERE`s are unchanged. The affected fan-out queries also stop walking
  every trail per reached node (ADR-0600).
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

[Unreleased]: https://github.com/relatedb/relate/compare/v1.0.1...HEAD

[1.0.1]: https://github.com/relatedb/relate/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/relatedb/relate/compare/v0.1.3...v1.0.0
[0.1.3]: https://github.com/relatedb/relate/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/relatedb/relate/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/relatedb/relate/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0

[Unreleased]: https://github.com/relatedb/relate
