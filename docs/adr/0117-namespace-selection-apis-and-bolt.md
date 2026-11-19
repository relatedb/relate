---
type: adr
area: surface
date: 2026-09-27
deciders: CypherLite maintainers
name: 0117-namespace-selection-apis-and-bolt
status: accepted
tags:
  - bolt
  - namespaces
title: Namespace selection in the embedding APIs and over Bolt
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0146-unknown-namespace-selection-is-graph-not-found]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
  - "[[adr/0502-typed-c-abi-v2]]"
---

# 0117. Namespace selection in the embedding APIs and over Bolt

## Context

[ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) gives every store the fixed namespaces `neo4j`, `system` and
`system_ontologies`, and routes each statement to its `USE` target or to the
connection's session namespace. Every surface today opens one `Database` and
connects without naming a namespace:

- Rust: `Database::connect()` (`cypherlite/cypherlite/src/lib.rs:407`).
- C: `cypherlite_connect(db, &conn)`
  (`surfaces/cypherlite-ffi/include/cypherlite.h:178`), which the JS, Java and
  Go clients wrap.
- Python: `cypherlite.connect(database=":memory:")`, where `database` is the
  store path (`surfaces/cypherlite-python/python/cypherlite/__init__.py:145`).
- Bolt: the server ignores the extra map of `BEGIN` and `RUN`, including its
  `db` field (`surfaces/cypherlite-bolt/src/protocol.rs`).

Neo4j drivers select a database through session or query configuration
(`session(database=…)`, `execute_query(database_=…)`). Since Bolt 4.0 they send
it as `db` in the `BEGIN` extra and in the auto-commit `RUN` extra, where
`null` or `""` means the home database. Neo4j's driver manuals prefer this
configuration over `USE` because drivers do not parse Cypher. CypherLite's Bolt
server speaks Bolt 4.4.

## Decision

1. **Default.** Connecting without a namespace keeps today's behaviour: the
   session namespace is `neo4j`.
2. **Rust.** Add `Database::connect_namespace(&self, name: &str) ->
   Result<Connection>`, `Connection::set_namespace(&mut self, name: &str) ->
   Result<()>` and `Connection::namespace(&self) -> &str`. `set_namespace`
   fails while a transaction is open: `Cannot change the namespace while a
   transaction is open.`
3. **C ABI.** Add two functions without changing any existing signature:

   ```c
   cypherlite_status cypherlite_connect_namespace(cypherlite_database db,
       const char *name, cypherlite_connection *out);
   cypherlite_status cypherlite_conn_set_namespace(cypherlite_connection conn,
       const char *name);
   ```

   The bundled handle API (`cypherlite_open`) is unchanged and selects
   namespaces with `USE`. The JS, Java and Go clients expose
   `connect(namespace)` and `setNamespace(name)` over these two functions
   only.
4. **Python.** `cypherlite.connect(database=":memory:", namespace="neo4j")`,
   plus a `Connection.namespace` property and `Connection.set_namespace(name)`.
   The parameter is not called `database`, which already names the store path.
5. **WASM.** No API change; `USE` selects the namespace.
6. **Unknown names.** Selecting anything other than a fixed namespace fails
   with `DatabaseNotFound` and CE's connect text, `Database does not exist.
   Database name: '<name>'.` (GQLSTATUS 22N51). Names match case-insensitively.
7. **Bolt.**
   - For each auto-commit `RUN` and each `BEGIN`, the session namespace is the
     extra's `db`, or `neo4j` when `db` is absent, `null` or `""`. It does not
     carry over from an earlier request.
   - A `RUN` inside an explicit transaction carries no `db`, per the Bolt
     specification, and uses the namespace chosen at `BEGIN`.
   - An unknown `db` produces a `FAILURE` with the text from (6) and the code
 `CypherLite.Error` ([ADR-0260](./0260-trust-boundary-error-sanitization.md)).
   - The final `SUCCESS` of `PULL` or `DISCARD` includes `db`, the namespace
     the query ran in.
   - `HELLO` is unchanged. `ROUTE` stays unsupported, so drivers connect with
     `bolt://`.

## Alternatives considered

- **A namespace option on `Database::open_with`.** Rejected: one store is one
  handle and one WAL. The namespace belongs to a connection, as a database
  belongs to a Neo4j session.
- **Select only through Cypher `USE`.** Rejected: drivers send `db`, not
  `USE`, and hosts should not have to rewrite their queries.
- **A `database` keyword in Python.** Rejected: `connect(database=…)` already
  means the store path, so reusing the name would change the meaning of
  existing calls.
- **Language-specific entry points in the JS, Java or Go clients.** Rejected:
  clients wrap the C ABI only (AGENTS.md §2).
- **Answer `ROUTE` now.** Rejected for this change: it would enable
  `neo4j://` URIs and home database resolution, but namespace selection does
  not depend on it, so it can follow separately.

## Consequences

- Neo4j drivers that pass `database="system_ontologies"` or
  `database="neo4j"` work unchanged over `bolt://`.
- `docs/api/{rust,c,python,javascript,java,go,bolt}/spec.md`, their
  descriptions and their runnable examples change with the implementation.
  The C header gains two functions and the ABI stays additive.
- Client tests in each language cover selecting a namespace, switching
  between transactions and the unknown-name error.

## Later references

- [ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md): supersedes §6. Live CE 2026.09 answers an unknown session
  database with `Graph not found: <name>` (wire 42001, cause 42N00), not
  the `Database does not exist. Database name: '<name>'.` text quoted here.
- [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md): supersedes §7's FAILURE code for an unknown `db`. It is
  `Neo.ClientError.Database.DatabaseNotFound`, not `CypherLite.Error`; the
  message is unchanged.
- [ADR-0502](./0502-typed-c-abi-v2.md): the typed C ABI v2 removes the `cypherlite_connect` alias and the
  `cypherlite_open` JSON handle API named above. The namespace-selection
  calls (`cypherlite_connect_namespace`, `cypherlite_conn_set_namespace`) keep
  their names and gain a trailing `char **error_json` out-param, and a
  statement failure is read from the result, not from `cypherlite_last_error`.
