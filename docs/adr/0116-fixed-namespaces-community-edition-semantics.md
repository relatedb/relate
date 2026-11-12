# 0116. Fixed namespaces with Neo4j Community Edition database semantics

- **Status**: Accepted
- **Date**: 2026-09-27
- **Deciders**: CypherLite maintainers (Linear LABS-102)
- **Later references**: ADR-0142 fills in the SHOW DATABASES fields this ADR
  left unspecified (`address`, `role`, `statusMessage`, `writer`) with fixed
  constants for Neo4j Desktop Browser compatibility, and confirms the
  `access`/status/`default`/`home` values decided here.
  ADR-0229 makes `system_ontologies` listed but not queryable: §1's
  "Readable with ordinary Cypher" and §4's 42N18 write gate are superseded —
  every statement routed there except §2's administration commands now
  fails with 42N17 (ADR-0229 §2's text), reads included; §5's capture
  captures a permanently empty graph; §7's `cypherlite.system_ontologies`
  segment is decode-only (no id is minted there); §10's slot is never built
  or replaced. The §6 SHOW row and its values stand.
  ADR-0590 implements the off-system half of §2's 51N28 boundary recorded
  below: the "deliberately not implemented" divergence is superseded for
  statements not routed to `system`, where live rejects every composed
  administration command — a `UNION` arm included — and the engine now does
  too. The system-routed half stands as recorded: `USE system` plus an
  administration command stays accepted, and the clauses allowed there
  remain §4's system-namespace whitelist.

## Context

LABS-102 asks for separate information domains in one store (data, ontologies
and system information) without mixing them in one graph.

Today a store holds exactly one graph (`Shared.graph`,
`cypherlite/cypherlite/src/lib.rs:69-70`). Neo4j's multi-database surface
exists only as metadata, the "theater" of ADR-0094 and ADR-0095:

- `CREATE/ALTER/DROP DATABASE` add or remove a row in `TheaterData.databases`
  (`cypherlite/cypherlite-executor/src/exec/catalog_schema.rs:397-487`). No
  graph is created. The default rows are `neo4j` and `system`
  (`cypherlite-executor/src/graph/theater.rs:27-41`).
- `USE` is parsed and placement-checked, then ignored at execution
  (`cypherlite-executor/src/exec/dispatch.rs:219`), so `USE anything MATCH …`
  silently reads the one graph. `docs/limitations.md` claims this is rejected.
- `db.info()` and `db.nameFromElementId()` always answer `neo4j`.

Neo4j Community Edition (CE) is the reference for a product with one user
database. It has `neo4j` and `system`, supports `SHOW DATABASE(S)` and
`ALTER DATABASE … SET DEFAULT LANGUAGE`, and rejects every other database
administration command with
`Neo.ClientError.Statement.UnsupportedAdministrationCommand` (GQLSTATUS
51N27): `Unsupported administration command: <statement>`. The error texts in
this ADR come from Neo4j's CE acceptance tests on `neo4j/neo4j` branch
`2026.09`.

Neo4j's ontology persistence proposal ("Ontology Persistence: Proposal",
2026-09-14) stores ontologies in a DBMS-managed database named
`system_ontologies`. Names starting with `system` are reserved in Neo4j, so the
name cannot clash with a user database. The database is read-only to Cypher,
written only through `db.ontology.*` procedures, and queried with ordinary
Cypher. CypherLite has one user database, so it holds at most one ontology.

## Decision

1. **Three fixed namespaces.** Every store has exactly these. None can be
   created, dropped or renamed, and names match case-insensitively.

   | Namespace | SHOW `type` / `access` | Behaviour |
   |---|---|---|
   | `neo4j` | standard / read-write; `default` and `home` | The user data graph. Existing stores are this namespace. |
   | `system` | system / read-write | CE rules: SHOW commands, supported administration commands and procedure calls only. It holds no graph data. |
   | `system_ontologies` | standard / **read-only** | Readable with ordinary Cypher. Cypher cannot write it; only the `db.ontology.*` procedures change it (ADR-0121). It holds at most one ontology, the one for `neo4j`. |

2. **Administration commands follow CE.** These are supported:
   `SHOW DATABASE[S] [name]`, `SHOW DEFAULT DATABASE`, `SHOW HOME DATABASE`,
   and `ALTER DATABASE <name> [IF EXISTS] SET DEFAULT LANGUAGE CYPHER 5|25` as
   the only clause and without `WAIT`.

   Every other database administration command fails with
   `UnsupportedAdministrationCommand`: `Unsupported administration command:
   <statement>`. The statement is echoed verbatim, without a leading
   `CYPHER n`. This covers:
   - `CREATE [OR REPLACE] [COMPOSITE] DATABASE` and `DROP [COMPOSITE] DATABASE`;
   - every other `ALTER DATABASE` item: `SET ACCESS`, `SET OPTION`,
     `REMOVE OPTION`, `SET TOPOLOGY`, `WAIT`;
   - `START DATABASE` and `STOP DATABASE`;
   - `CREATE`, `ALTER` and `DROP ALIAS`, and `SHOW ALIAS[ES] … FOR DATABASE[S]`.

   The parser recognizes these forms so they reach this error rather than a
   syntax error. An `ALTER` of an unknown database fails with `Failed to alter
   the specified database '<name>': Database does not exist.`, unless
   `IF EXISTS` makes it a no-op. Administration commands run from any
   namespace, as they do for a CE client over Bolt. User, role, privilege and
   server commands are out of scope for this ADR and unchanged.
   **Later references** (2026-11-04, live-verified against Neo4j 2026.09
   CE): the closing claim above is wrong for composed forms — the bare
   `SHOW DATABASES` succeeds from the user database as decided here, but
   every composed form off-system (`YIELD …` composed with `MATCH`,
   `WITH`, `UNION`, `NEXT`, …) is rejected 51N28 `NotSystemDatabaseError`
   ("This is an administration command and it should be executed against
   the system database: SHOW DATABASES"). That boundary is DBMS
   session-model semantics — session routing and privilege enforcement —
   and is deliberately not implemented: a single-user embedded engine with
   no privileges and a store-level catalog (ADR-0276) has no wrong
   database a composed administration command could run against, so the
   no-`USE` composed forms keep running from any namespace (user decision
   2026-11-04; the spec harness tags those scenarios
   `@fails:admin-session-boundary`). Two rules from the same verification
   are implemented:
   - §4's system-namespace whitelist is completed: a `WITH` composing an
     administration command's `YIELD`, routed to `system`, fails with
     CE's `The following clauses are not allowed on a system database:
     WITH.` (wire 42001, `Neo.ClientError.Statement.SemanticError`;
     fixture case `use_system_show_databases_with`).
   - An explicit `USE` clause targeting a non-system namespace on a
     statement headed by one of these commands fails with CE's 42N79
     auto-routing rejection ("The `USE` clause is not required for
     administration commands. Retry your query without the `USE` clause,
     and it will be routed automatically.",
     `Neo.ClientError.Statement.SyntaxError`; fixture case
     `use_neo4j_show_databases`).

   `USE system` + administration command stays accepted — live CE rejects
   even `USE system ALTER DATABASE …` with 42N79, so that acceptance is a
   recorded divergence — and the non-head command positions live CE runs
   (`USE neo4j MATCH (n) SHOW DATABASES YIELD name RETURN name`) keep
   running.

3. **`USE` routes a statement.** A statement targets the namespace named by
   its `USE` clause, or else the connection's session namespace (ADR-0117,
   ADR-0118). Graph references resolve before execution:
   - A static name must be one of the fixed namespaces.
     `graph.byElementId(<string>)` resolves through the element id scheme in
     (7); an id that names no namespace fails as in Neo4j (42NA7).
   - An unknown name fails with `DatabaseNotFound`: `Graph not found: <name>`
     (GQLSTATUS 42N00).
   - `graph.byName`, `graph.names` and `graph.propertiesByName` fail with
     42N72, since they are only valid on composite databases. They stay
     registered and listed by `SHOW FUNCTIONS`.
   - Different targets in one statement, across `UNION` arms, `CALL`
     subqueries or `NEXT`, fail with `Multiple graphs in the same query not
     allowed here. This feature is only available on composite databases.`
     (42NA5).

   `USE` works on every surface, including embedded calls, as it does for a CE
   client over Bolt.

4. **Namespace rules.**
   - `system`: anything other than SHOW commands, supported administration
     commands and standalone procedure calls (`CALL … YIELD … RETURN`) fails
     with `SemanticError` and CE's 42N17 text, `The following unsupported
     clauses were used: <clauses>. …`. The existing `USE system` rules for
     composing SHOW commands stay.
   - `system_ontologies`: any write fails with `WriteOnReadOnlyAccessDatabase`:
     `No write operations are allowed on this database. The database is in
     read-only mode.` (GQLSTATUS 42N18). Writes include data clauses; index,
     constraint and graph-type DDL; and write procedures other than those of
     ADR-0121.

5. **Transactions read any namespace and write only `neo4j`.** This is Neo4j's
   query-router rule (read many, write one); `neo4j` is the only namespace
   Cypher can write. `BEGIN` pins `neo4j` as it does today and also captures
   the current `system_ontologies` graph, so every statement in the
   transaction reads one consistent snapshot of both. A connection's session
   namespace changes only between transactions.

6. **SHOW output is real.** `SHOW DATABASES` returns the three rows with:
   - `currentStatus` and `requestedStatus` set to `online`;
   - `access` as in the table in (1);
   - `default` and `home` true only for `neo4j`;
   - empty `aliases` and `constituents`;
   - the stored `defaultLanguage`.

   `SHOW DEFAULT DATABASE` and `SHOW HOME DATABASE` return `neo4j`, and an
   unknown name returns no rows. `db.info()` reports the namespace the
   statement targets.

7. **Element ids name their namespace.** The format is
   `<kind>:cypherlite[.<namespace>]:<n|r><id>`.
   - `neo4j` keeps the bare `cypherlite` segment, so `4:cypherlite:n5` and
     every id already handed out keep their meaning.
   - `system_ontologies` uses `cypherlite.system_ontologies`, for example
     `4:cypherlite.system_ontologies:n5`.
   - `system` has no elements and no segment.

   `db.nameFromElementId` maps the middle segment to the namespace name and
   keeps today's error for anything else. Ids keep exactly three
   `:`-separated parts.

8. **Settings stay in the existing catalog.** A namespace's default language
   stays in `TheaterData.databases` and the `PutDatabase` redo op; neither the
   WAL nor the snapshot format changes. Entries for other names, written by
   earlier `CREATE DATABASE` calls or by an older binary, are ignored on load
   and replay and left out of the next checkpoint image. A fixed namespace
   without an entry defaults to `CYPHER 25`. As today, the default language is
   stored and shown but does not select the parser version.
   **Later references**: ADR-0276 moves the table out of
   `TheaterData.databases` into the store-level `DatabasesCatalog` shared
   through `StoreServices`; the format and invariant statements here stand.

9. **Errors.** Messages match CE's text. The engine gains the error kinds
   `UnsupportedAdministrationCommand` and `WriteOnReadOnlyAccessDatabase`;
   `DatabaseNotFound` already exists. Bolt keeps ADR-0260's codes, so these
   errors reach drivers as `CypherLite.Error` with CE's message.
   **Later references**: ADR-0307 replaces those codes with Neo4j status
   codes: `DatabaseNotFound` is `Neo.ClientError.Database.DatabaseNotFound`
   (a typed engine variant now), the other CE errors
   `Neo.ClientError.Statement.SemanticError`; CE's messages are unchanged.

10. **Engine shape.** `Shared` holds one slot per namespace: `neo4j` is today's
    live graph, `system` an empty graph, and `system_ontologies` an immutable
    graph that is replaced as a whole (ADR-0120). The planner lifts the static
    `USE` target into the plan, and the connection resolves it and enforces
    the rules above. Executor operators do not change. Registered procedures
    and the LOAD CSV policy (ADR-0081) move from the graph to the store, and
    the memory limit (ADR-0042) covers every namespace. `pull` (ADR-0080)
    keeps writing `neo4j`.

## Alternatives considered

- **User-created namespaces (Enterprise-style multi-database).** Rejected: the
  store would become a small DBMS with per-database persistence, naming rules
  and lifecycle. The requested domains are a fixed set, and CE, the closest
  Neo4j product, has one user database.
- **A separate store per namespace.** Rejected: a `.cl` bundle carries one WAL
  stream (ADR-0074), ADR-0097 refuses stores nested in stores, and creating or
  dropping a namespace would span two durability domains.
- **Label or property partitions inside one graph.** Rejected: every operator,
  index, constraint and procedure would have to filter, a missed filter leaks
  data across domains, and it gives none of Neo4j's database semantics.
- **Keep the theater.** Rejected: `USE other` silently reads the wrong graph,
  and `CREATE DATABASE` reports success for a database that does not exist.
- **Resolve `USE` lazily, failing only when the statement touches graph
  data.** Rejected: `USE nosuch RETURN 1` would succeed where CE fails. It
  would only keep scenarios green that Neo4j itself rejects (ADR-0119).
- **Make `system` a readable data graph now.** Rejected: CE rejects data
  clauses there. Relaxing an error later is backward compatible; tightening
  one is not.
- **`ALTER DATABASE … SET ACCESS READ ONLY | READ WRITE` (CIP-13).** Rejected:
  CE rejects it, and read-only is a fixed property of `system_ontologies`.
- **One namespace per transaction.** Rejected: it would forbid reading the
  ontology and writing data that follows it in one transaction. Since only
  `neo4j` is writable, Neo4j's read-many/write-one rule costs no more.
- **Neo4j status codes over Bolt.** Rejected: ADR-0260 keeps CypherLite's
  codes, and changing that is a global decision, not a per-feature one.

## Consequences

- The Cypher dialect gains real routing. `USE neo4j`, `USE system` and
  `USE system_ontologies` behave as in Neo4j, and misspelled targets fail.
- Behaviour changes for existing users: database DDL now fails instead of
  recording a row, `USE <other>` fails instead of reading `neo4j`, and
  `SHOW DATABASES` lists `system_ontologies`.
- Neither the WAL nor the snapshot format changes. Stores stay readable by
  older binaries, which keep their old theater behaviour.
- 24 frozen CIP-eval scenarios exercise Enterprise or composite behaviour that
  CE rejects; ADR-0119 removes them from the frozen set. Crate tests that use
  `CREATE DATABASE analytics` or `USE graph.byName` are rewritten.
- `docs/limitations.md`, the "Cypher dialect" section of `AGENTS.md`, the API
  specs and the agent skill bundle change with the implementation. The error
  texts are checked against a live CE container and pinned as test fixtures.
- ADR-0094 and ADR-0095 gain Later references notes: their database theater
  rows now mirror the fixed namespaces.

## Later references

ADR-0554 makes the element-id namespace segment of (7) dash-bearing and
short: the writable namespace's segment is now `rl-id` and the ontology
namespace's is `rl-onto` (this ADR's `cypherlite` spelling predates the
product rename; the segment was `relate`, then the dash-bearing
`relate-neo4j`, at the time of those changes). The three-part shape and the
segment-to-namespace mapping of (7) are unchanged.
