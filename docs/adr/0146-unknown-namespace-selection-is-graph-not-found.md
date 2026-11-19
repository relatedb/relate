---
type: adr
area: engine
date: 2026-09-28
deciders: CypherLite maintainers (agreed in conversation with the project owner on 2026-09-28)
name: 0146-unknown-namespace-selection-is-graph-not-found
status: accepted
tags:
  - errors
  - namespaces
title: "Unknown-namespace selection is \"Graph not found: \\<name\\>\""
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
SUPERSEDES:
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0121-ontology-procedures]]"
---

# 0146. Unknown-namespace selection is "Graph not found: \<name\>"

## Context

[ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §6 specified that selecting an unknown namespace — over
`Database::connect_namespace`, `Connection::set_namespace`, the C ABI and
Bolt `db` extras — fails with `DatabaseNotFound` and the text
`Database does not exist. Database name: '<name>'.` (GQLSTATUS 22N51). That
text was sourced from Neo4j CE's
`NoSuchDatabaseTransactionCreationException`
(`community/bolt/src/main/java/org/neo4j/bolt/tx/error/NoSuchDatabaseTransactionCreationException.java`).

Probing live Neo4j CE 2026.09 (`neo4j:2026.09-community` container) with the
official Python driver shows that exception never surfaces on the
session-database path. All three driver spellings that select a database —

- `driver.session(database="nosuch")` + `session.run(...)`,
- `driver.session(database="nosuch")` + `session.begin_transaction()`,
- `driver.execute_query(..., database_="nosuch")`,

fail identically with the message **`Graph not found: nosuch`**, Neo4j code
`Neo.ClientError.Database.DatabaseNotFound`, wire GQLSTATUS `42001` with a
`42N00` cause — exactly the text, code and status chain of an unknown static
`USE` target ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §3, fixture case `use_unknown_graph`). The regenerated
live fixture (`testdata/ce-errors/neo4j-ce.json`, source `live-container`)
pins this in its `database_does_not_exist` case.

## Decision

Unknown-namespace **selection** errors use CE's live text
`Graph not found: <name>`, identical to an unknown static `USE` target. The
`DatabaseNotFound` kind token is kept, so bindings' name scans and error
kinds are unchanged.

- `cypherlite_core::ce_messages::database_does_not_exist` now delegates to
  `graph_not_found`; both named entry points stay because the CE paths
  (session database selection vs. statement routing) are distinct call
  sites.
- The GQLSTATUS claim in [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §6 is likewise superseded: live CE wires
  `42001` with a `42N00` cause, not top-level `22000` / root-cause `22N51`.
 CypherLite's Bolt server keeps the `CypherLite.Error` code per [ADR-0260](./0260-trust-boundary-error-sanitization.md),
  so the wire code change is informational only; the fixture case's pinned
  root-cause GQLSTATUS is now `42N00`.
- [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §6 is superseded by this ADR; the rest of [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) stands.

## Alternatives considered

- **Keep the [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §6 text as a documented divergence.** Rejected: CE
 fidelity is the point of the fixed-namespaces feature ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md)), the
  fixture exists to pin live CE texts, and no CypherLite behavior depends
  on the old wording beyond tests that restate it.
- **Find the input that triggers
  `NoSuchDatabaseTransactionCreationException`.** Done: probing the three
  driver selection paths above on live 2026.09 CE shows the exception never
  surfaces there; every path yields `Graph not found: <name>`. There is no
  observable CE behavior left for the old text to match.

## Consequences

- `cypherlite_core::ce_messages::database_does_not_exist`, its callers and
  every surface test, client test and doc page that quoted
  `Database does not exist. Database name: '<name>'.` change with this ADR;
  the fixture's `database_does_not_exist` case leaves
  `ce_error_fixture.rs`'s skip list and runs against the engine again.
- The error text on this path is now byte-identical to `USE nosuch`, which
  is what CE users see; error-kind recovery (`DatabaseNotFound`) is
  unaffected.
- The unrelated `ALTER DATABASE <unknown>` text (`Failed to alter the
 specified database '<name>': Database does not exist.`, [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §2) is
  unchanged — live CE still emits it.

## Later references

- [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md) reverses "CypherLite's Bolt server keeps the `CypherLite.Error`
  code": the unknown-namespace error is now the typed
  `Error::DatabaseNotFound { name }` (same `Graph not found: <name>` text),
  and Bolt sends it as `Neo.ClientError.Database.DatabaseNotFound`, the code
  live CE sends.
