---
type: adr
area: engine
date: 2026-09-17
deciders: CypherLite / MemoryDB merge
name: 0059-wal-redo-typed-payloads
status: proposed
tags:
  - catalog
  - wal
title: WAL redo payloads for catalog DDL are typed models
REFERENCES:
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0114-wal-recovery-rejects-undecodable-records]]"
  - "[[adr/0120-ontology-document-persistence]]"
  - "[[adr/0127-store-version-compatibility]]"
---

# 0059. WAL redo payloads for catalog DDL are typed models

## Context

[ADR-0012](./0012-wal-and-parquet-checkpoints.md) established that WAL redo is physical graph ops, not Cypher text;
[ADR-0014](./0014-single-wal-write-path.md) made every commit a WAL append followed by `apply_redo`. The merge
([ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)) brought in three catalog-DDL redo ops that violated that
discipline: `CatalogIndexDetail`, `PutGraphType`, and
`SetCurrentGraphType` serialized a **Cypher-text payload** (a
`specification` string) into the WAL record, and replay formatted /
re-parsed that text to rebuild index and graph-type state. That made
durable state depend on the parser round-tripping its own output, and a
replay-time parse failure could silently drop index or graph-type state.

[ADR-0012](./0012-wal-and-parquet-checkpoints.md)'s format-version discipline covers checkpoint manifest metadata
(`manifest.json`); no prior ADR commits to compatibility of WAL record
payloads. The repository is `0.3.0-alpha` (pre-1.0), so no WAL payload
compatibility guarantee exists to preserve.

## Decision

1. **Typed payloads.** The three catalog-DDL redo ops carry typed,
   serde-serializable models captured once at DDL-execution time from the
   parsed clause:
   - `CatalogIndexDetail` carries `redo::IndexDetailSpec` (with
     `redo::IndexKindSpec` for the index kind) plus the evaluated
     `OPTIONS` map as tagged JSON (`redo::props_json`).
   - `PutGraphType` and `SetCurrentGraphType` carry
     `graph_type::GraphTypeSpec`.
2. **No re-parse on replay.** `Graph::apply_redo` stores these models
   verbatim. It never formats a DDL string and never re-parses Cypher
   text during replay.
3. **Loud decode failure.** WAL records written in the old Cypher-text
   shape fail serde decode at replay: replay returns `Err` and startup
   fails loudly. There is no silent skip or drop of an undecodable
   record. (Covered by `legacy_string_payload_no_longer_decodes` in
   `crates/cypherlite-executor/src/redo.rs`.)
4. **Pre-1.0 rationale.** Breaking the old record shape without a
   migration path is accepted because the crate is pre-1.0
   (`0.3.0-alpha`) and no ADR has ever guaranteed WAL payload
 compatibility — [ADR-0012](./0012-wal-and-parquet-checkpoints.md)'s format discipline covers checkpoint
   manifests only. The change is a correctness hardening: replay can no
   longer silently lose index or graph-type catalog state.

## Alternatives considered

- **Keep Cypher-text payloads, keep re-parsing on replay** — rejected:
  durable catalog state would depend on format-then-reparse round-trips,
  and a replay-time parse failure can silently drop index/graph-type
  state (the exact hazard this ADR removes).
- **Version the WAL record envelope and migrate old records on open** —
  rejected: no WAL payload compatibility has ever been promised pre-1.0,
  and a migration shim for a shape only ever written by development
  builds adds permanent replay complexity for no released-format
  benefit.
- **Skip undecodable records and continue replay** — rejected: silently
  dropping a committed catalog op corrupts the recovered graph while
  reporting success; loud failure is the only acceptable behavior.

## Consequences

- Opening a directory whose WAL contains old-shape records for these
  three ops fails with a decode error; the remedy is to checkpoint with
  a build that writes the old shape (or discard the pre-1.0 WAL), not to
  expect silent recovery.
- All other redo ops are unchanged; this ADR allocates no new WAL
  segment, manifest, or checkpoint-format version.
- Any future WAL payload shape change after 1.0 needs its own ADR and an
  explicit compatibility story; this ADR's loud-failure stance is the
  pre-1.0 exception, not the permanent rule.

## Later references

[ADR-0114](./0114-wal-recovery-rejects-undecodable-records.md) extends Decision 3 to the final frame: a checksum-valid record that
does not decode fails loudly in every position, instead of being trimmed as a
torn tail.

[ADR-0120](./0120-ontology-document-persistence.md) gives the WAL a compatibility story for new record types. Segments
that may contain ontology ops start with `MDBWAL02` instead of `MDBWAL01`, so
older binaries fail at the segment header instead of misreading or trimming
the new records.

[ADR-0127](./0127-store-version-compatibility.md) defines the post-1.0 store-version compatibility contract
(migrate-on-open, hands-off, forward-only) and supersedes Decision 4's
pre-1.0 no-compatibility rationale: future WAL payload shape changes bump the
segment magic and ship a legacy decoder instead of failing loudly with no
path. Decision 3's loud-decode-failure rule still applies to records a binary
is expected to understand.
