# Changelog

All notable changes to Relate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- New .NET client `Relate.Net` under `clients/dotnet` (ADR-0625): an
  embedded C#, F# and Visual Basic API over the typed C ABI v2, with
  disposable databases, connections and transactions, typed parameters,
  graph and temporal values, result metadata and structured errors. CI
  builds the native library from the same checkout and runs the
  three-language tests on Linux, macOS and Windows.
- Mistral Vibe joins the `relate skill install` agent catalog. A machine
  with `~/.vibe` gets a `vibe:` row in `relate skill list`, and
  `relate skill install --agent vibe` (or a bare `relate skill install`,
  which targets every detected agent) writes the skill bundle to
  `~/.vibe/skills/relate` — detection, install, `skill check` drift
  detection and the `relate upgrade` refresh treat it like every other
  agent.

### Changed

- The engine's sidecar directory inside a store is `.relate/`, not
  `.cypherlite/` (the schema-files mirror moves with it): a writable
  open adopts an existing store in place with one directory rename,
  run as the last step before the lease so an open refused by any
  earlier check never renames the user's directory — natively, and
  through a caller-supplied `Vfs` (`Database::open_vfs`,
  `StoreTarget::from_vfs_with_codec`) the same way. A read-only open
  never adopts: a store still carrying the legacy `.cypherlite/`
  markdown sidecar refuses closed, naming the one writable pass that
  adopts it, and a legacy `.cypherlite/` without the manifest (a
  classic store's schema mirror alone) is inert and opens unchanged.
  The lock and writer marker files keep their names.
- `MATCH (n:L) RETURN n.p, count(*)` and the min, max and `count(n.p)`
  aggregates over one labeled property now answer from the existing
  `(L, p)` index instead of scanning every node of the label — the
  `group_by_age` query of the SF0.1 latency suite drops from 17.3 ms to
  6.2 ms. Results are unchanged: whenever the index cannot serve the
  answer exactly, the aggregate falls back to the scan's exact answer
  (ADR-0611).
- Redundant parentheses around a projection item no longer change its
  meaning: `WITH (n)`, `WITH DISTINCT(n)`,
  `RETURN DISTINCT(n.p1) ORDER BY n.p1` and `GROUP BY (a)` now behave
  exactly like their unparenthesized forms — alias rules, ordering under
  `DISTINCT`, grouping keys and exemptions, `CALL` imports and the
  rendered column names — as Neo4j treats them. A property access keeps
  Neo4j's verbatim column name (`(n.p1)` names its column `(n.p1)`), and
  no restriction is loosened: every pinned refusal keeps its verdict.
- A constraint, index or database can now be literally named `null`:
  `CREATE CONSTRAINT null …`, `CREATE INDEX null`, `DROP CONSTRAINT
  null`, `DROP INDEX null`, `ALTER DATABASE null` and
  `SHOW DATABASE null` parse and run, as in Neo4j. String-literal names
  and the continuation keywords stay rejected.
- Two parameter-name spellings Neo4j accepts now plan and run: the
  backtick-quoted name (``$`p1``, ``$`p 1``) and whitespace between `$`
  and the name (`$ p2`) — both previously failed at the lexer with
  `InvalidParameterName`. Every `$name` render now goes through one
  canonical quoting policy, so two different parameter names can never
  collide in a plan-cache key; `$ €`, a bare `$` and `$ (x)` keep their
  rejections.
- Cypher text may carry `\uXXXX` unicode escapes in the whitespace
  between tokens and in identifier text — `\u2028` between two tokens
  reads as whitespace, `\u0041` as the identifier `A` — matching the
  reference server's decoding. Parameters, numbers, comments and
  backtick names keep their rejections: an escape cannot spell a
  parameter name or any other token (ADR-0619).
- A fixed-length clause entered by two or more rows now runs on the
  frontier kernel, one run per entering row, instead of falling to the
  per-row walker — the walker enumerates trails, exponentially on a
  dense cyclic component. A single row whose run declines at run time
  still drags the clause to the walker.
- Dense id-span stores reserve the smaller of a 1.5-growth ladder rung
  and the old power-of-two doubling, and the memory floor charges
  exactly that reservation — the worst-case slot slack in a restored
  store drops from below 2× to below 1.5× (ADR-0617).
- The Java client now publishes to Maven Central by default from a
  Publish-to-registries workflow dispatch, over a repaired natives gate
  (the `release` environment still gates the credentials, and
  `autoPublish=false` still leaves the Central Portal release in hand;
  uncheck `maven` to publish only the other registries).
- Every release now publishes the Java client to Maven Central
  automatically through the release pipeline, and the publish profile
  gains a dry run that rehearses it against `main` without uploading.
- A checkpoint reopen now decodes and installs across all cores — one
  Parquet reader per row-group span, a slice-based bulk install that
  reads rows in place — so reopening a large store drops from 1225 ms
  to 461 ms on the measured fraud shape (2.66×; 1.36× on a shape whose
  restore work was already sharded). Restore rows are read in place and
  installed in bounded slices, so peak restore memory is
  `max(rows, graph)`, not `rows + graph`.
- `EXPLAIN`/`PROFILE` plan trees show display-only child steps under an
  index-seek-started `MATCH` — the seek step plus one `Expand` step per
  hop, in the CLI's plan table and as fused-flagged children in the
  structured plan (Bolt `queryPlan`/`profiledQueryPlan`, HTTP v2, the
  CLI's machine modes). Execution is unchanged: the clause stays the
  executed unit; consumers that require per-operator metrics skip the
  children.

### Notes

- The 1.1.0 Swift entries were misfiled under [1.0.2] by their pull requests and moved to their release (ADR-0618).
- The shortest-route fan, anchor, selector and class-walk work landed as consolidation campaigns — no user-facing change (#499, #497).
- The PR cycle is now stated as one loop in the repository's agent contract, the ready flip the agent's own signal and the merge a separate authorization (#525).

## [1.1.0] — 2026-11-17

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
- Swift: live values. `Database.values(tags:_:)` is a stream of what a
  read returns, now and again after each commit that may have changed it;
  `@LiveQuery` is the same observation as a SwiftUI property, reading the
  database set with `.relateDatabase(_:)`.
- Swift: `Database.defaultStoreURL(named:)` and
  `Database.provision(at:from:)` for seeding an app's store from its bundle,
  and `Migrator` for idempotent schema statements plus versioned steps.
- A guide, "Relate in an iOS app" (`docs/guides/ios-apps.md`), and the
  `examples/ios-snotel` app moved to live queries and the asynchronous,
  typed Swift API.
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

- A shortest-path endpoint whose `WHERE` no index serves (`WHERE b.id + 0 =
  7`, a function over the property) is now listed by scanning its label with
  that predicate, and the pair search runs for the few matches, instead of a
  search that reaches every node and builds a row and a path for each. On a
  1.6M-node social graph `SHORTEST 1 (a {id: …})-[*..6]->(b) WHERE b.id + 0 =
  …` drops from 16 s to 0.5 s, the cost of the same predicate in a separate
  `MATCH`. Results are unchanged; a predicate matching more than 16 nodes, a
  nondeterministic one, one that reads another variable, or one that raises
  keeps the earlier plan.
- `SHORTEST k` (k above 1), `SHORTEST k GROUPS` and `ANY k` between two
  fixed endpoints now walk the trails one length class at a time instead of
  enumerating every trail up to the hop bound, so a bounded pair query that
  never finished on a social graph answers in milliseconds. Results are
  unchanged; a count selector with one open end still enumerates. A start
  with a large neighbourhood is no longer scanned neighbour by neighbour:
  the walk reads its steps near the end from the distance ball, and a full
  top-k heap rejects a worse trail before building it.
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
## [1.0.2] — 2026-11-14

### Added

- Swift: `Database(url:readOnly:)`, `connect(namespace:)` and
  `setNamespace(_:)` taking the typed namespace, and error predicates on
  `EngineError` (`isConstraintViolation`, `isSyntaxError`,
  `isMissingParameter`, `isWriteConflict`, `classification`) plus
  `RelateError.isRetryable`.

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

[Unreleased]: https://github.com/relatedb/relate/compare/v1.1.0...HEAD

[1.1.0]: https://github.com/relatedb/relate/compare/v1.0.2...v1.1.0
[1.0.2]: https://github.com/relatedb/relate/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/relatedb/relate/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/relatedb/relate/compare/v0.1.3...v1.0.0
[0.1.3]: https://github.com/relatedb/relate/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/relatedb/relate/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/relatedb/relate/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0

[Unreleased]: https://github.com/relatedb/relate
