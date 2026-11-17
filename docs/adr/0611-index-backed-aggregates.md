# ADR-0611: Index-backed grouped and extremum aggregates over a labeled property

Status: Accepted
Claim: 720cbb0a79277e3dddd32a1e1c45a41ef9151c2f

## Context

`MATCH (n:Person) RETURN n.age, count(*)` and `MATCH (n:Person) RETURN
min(n.age), max(n.age)` scan every node of the label, read the property and
aggregate row by row. On the benchmark graph the grouped form takes about 44 ms
and the extremum form about 56 ms. When the store already holds an index on the
(label, property) pair, the answer is sitting in it: a range index is ordered,
and an equality index holds one bucket per value.

The indexes are not exact counts. Their postings are built from every version
of every candidate node, so an overwritten value stays in its old bucket until
version collection, and a deleted node keeps its id there. A bucket size is an
upper bound, and a range index's first key may belong only to a stale version.
Intersecting with the label index removes deleted and unlabeled nodes, not
overwritten values. ADR-0589 set the precedent: an answer from maintained
structures is allowed only for a view that sees exactly the latest committed
state, and every other view declines to the scan.

## Decision

Add a dispatch shortcut, modeled on ADR-0589's recognizer, that answers a
narrow family of aggregates from existing indexes.

1. **Recognition.** The statement is exactly one plain pattern `MATCH (n:L)`
   with a single label, no other predicate, returning `n.p` grouped with
   `count(*)`, or any of `min(n.p)`, `max(n.p)`, `count(n.p)`. Everything else
   declines to the scan, including unlabeled patterns (an antipattern the
   engine does not optimize), `avg` and `sum`, and multi-label patterns.
2. **Sources.** Extremum answers read the first or last key of the range index
   on (L, p). Counts read the postings that already exist: a declared index, or
   a lazily built pair an earlier equality seek created. An aggregate never
   builds a pair and keeps no request counter; with no usable pair it declines.
3. **Extremum liveness.** The candidate id behind the first (last) key is
   checked against the statement's snapshot: live, labeled L, and still holding
   that value. A stale candidate steps to the neighbor key, so the answer is
   the scan's answer, not the index's.
4. **Exactness.** The shortcut runs only on ADR-0589's exactness window: no
   scratch overlay, no write in flight, the view equal to the latest committed
   state, and no older snapshot pinned; any other view declines to the scan.
   Within the window every posted id is still checked against the snapshot
   (live, labeled L, holding the bucket's value) before it counts, because
   bucket sizes include stale ids; the null group is the label's maintained
   live count less the counted ids. The index's dirty set is drained first.
5. **Finishing.** The shortcut produces grouped rows and the ordinary
   projection applies ordering, skip and limit, as in ADR-0589. PROFILE and
   EXPLAIN keep the scan's plan.

## Alternatives considered

- **Trust bucket sizes as counts.** Rejected: overwritten values inflate the old
  bucket, so the answer would be wrong under any update workload.
- **Maintain exact per-(label, property, value) counts on every write.**
  Rejected: taxes every write and every index for a read pattern the index
  already half-serves.
- **Optimize unlabeled patterns too.** Rejected: they are antipatterns, and the
  label-less scan warning already tells users to add a label.
- **Build a pair on demand when an aggregate arrives.** Rejected: a read would
  pay an index build and memory charge the user never asked for.
- **A planner rewrite instead of a dispatch shortcut.** Rejected for the reason
  ADR-0589 gave: the planner has no cost basis to choose between the two.

## Consequences

- The recognized shapes answer in time proportional to distinct values (or a
  few key steps for extrema) on a quiet store, with the scan's exact answer
  under overwrites, deletes, explicit transactions and pinned snapshots.
- Stores without a usable pair, and views that are not the latest committed
  state, are unchanged: they scan.
- Extremum reads decline unless both ends of the index are the same Int or
  String key variant with no off-order postings (mixed types, floats, lists),
  since order keys do not rank across types the way the scan does.
- A property the index posts nowhere (a NaN) is counted with the null group.
- Contract tests compare every shape to the scan under overwrite, delete,
  pinned-snapshot, explicit-transaction and unbuilt-pair cases.

## Future direction

A compound index on (key, aggregated value), in that order, could answer a
per-key min or max from the first and last posting of the secondary value,
extending the extremum read to grouped statements without a scan.
