# 0561 — `dbms.listCapabilities` lists this engine's own capabilities

Status: Proposed
Claim: f5402057a999eeda3af71768915d6a703ee36621

## Context

ADR-0557 dropped the procedure families that described a server Relate is
not. One of them, `dbms.listCapabilities`, was client-facing in a way the
others were not: a client asks it what the DBMS can do, and the answer is a
statement about *this* engine rather than about cluster membership, routing,
or a job scheduler.

Probing Neo4j 2026.10.1 shows why the row set cannot be mirrored: its four
rows are `virtual_graph.enabled`, `virtual_graph.generic_drivers`,
`virtual_graph.supported_drivers` and `virtual_graph.version` — the Graph
Engine's own facts and the installed JDBC drivers. The vocabulary is
vendor-owned; there is no generic upstream set for another engine to answer
with, and every one of those four names would be a lie here.

## Decision

`dbms.listCapabilities` returns as an executable procedure, listing the
capabilities this engine has. Its shape stays Neo4j's — one row per
capability with a `name`, a `description` and a `value`, mode `DBMS`,
callable from the system database — because that is the contract a client
reads. The rows are ours:

- `cypher.version` — the dialect version this engine implements
- `fulltext.index` — whether fulltext indexes and their query procedures are available
- `vector.index` — whether vector indexes and their query procedures are available
- `pattern.engine` — the pattern-matching engine in force for this database

The last one is read from the store rather than from a constant, so a
database opened with the diagnostic pattern-engine switch reports what it is
actually running.

## Alternatives considered

- **Leave it dropped with the rest.** Rejected: this is the one dropped name
  whose honest answer is a fact about the engine, and a client that asks it
  otherwise has no surface at all.
- **Mirror Neo4j's four `virtual_graph.*` rows.** Rejected: they describe a
  Graph Engine and JDBC drivers Relate does not have; the rows would be
  false, which is the failure mode ADR-0557 exists to remove.
- **Report every listed procedure as a capability.** Rejected: `SHOW
  PROCEDURES` already answers that. The value here is a small, stable set a
  client can read without filtering.
- **Answer from a compile-time constant table.** Rejected for the
  pattern-engine row: the answer would not track the running store's
  configuration.

## Consequences

- `SHOW PROCEDURES` lists 35 rows rather than 34.
- ADR-0557's removal stands for every other family; this is the new decision
  its own Consequences section called for, not a reversal of it.
- The capability vocabulary is ours to extend: a new capability is a row
  added here, not a compat mirror to maintain.
