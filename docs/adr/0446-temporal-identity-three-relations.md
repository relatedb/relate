---
type: adr
area: engine
claim: 333c359ec76fa2bea937705945bf682ab8dec3a0
name: 0446-temporal-identity-three-relations
status: proposed
status_note: 2026-10-29
tags:
  - values
title: Temporal identity — equality, ordering and equivalence are three relations
REFERENCES:
  - "[[adr/0021-opencypher-temporal-values]]"
  - "[[adr/0056-opencypher-tck-language-contract]]"
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0172-incremental-constraint-validation]]"
  - "[[adr/0184-composite-key-smallest-bucket-probe]]"
  - "[[adr/0185-relationship-equality-postings]]"
---

# ADR-0446: Temporal identity — equality, ordering and equivalence are three relations

## Context

A temporal value answers three distinct questions, and the pinned openCypher
TCK 2024.3 ([ADR-0021](./0021-opencypher-temporal-values.md), [ADR-0056](./0056-opencypher-tck-language-contract.md)) pins them apart on the zoned-date-time DST
fall-back overlap — `2025-10-26T02:30+02:00[Europe/Stockholm]` versus
`2025-10-26T02:30+01:00[Europe/Stockholm]`:

- `=` compares the **local wall clock plus the zone identity**, so the two
  halves are equal (`Temporal::equals`).
- Ordering (`<`, `>`, `ORDER BY`, `min`/`max`) compares **instants**, so the
  same halves are ordered (`Temporal::compare` / `total_cmp`).
- Equivalence (`DISTINCT`, `GROUP BY`, `UNION` dedup, `count(DISTINCT …)`,
  `coll.distinct`) compares the **UTC instant plus the zone**, so the pair is
  two classes (`GroupKey::new`).

Everything that *buckets* values to answer an equality question was keyed by
the equivalence relation: the lazily materialized property-equality postings
([ADR-0063](./0063-lazy-string-index-postings.md)/[ADR-0185](./0185-relationship-equality-postings.md)), the RANGE index's equality probe, the uniqueness probe
and scan re-check ([ADR-0172](./0172-incremental-constraint-validation.md), [ADR-0184](./0184-composite-key-smallest-bucket-probe.md)) and the pattern match `MERGE` runs.
Each of those verifies candidates with `Value::eq_non_null` (i.e. `=`), so a
bucket finer than `=` silently loses answers: `GroupKey` keyed a zoned
datetime by its UTC instant plus zone, while `=` also equates the DST overlap
(the same local wall clock and zone, different instants).

Observed with a RANGE index or a uniqueness constraint present (an
index-backed constraint owns a backing range index):

- `MATCH (n:X {dt: a})` missed a stored `b` even though `a = b`;
- `CREATE CONSTRAINT … IS UNIQUE` accepted both halves — two nodes carrying
  `=`-equal `dt` values;
- `MERGE (:X {dt: b})` created a second node instead of matching the equal
  one.

Without an index the same statements scanned and answered by `=`, so the
presence of an index changed the answer. [ADR-0021](./0021-opencypher-temporal-values.md) decided that comparison
follows the TCK but never said which of the three relations an equality
*bucket* must encode.

## Decision

Keep the three relations exactly as the TCK defines them. Every value
*identity* bucket — grouping **and** the property index, uniqueness probe and
`MERGE` — uses the **equivalence** relation (UTC instant plus zone). `=` is
the coarser relation: the overlap pair is equal yet two index entries.

- `Temporal::datetime_eq_identity` is the single definition of a zoned
  datetime's `=` identity: `(local wall clock, ZoneId)`. `Temporal::equals`
  compares exactly that pair.
- `GroupKey::new` and `GroupKey::for_property_index` both encode the
  equivalence relation, so the lazy postings, the RANGE index, the uniqueness
  probe and `MERGE` key the value identity.
- The RANGE index posts a zoned datetime under its order key (instant, for
  `<`/`>`/`ORDER BY` seeks) and under the identity key, and an equality probe
  reads the identity posting. The order key stays instant-keyed because
  ordering is instant-keyed, but an instant-keyed bucket is coarser than the
  identity (two zones can share an instant), so it cannot serve an equality
  probe.

This corrects the original decision, which derived the buckets from `=`.
Verified against Neo4j 2026.09 with a RANGE index and with a uniqueness
constraint over the two overlap halves: the indexed equality seek returns one
row (the exact half), the constraint accepts both nodes, and `MERGE` creates
a second node — while a label scan still applies `=` and matches both.

## Alternatives considered

- **Collapse the three relations into one** (make `=` instant-based, or make
  grouping use `=`). Rejected: the pinned corpus requires the overlap pair to
  be equal, ordered *and* two distinct groups; any single relation
  contradicts at least one of those.
- **Decline the index for a zoned-datetime equality seek** (fall back to the
  label scan). Rejected: it pays O(label) for every datetime `MATCH`/`MERGE`
  exactly where an index exists to avoid it, and it contradicts Neo4j's own
  indexed answer (one row, not the `=`-equal pair).
- **Coarsen the index bucket to `=`** (the original decision). Rejected by
  the measured suite: Neo4j's RANGE index and uniqueness constraint keep the
  overlap halves apart, so a `=`-coarsened bucket makes an indexed seek
  return two rows where Neo4j returns one and rejects a pair Neo4j accepts.

## Consequences

- `Temporal::datetime_eq_identity` remains the one place a zoned datetime's
  `=` identity is derived; `Temporal::equals` uses it, while the index and
  grouping both use the equivalence identity.
- A RANGE index on a datetime property stores one extra identity posting per
  value (memory only; order keys and range seeks are unchanged).
- A uniqueness constraint on a zoned datetime accepts both overlap halves,
  an indexed equality seek finds only the exact half, and `MERGE` through the
  constraint's backing index creates the second node — all matching Neo4j
  2026.09 and pinned by
  `cypherlite/cypherlite/tests/temporal_identity_split.rs`.
- An indexed equality seek and an unindexed scan can disagree for the overlap
  pair (`=` is coarser than the index identity). That asymmetry is Neo4j's
  own, not an engine defect.
- [ADR-0021](./0021-opencypher-temporal-values.md)'s "comparison follows the TCK" is refined, not replaced: this ADR
 names which relation each consumer uses (see [ADR-0021](./0021-opencypher-temporal-values.md)'s Later references).
