# 0595: Reclaim label-index postings at the apply point

Status: Proposed
Claim: 14365c0899b53ae1572f48d196d3bce7ebe58b61

## Context

The label index maps each interned label to the ids of every node that
ever carried it; entries are added when a write applies and removed only
by the checkpoint's GC pass, which respects the snapshot horizon
(`ADR-0171` rule 8). A store that does not checkpoint — an in-memory
store, or a durable one between checkpoints — therefore accumulates the
ids of deleted or relabeled nodes indefinitely.

The label probe (`ADR-0171`) walks this set per label-accounted write to
decide label liveness. When the live carrier count is below the
threshold, the probe must check **every** id in the set to prove it — an
absence proof with no early exit. Tombstoned ids interleave with live
ones in hash order, so each statement pays a walk over the whole
accumulated span: measured on the bulk-create loadtest (one clear plus
one thousand-row create per repetition), per-statement latency grows
with the accumulated id count and the probe family holds ~57% of CPU at
two hundred thousand accumulated ids — bounded only by reclamation that,
for these stores, never comes.

## Decision

Reclaim label-index postings incrementally, at the removal sites, in
the same apply that kills them:

- The removal sites append the dying postings to a per-label candidate
  set on the graph: a tombstone (`DELETE`), a label drop (`REMOVE`),
  and a first label leaving the empty-label bucket — a ternary
  transition that is a removal site even though the write is an add.
  The buckets come from the same rule the insert path applies: the
  record's labels, or the empty-label bucket when it carries none. The removal site knows the ids and labels exactly;
  no read path mutates index state, and no store's reclamation depends
  on a probe happening to walk the dead ids.
- The same `apply_redo` — the single-writer moment where index postings
  are added — drains the candidates at its tail, by posting fate: an id
  whose **live** version carries the label is current and drops from
  the candidates; an id whose carrying version's visibility interval
  `[xmin, xmax)` ends past the snapshot horizon (`ADR-0038` pins, else
  the commit timestamp) is still visible to a pinned snapshot and is
  requeued for a later drain; an id no snapshot at or after the horizon
  can see is removed from its label's set. The interval rule, decided
  by the checkpoint GC's own `dead_at` predicate, matters where a ts
  probe would mislead: a version created after the horizon is invisible
  to a probe at the horizon while its live interval is wide open.

The candidate set holds each id once, so a long-lived pin defers but
never multiplies the drain's work. The single-writer apply makes the
removal race-free against posting inserts.

## Alternatives considered

- **Iterate the label index in id order** so an early exit can skip a
  dead prefix: rejected — it helps only when enough live carriers exist
  to stop the walk early. The measured cost is the absence proof, which
  must visit every id regardless of order.

- **Run the checkpoint GC per commit**: rejected — it walks every node
  and rel chain of the graph, O(store) per statement; the incremental
  drain is O(walked) and only where the probe already paid.

- **Cache probe results across statements** (persist the memo):
  rejected — the memo's facts are snapshot-bound; a later statement's
  snapshot can see a re-added label, so cached "not a carrier" facts
  would need invalidation machinery the drain gets for free.

## Consequences

- The label index set stays within an apply's reach of the ids any
  snapshot can see, so the probe's walk is bounded by live-plus-recent
  ids, not by store history. The measured super-linear create ladder
  becomes flat.
- A reader iterating a posting set (the snapshot label read, the scan
  candidates) may still see ids whose visible version lacks the label;
  removal never takes an id a live snapshot can see.
- An id re-labeled between its suspecting and the drain survives (its
  live version carries the label again); an id deleted after being
  suspected is reclaimed one commit later at worst. A long-lived pin
  defers the reclamation of what it can see — the requeued candidates
  are re-checked each apply.
- The checkpoint GC remains the reclamation of record for the equality
  and pair postings; this decision covers the label postings only.
