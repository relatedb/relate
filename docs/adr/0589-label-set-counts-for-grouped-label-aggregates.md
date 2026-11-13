# ADR-0589: Label-set counts for grouped label aggregates

Status: Proposed
Claim: 84099850ced1ad00aa876defcfb5341c57c160c6

## Context

ADR-0387 answers whole-population counts from maintained statistics: the exact
per-label live count and the exact node total. Neither can answer a grouped
aggregate over labels. `MATCH (n) RETURN labels(n)[0] AS kind, count(*)` (the
"kinds of nodes" overview every schema browser and dashboard opens with) needs
the distribution of label *sets*: a multi-label node counts once per label in
the per-label map, and the first label is a property of the node's label order,
not of any one label. The count shapes decline, and the per-row walker scans
every node, builds a node value for it, and evaluates the key through the
generic expression walker. Measured on a ~107k-node graph this takes about 29 ms
against about 2 ms for a relational engine with an index on its kind column, while
the ungrouped count answers in 0.02 ms. The per-node costs (scan floor plus key
evaluation) are what a cheaper scan could trim but not remove, so closing the
gap needs an answer whose cost does not scale with the node count.

## Decision

Maintain a **label-set count map** in the same count bundle as the label and
node counts (ADR-0387): the exact number of live nodes per distinct label set,
keyed by the node's label ids in label order, including the empty set.

1. **Maintenance** rides the seams the other counts ride, under the bundle's one
   guard: node create and node delete bump the node's set; a label add or
   remove moves one node from its old set to its new one in a single guarded
   step, so a reader never sees the intermediate state. Abort, restore and
   open recompute the map in the wholesale count rebuild, in the same pass
   that folds the label and total counts. Drained entries leave the map, and
   the same loud pairing contract applies (a removal without its paired add is
   a broken store and fails where it happened).
2. **Charging.** Entries are query-controlled, so each entry charges the memory
   accounting on creation and refunds on drain, at a per-entry constant plus
   the key's width, and the structure recount reads the same constant. The
   admission estimate prices a batch's set creations and drains the same way.
3. **Recognition.** A new shape recognizer answers only the statement of
   exactly one plain, variable-bound node match returning group keys plus one
   `count(*)` or `count(n)`, where every key depends on the node only through
   its label list (`labels(n)`, `size(labels(n))`, a literal subscript of it, or
   a label predicate on it). `ORDER BY` items must name an output column or
   repeat an output expression; `SKIP`/`LIMIT` are unrestricted. Anything else
   declines to the scan.
4. **Evaluation.** Each key expression is evaluated by the ordinary expression
   evaluator over one stand-in node per distinct label set, so semantics
   (errors, nulls, subscript bounds, label predicates) are the evaluator's, not
   a second implementation. Counts are summed per output key (grouping
   equivalence is the aggregation's own key), then the ordinary projection
   finishes ordering and paging over the grouped rows. The copy of the set
   map and the group table charge the expression budget as they land, so a
   paged statement refuses at the cap as the scan's aggregation does.
5. **Visibility.** The map describes the latest committed state, as the other
   counts do. It answers only for a view that sees exactly that state, under
   the same two-step check as the counting operator (no scratch overlay, no
   write in flight, snapshot equal to the latest commit, re-checked under the
   bundle guard). Any other view, including an explicit transaction and a
   pinned older snapshot, declines to the scan, which reads its own snapshot.
   A graph whose sets approach its node count (above a fixed floor, fewer
   than a few nodes per set on average) declines as well: the map has no speed
   to offer there. The decline is the family's existing contract: an absent answer, never a
   substituted or approximate one. PROFILE runs the scan.

## Alternatives considered

- **Make the scan cheaper** (batched label reads, a specialized key
  evaluator). Rejected: it lowers the per-node cost, but the answer stays
  proportional to the node count and cannot reach parity with a maintained
  count.
- **Per-label counts plus first-label counts.** Rejected: it answers one query
  text and breaks on any other key over the label list; the set distribution
  answers every label-only key by evaluation.
- **Versioned (snapshot-exact) counts.** Rejected: answering for any pinned
  snapshot needs counts per timestamp or per-chain deltas, which taxes every
  write and every read for a case the scan already serves correctly.
- **Derive the answer from the label index.** Rejected: the index is an upper
  bound that keeps deleted ids until checkpoint cleanup, and it is keyed per
  label, not per set.
- **A planner-level rewrite instead of a dispatch shortcut.** Rejected: the
  counting operator is already a dispatch shortcut with the same gate, and the
  planner has no cost basis for choosing between the two.

## Consequences

- The recognized grouped label aggregates answer in time proportional to the
  number of distinct label sets, not nodes. Writes pay one extra map update per
  node create, delete or label change under a lock they already hold.
- Memory grows with the number of distinct label sets, charged and capped like
  the other maintained counts. A graph whose nodes all carry distinct label
  sets pays about one entry per node; its grouped queries decline to the scan.
- Results equal the scan's, so differential tests against the scan across
  create, delete, label add and remove, abort, restore and reopen pin the
  contract. Row order without `ORDER BY` is unspecified, as before; the
  answer's order is deterministic.
- Transactions and pinned snapshots keep scan speed. Widening the map to
  snapshot-exact answers would be a later decision.
- ADR-0387's count family gains a third map; its recognizer is untouched.
