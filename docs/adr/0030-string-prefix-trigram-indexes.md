---
type: adr
area: engine
date: 2026-09-01
deciders: MemoryDB maintainers
name: 0030-string-prefix-trigram-indexes
status: accepted
tags:
  - index
title: Equality, prefix, and trigram indexes on string properties
REFERENCES:
  - "[[adr/0010-label-property-indexes]]"
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0024-match-rewrite-planner]]"
  - "[[adr/0026-eager-residual-match-predicates]]"
  - "[[adr/0040-create-drop-index]]"
  - "[[adr/0063-lazy-string-index-postings]]"
---

# 0030. Equality, prefix, and trigram indexes on string properties

## Context

[ADR-0010](./0010-label-property-indexes.md) indexes label presence and label+property **equality**.
`MATCH (c:Customer) WHERE c.name CONTAINS $q` is a label scan plus a
string predicate on every hit. The fraud read-only load test’s
`search_customer` use case (`$q = "Customer"`) is O(label) and dominated
the mix at scale 0.30. [ADR-0010](./0010-label-property-indexes.md) already deferred range/text indexes.

`CONTAINS` with no index will always be O(label). Equality already seeks.
`STARTS WITH` can use a sorted map. `CONTAINS` needs an inverted
substring access method.

## Decision

Keep indexes engine-internal (no `CREATE INDEX`). On every string node
property, maintain three access methods, updated on create / SET /
REMOVE / DELETE like `prop_index`:

1. **Equality** — existing [ADR-0010](./0010-label-property-indexes.md) `(label, key, literal) → ids`.
   Unchanged. `{id: $id}` / `n.prop = $q` stay IndexSeek.
2. **Prefix** — per `(label, key)`, a `BTreeMap` of the string value →
   ids. `STARTS WITH` is a range over that map, then a snapshot
   visibility + `starts_with` check (indexes remain a superset,
 [ADR-0018](./0018-concurrent-snapshot-reads.md)).
3. **Trigram** — overlapping character 3-grams of the value (and 1- and
   2-grams so needles shorter than 3 can post). `CONTAINS $q` intersects
   the needle’s grams, then verifies `contains` at `snapshot_ts`.

The executor seeds `MATCH (n:Label) WHERE n.prop CONTAINS $q` (or
`STARTS WITH`) from these postings when the conjunct is a top-level AND
leaf on a node introduced in that MATCH. Residual `WHERE` still runs
(false-positive grams, overlay creates, MVCC).

Empty `CONTAINS` needle: do not post; fall back to the label scan
(O(label)). `OR` / `XOR` / `ENDS WITH` / `=~` are not folded.

## Alternatives considered

- **Leave CONTAINS as a label scan** — correct, `search_customer` stays
  the mix tax. Rejected as the only access method.
- **User-visible `CREATE INDEX`** — extra TCK surface. Rejected (same as
 [ADR-0010](./0010-label-property-indexes.md)).
- **Prefix only** — `search_customer` and identity `email CONTAINS` are
  not prefixes. Rejected as the sole text method.
- **External search engine** — overkill for a RAM-resident graph.
 Rejected ([ADR-0010](./0010-label-property-indexes.md)).

## Consequences

- Selective needles (and identity `email CONTAINS`) avoid a full label
  walk. A needle that occurs in every value (fraud `name` is
  `"Customer {n}"`, `$q` is `"Customer"`) still returns the whole label;
  Wave 1 top-K still applies after the seek.
- Mutation and GC must drop unused prefix/trigram postings the same way
  as `prop_index`.
- [ADR-0024](./0024-match-rewrite-planner.md)/[ADR-0026](./0026-eager-residual-match-predicates.md) still do not fold `OR` into a seek. Pattern predicates
  with `OR any(…)` stay a scan unless every disjunct is itself a seek
  and we union (out of scope here).

## Later references

[ADR-0040](./0040-create-drop-index.md): `CREATE` / `DROP` / `SHOW INDEX` persist a catalog name for a
`(label, property)` pair. Prefix and trigram postings stay always-on;
the DDL does not opt them in or out.

[ADR-0063](./0063-lazy-string-index-postings.md): prefix and trigram postings become **lazy** — built on the first
`CONTAINS` / `STARTS WITH` seek of a `(label, key)` pair, then caught up
incrementally from per-pair dirty sets. The access methods, the superset
invariant, and the residual re-verification are unchanged; eager
maintenance on the write path was removed because it cost ~86% of
bulk-`CREATE` wall time.
