---
type: adr
area: engine
claim: 504326c8e9b8b7e05dc3b9eafebecd21fdd6db5b
name: 0372-serving-index-conflict
status: accepted
tags:
  - catalog
  - index
title: The schema-rule conflict is narrowed to serving index kinds
REFERENCES:
  - "[[adr/0339-property-validation-verdict-walk]]"
---

# ADR-0372: The schema-rule conflict is narrowed to serving index kinds

## Context

Declaring `Question.title :: STRING NOT NULL` in a graph type promotes to a
`NODE_PROPERTY_TYPE` constraint plus an existence rule on `Question.title`;
a `CREATE FULLTEXT INDEX … ON EACH [q.title]` on the same property then
fails with `IndexWithNameAlreadyExists` — `catalog_create_index`'s
`conflicts_with_constraint` treats *any* constraint on the same
entity/label/properties as a conflict, and the constraint-creation path
does not check existing indexes at all (LABS-274). A full-text (or vector)
index and a property-type/existence constraint are not equivalent schema
rules — one serves text search, the other validates values — so a store
whose data carries both cannot declare its real schema (the Stack Overflow
notebook documents the workaround; LABS-275).

Neo4j 2026.09 was probed directly (LABS-275): it rejects in both orders
(`22N70 equivalent index already exists` / `22N65 equivalent constraint
already exists`) — its equivalence check treats *any* index on the property
as satisfying the existence constraint. Since [ADR-0339](./0339-property-validation-verdict-walk.md), a uniqueness/key
constraint's backing index is always RANGE — the only index kind that can
serve a constraint.

## Decision

The schema-rule conflict is narrowed to **serving pairs**: an index and a
constraint conflict only when the constraint is index-backed
(uniqueness/key) *and* the index kind is RANGE, on the same
entity/label/properties. Concretely:

- Property-type and existence constraints coexist with every index kind —
  they are not backed, so no index can serve or shadow them.
- FULLTEXT, VECTOR and POINT indexes coexist with every constraint — they
  serve search, not validation, and cannot back a constraint.
- A RANGE index and an index-backed constraint on the same
  entity/label/properties keep conflicting (the RANGE backing would
  duplicate), in both creation orders.

This diverges from Neo4j 2026.09's blanket equivalence and is recorded as
an intentional divergence: the declared schema becomes as expressive as
the data's real schema.

## Alternatives considered

- **Keep Neo4j parity — reject in both orders** — rejected: the declared
  schema stays less expressive than the data's real schema and the
  notebook keeps its documented workaround, exactly the gap LABS-265 set
  out to close.
- **Express indexes inside the graph type** (a `WITH INDEX` form) —
  rejected: GQL has no such syntax yet; a larger design with no reference
  to follow.

## Consequences

- The notebook's `Question.title` can be declared `STRING NOT NULL` while
  its full-text index exists; its section 3 prose drops the workaround.
- The divergence from Neo4j 2026.09 is deliberate and probed-confirmed;
  CIP-eval scenarios pin both creation orders.
- LABS-274 (CypherLite accepting index-then-constraint for the range-backed
  pair) is resolved for non-serving pairs by this decision; the
  range-backed pair keeps a single conflict rule in both orders.
