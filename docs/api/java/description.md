# Java client — description

`ai.relatedb.Database` wraps the C ABI for JVM embedders through JNA.
The jar is self-contained (ADR-0490): the plain `ai.relatedb:relate`
artifact bundles `relate_ffi` for all five supported platforms and
loads the right one with no environment variable or library path; a
per-platform classifier jar (`osx-aarch_64`, `linux-x86_64`, …) carries one
platform's library for size-sensitive deployables. A platform the jar does
not cover fails at first use with an error naming the classifier to add.
Every jar also carries the library skill for AI agents at
`.agents/skills/relate/SKILL.md`. See `examples/BundledNative.java`.

Sessions live in one of the three fixed namespaces (`neo4j`, `system`,
`system_ontologies`; ADR-0116/ADR-0117): pick one at connect time with
`Database.connect(String)` or switch between transactions with
`Connection.setNamespace(String)`. See `examples/Namespace.java`.

Every `Result` also reports two plan-time stamps (ADR-0285):
`getQueryType()` — the `QueryType` enum, `READ`, `WRITE`, `READ_WRITE` or `SCHEMA` (ADR-0162) — and
`getNamespace()` (the `Namespace` enum), the statement's resolved namespace (its `USE` target, or
the session namespace when unrouted). `getNotifications()` adds the
statement's query notifications as typed `Notification` entries
(ADR-0363): `EXPLAIN`/`PROFILE` populate the list with each code, GQLSTATUS,
severity, category, title, description, remedy and, on operator-scoped
kinds, the raising operator's id; a plain run returns an empty list. See
`examples/ResultMetadata.java`.

Result cells hydrate typed through the value walker of the typed ABI v2
(ADR-0502): integers as exact `Long`s (the full int64 range), strings with
their interior NULs whole, nodes/relationships/paths as `Node` /
`Relationship` / `Path` records, temporals as `java.time` values, durations
as their four components, points and vectors as `Point` / `VectorValue`.
`Connection.query(cypher, params)` binds named parameters the same way —
one putter per Java type, temporals and vectors included, no JSON and no
string concatenation. `Database` and `Connection` are `AutoCloseable`
(closing is what makes a file store reopenable in the same JVM), commits
carry the engine's bookmarks, and every failure throws a typed exception —
`TransientException` exactly when the engine marked it retryable, then
`ClientException` / `DatabaseException` by the classified code — with the
Neo4j status code, GQLSTATUS, stage and retry verdict along for the caller.
See `examples/Parameters.java`, `examples/Transaction.java` and
`examples/RelateE2E.java`.
