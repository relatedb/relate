# 0010. Label and property indexes for MATCH

- **Status**: Accepted
- **Date**: 2026-08-28
- **Deciders**: MemoryDB maintainers

## Context

UNWIND/MATCH loads of fraud transfers scan every `Account` for each row.
Schema-optional LPG (ADR-0005) still needs *optional* indexes: label
presence and label+property equality.

## Decision

Maintain in-memory indexes on the graph:

- label → node ids
- (label, property key, value literal) → node ids

The executor uses them when a node pattern has labels and/or equality
property maps. Indexes are not Cypher schema; they are an engine
implementation detail. No `CREATE INDEX` statement in v1.

## Alternatives considered

- **Full scans only** — correct, too slow for dataset tests. Rejected.
- **User-visible `CREATE INDEX` in v1** — extra TCK/language surface.
  Rejected until the language subset needs it.
- **External search engine** — overkill for RAM-resident graphs. Rejected.

## Consequences

- Mutations update indexes (create, SET, REMOVE, DELETE).
- Snapshot load via Parquet rebuilds indexes through `create_node`.
- A later ADR may add relationship-type indexes or range indexes.

## Later references

ADR-0024: the planner folds MATCH `WHERE` equality and orients paths so
the executor’s existing `lookup_nodes` seek is used when Cypher text
would have scanned first.

ADR-0030: prefix (`STARTS WITH`) and trigram (`CONTAINS`) indexes on
string properties. Equality seeks are unchanged. No `CREATE INDEX`.

ADR-0031: in/out adjacency is partitioned by relationship type (the
relationship-type index ADR-0010 deferred).

ADR-0040: `CREATE` / `DROP` / `SHOW INDEX` name a `(label, property)`
pair in the catalog. Access methods stay always-on; the DDL does not
enable or disable seeks.

ADR-0614: equality MATCH seeks the lazy always-on postings again when
no declared RANGE index covers the pair; a zoned-datetime value
declines to the scan so the un-indexed answer keeps `=` semantics.
