# 0040. CREATE / DROP / SHOW INDEX name a (label, property) pair

- **Status**: Accepted
- **Date**: 2026-09-02
- **Deciders**: MemoryDB maintainers

## Context

ADR-0010 and ADR-0030 maintain always-on label, equality, prefix, and
trigram access methods. Those indexes are an engine detail: MATCH seeks
without a schema statement. The completion roadmap still needs a Cypher
surface so operators can **name** a `(label, property)` pair, persist
that name across WAL replay and Parquet checkpoints, and list it.

Making engine indexes opt-in would regress MATCH, the fraud loadtests,
and the TCK. The language surface must not turn access methods off.

## Decision

`CREATE INDEX` / `DROP INDEX` / `SHOW INDEXES` are **catalog names** for
a single node `(label, property)` pair. They do not create, drop, or
gate the ADR-0010 / ADR-0030 access methods. MATCH continues to seek
whether or not a named entry exists.

Syntax (Neo4j 5 style, one property):

- `CREATE [RANGE] INDEX [IF NOT EXISTS] [name] FOR (n:Label) ON (n.prop)`
- `CREATE [RANGE] INDEX [IF NOT EXISTS] [name] ON :Label(prop)`
- `DROP INDEX [IF EXISTS] name`
- `DROP INDEX [IF EXISTS] ON :Label(prop)`
- `SHOW INDEXES` (also `SHOW INDEX`)

Rules:

- Omitted name defaults to `{label}_{property}`.
- One named entry per `(label, property)`. Duplicate name or duplicate
  schema is a semantic error unless `IF NOT EXISTS` / `IF EXISTS`.
- The schema clause is standalone: it is the only clause in that query
  part. Do not mix with MATCH / CREATE graph / UNION.
- `CREATE` / `DROP INDEX` are writes. They emit `RedoOp::CreateIndex` /
  `RedoOp::DropIndex` and apply through `apply_redo` like other mutations
  (ADR-0014 / ADR-0015).
- `SHOW INDEXES` is a read. Columns: `name`, `label`, `property`,
  `type`. `type` is always `RANGE` (the Cypher 5 name for this object).
- Persist named entries on `manifest.json` as an optional `indexes`
  array (`#[serde(default)]`). No format-version bump (ADR-0005 / ADR-0022
  version stays 3).

Composite, relationship, full-text, and vector indexes are out of scope.

## Alternatives considered

- **Opt-in engine indexes** — `CREATE INDEX` would enable seeks.
  Rejected: MATCH / fraud / TCK already depend on always-on postings
  (ADR-0010, ADR-0030).
- **No language surface** — keep names out of Cypher. Rejected: the
  phase-8 contract is the DDL, and operators need a durable catalog
  name plus `SHOW`.
- **Neo4j 4 `CREATE INDEX ON :L(p)` only** — rejected as the sole
  syntax; kept as an accepted alias next to `FOR` / `ON`.
- **Bump `format_version`** — unnecessary; defaulted `indexes` loads as
  empty on old manifests. Rejected.

## Consequences

- Parser, planner, executor, WAL, and checkpoint gain schema clauses.
- Later-references notes on ADR-0010 and ADR-0030: the access methods
  stay engine-internal; this ADR only names them in the catalog.
- C ABI and language clients pass Cypher strings; no new FFI.
- The openCypher TCK corpus has no index DDL; this is not allowlist
  work (ADR-0056 / ADR-0006).

## Later references

ADR-0063: "postings stay always-on" now reads as **always available, built
on demand** — prefix/trigram postings materialize lazily per `(label,
property)` pair on first seek. The DDL still does not opt them in or out.

ADR-0614: "MATCH continues to seek whether or not a named entry exists"
holds again for equality — the lazy equality postings serve a keyed
lookup no declared pair covers, so the entry names an ordered structure,
not a gate. (Between the declared index structures landing in 2026-10
and ADR-0614, equality MATCH was declared-pair-only, without a reversing
decision; ADR-0614 restores the always-available intent.)
