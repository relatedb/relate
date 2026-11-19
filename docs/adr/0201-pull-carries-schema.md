---
type: adr
area: surface
date: 2026-10-11
deciders: CypherLite maintainers
name: 0201-pull-carries-schema
status: proposed
tags:
  - load
title: Pull carries the source's constraints and indexes
REFERENCES:
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0156-directory-clone]]"
  - "[[adr/0158-streaming-single-process-pull]]"
  - "[[adr/0291-graph-examples-extras-and-schema-ddl]]"
---

# 0201. Pull carries the source's constraints and indexes

- **Related**: [ADR-0080](./0080-cli-pull-command.md) (original sync), [ADR-0156](./0156-directory-clone.md) / [ADR-0158](./0158-streaming-single-process-pull.md) (streaming
 pull), [ADR-0291](./0291-graph-examples-extras-and-schema-ddl.md) (schema DDL is runnable)

## Context

`cypherlite pull` copied data only. Every `CALL db.schema.visualization()`
walkthrough in the graph-examples differential suite therefore diffed
against the Neo4j reference: the source's constraints and indexes existed
on the reference store but not in the pulled store (LABS-257; 7 DIFFs
across datasets).

The engine already runs and enforces the needed DDL (probed against the
built CLI, 2026-10-11):

- `CREATE CONSTRAINT … IF NOT EXISTS FOR (n:L) REQUIRE n.p IS UNIQUE` /
  `IS NOT NULL` / composite `IS NODE KEY` / `IS TYPED <type>`;
- the relationship-side equivalents
  (`FOR ()-[r:T]-() REQUIRE … IS UNIQUE` / `IS NOT NULL` /
  `IS RELATIONSHIP KEY` / `IS TYPED`);
- `CREATE INDEX … IF NOT EXISTS FOR (n:L) ON (…)` and the `TEXT` / `POINT`
  spellings, on node and relationship targets alike;
- `CREATE FULLTEXT INDEX … ON EACH […]` parses and runs, but Neo4j's
  fulltext/vector indexes carry configuration (analyzer, dimensions,
  similarity) that `SHOW INDEXES` does not return as replayable DDL —
  they stay out of the mapping (recorded gap, skipped with a note).

## Decision

After the data stream completes, `load_streaming` fetches the source
schema over the same Query API v2 connection
(`SHOW CONSTRAINTS YIELD name, type, entityType, labelsOrTypes,
properties, propertyType` and `SHOW INDEXES YIELD name, type, entityType,
labelsOrTypes, properties, owningConstraint, state`) and replays each row
as a standalone `CREATE … IF NOT EXISTS` statement against the target,
one statement per row inside the loader's existing `with_txn` transaction
policy.

Mapping: `NODE_PROPERTY_UNIQUENESS` → `IS UNIQUE`,
`NODE_PROPERTY_EXISTENCE` → `IS NOT NULL`, `NODE_KEY` → `IS NODE KEY`,
`NODE_PROPERTY_TYPE` → `IS TYPED <propertyType>`, and the
`RELATIONSHIP_*` equivalents on a `FOR ()-[r:TYPE]-()` target. Indexes:
`RANGE` → `CREATE INDEX`, `TEXT` → `CREATE TEXT INDEX`, `POINT` →
`CREATE POINT INDEX`, on node or relationship targets. Filtered out
without counting: indexes with a non-null `owningConstraint` (they back a
constraint and arrive with its replay), non-`ONLINE` indexes, and
`LOOKUP` indexes (engine-internal). Everything else — unknown constraint
kinds (e.g. node-label existence), `FULLTEXT`/`VECTOR` indexes, rows with
no label or no properties — is skipped with a stderr note and counted.

Schema replay never fails a pull: the data is already loaded, so a source
that cannot answer the SHOW queries (older server, missing privilege)
yields a stderr warning and zero counts, and a single rejected DDL is one
skip. `LoadStats` gains a `schema: SchemaStats` field
(`constraints_applied`, `constraints_skipped`, `indexes_applied`,
`indexes_skipped`, all `Default`) and the pull output line becomes
`pulled N new / M reused nodes, P new / Q reused rels (+ C constraints,
I indexes; S schema skipped) to PATH`.

Engine gaps found while probing: none among the mapped kinds — node and
relationship uniqueness/existence/key/property-type constraints and
RANGE/TEXT/POINT indexes all run and enforce. The recorded gaps are the
unmapped `FULLTEXT`/`VECTOR` index kinds (configuration not recoverable
from `SHOW INDEXES`) and any future constraint kind outside the mapping
table.

## Alternatives considered

- **Do nothing / document-only**: the differential DIFFs stay forever and
  every pulled store silently loses the source's integrity rules.
  Rejected: the engine already enforces the DDL, so the fix is cheap.
- **Pull schema as a data manifest** (record the SHOW rows into the store
  instead of running DDL): `db.schema.visualization()` would still see an
  empty schema and constraints would not be enforced. Rejected: the point
  of the pull is a working store, not a description of one.
- **Reconstruct constraints from `ownedIndex` metadata only**: loses
  existence, key and property-type constraints (they own no index) and
  cannot distinguish constraint kinds. Rejected: `SHOW CONSTRAINTS` is
  the authoritative source and costs one extra request.

## Consequences

- Pulled stores enforce the source's uniqueness/existence/key/property-type
  constraints and carry its RANGE/TEXT/POINT indexes;
  `db.schema.visualization()` walkthroughs diff clean (LABS-257).
- Two extra Query API requests per pull (both plain JSON, bounded
  responses).
- A pull against a source that hides schema (privileges) still succeeds;
  the skipped counts and stderr notes say what was lost.
- New gap to revisit if needed: FULLTEXT/VECTOR index replay once the
  engine's DDL and the SHOW metadata make the configuration recoverable.
