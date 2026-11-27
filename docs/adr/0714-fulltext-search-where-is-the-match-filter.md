---
type: adr
area: cypher
claim: 6efd38481c298ce213e58992f6b22488f9ecee63
date: 2026-11-27
deciders: Relate maintainers
name: 0714-fulltext-search-where-is-the-match-filter
status: proposed
tags:
  - dialect
  - search
title: A fulltext SEARCH window's inner WHERE is the match filter only
REFERENCES:
  - "[[adr/0678-search-complex-patterns-and-entity-constrained-window]]"
  - "[[adr/0680-search-match-filter-semantics]]"
---

# ADR-0714: A fulltext SEARCH window's inner WHERE is the match filter only

## Context

The inner `WHERE` of a `SEARCH` window has served both index kinds with
one predicate grammar since [ADR-0678](./0678-search-complex-patterns-and-entity-constrained-window.md)
one grammar for both. A 2026-11 re-probe of live Neo4j split the
kinds: on a fulltext index the inner `WHERE` is the explicit match
filter only, and a property comparison inside a fulltext window is
rejected (`WHERE movie.rating > 7` → the invalid-predicate rejection,
code 42I73). The engine accepted it and returned rows.
that row failed.

The vector side is unchanged: the vector window's single-stage filtering keeps
its six predicate forms there, property comparisons and their `AND`
combinations included, with the disjunctive forms rejected.

## Decision

The search-filter lowering is kind-aware. A `FULLTEXT INDEX` source's
inner `WHERE` admits exactly the match-filter family — the entity-set
form (`<binding variable> IN <expr>`) and its id-spelled twin
(`id(<binding variable>) IN <expr>`) — and rejects every property
predicate (comparison, existence, property-in-set) with the authored
invalid-predicate sentence, `SyntaxError` class, the reference server's preferred
42I73 shape: `invalid predicate for search with filters. The search
filter predicate '<predicate>' is not supported, see documentation for
all supported predicates.` The sentence renders the term's own
spelling through the front's structural-expression renderer for the
predicate grammar's shapes (the grouping-element precedent). A `VECTOR
INDEX` source keeps the vector filter forms unchanged; a `COMBINE` tree applies
each nested source its own kind's rule.

[ADR-0678](./0678-search-complex-patterns-and-entity-constrained-window.md)
carries a Later references note pointing here: its "the inner WHERE
serves both kinds" line is this decision's superseded record.

## Consequences

- The fulltext error row passes; no vector row moves (their forms and
  rejections are pinned by the engine's own
  integration tests).
- The property-predicate forms stay reachable on fulltext sources
  through the outer `WHERE` of the MATCH, which was always legal and
  answers the same rows — only the window-internal filter narrows.
- The wrong-variable and self-reference rejections keep their more
  specific errors for the match-filter forms themselves; the
  invalid-predicate sentence is the property-predicate forms' own.
