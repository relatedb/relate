# 0557 — List only the procedures an embedded engine can answer

Status: Proposed
Claim: e7abcfac69d86bd9722ef4f59954d4828a970085

## Context

`SHOW PROCEDURES` lists 92 rows while `CALL` executes 27. The listed-only rows
arrived as catalogue-compatibility work — display scenarios for the procedure
listing rather than capabilities anyone built — and most of them describe a
server Relate is not: cluster membership, routing tables, replica promotion,
fleet management, change-data-capture, connection/lock/pool/JMX
administration, a job scheduler, auth-cache and TLS reload, usage-statistics
reporting, and server-configuration writes.

A client that reads the catalogue is told it may call those procedures. It
cannot: the families have no implementation, and several of them could not
have one in a single-process engine with one local store.

## Decision

The catalogue lists only what the engine can answer. The cluster, routing,
fleet-management, CDC, session/connection/lock/pool/JMX/security/scheduler,
usage-statistics and server-configuration families leave the registry, as do
the vector-property setters and the index-resampling pair.

What stays is the graph and schema introspection, the fulltext and vector
query procedures, the catalog writes, the ontology family, the client-facing
server-info rows (`dbms.components`, `dbms.info`, `dbms.listConfig`,
`dbms.clientConfig`, `dbms.licenseAgreementDetails`, `dbms.showCurrentUser`),
graph counts, and the embedded-relevant rows that become executable next:
`db.checkpoint`, `db.awaitIndex`, `db.listLocks`, `db.clearQueryCaches`,
`db.prepareForReplanning`.

## Alternatives considered

- **Keep the rows listed for client compatibility.** Rejected: a catalogue
  entry is a promise the call can be answered, and a client that trusts the
  list is misled in exactly the way this decision removes.
- **Keep them and mark them not executable.** Rejected: `SHOW PROCEDURES` has
  no column for "listed but impossible", and the listing is what clients read.
- **Repurpose the server-configuration writes for engine settings** (safe
  mode, memory limits, snapshot codec). Rejected: those are open-time options
  resolved once at open and immutable afterwards, so a runtime setter would
  have nothing to set; the Neo4j names also carry server semantics a client
  would read literally. A runtime settings surface, if wanted, is its own
  decision.
- **Keep CDC because the WAL already carries change records.** Rejected as a
  namespace decision rather than a capability one: the underlying primitive
  stays, the procedures go.

## Consequences

- `SHOW PROCEDURES` shrinks from 92 listed rows to 34, and the admin inventory
  from 34 rows to 5.
- Scenarios whose worked example is a dropped procedure can no longer pass.
  They are marked as expected failures in the compatibility harness rather
  than deleted, so the semantics they assert — signature rendering,
  deprecation filtering, reserved-namespace listing — can be re-pinned to a
  kept procedure.
- Index resampling is dropped rather than stubbed: planner statistics are
  computed on demand from the store, so the procedures could only report work
  they do not do.
- Restoring any dropped family — or adding a runtime settings surface — is a
  new decision, not a reversal of this one.

## Later references

- **ADR-0561** — `dbms.listCapabilities` returns, as an executable procedure
  listing this engine's own capabilities. It is the one dropped name whose
  answer is a fact about the engine rather than about a server it is not; the
  removal stands for every other family.
