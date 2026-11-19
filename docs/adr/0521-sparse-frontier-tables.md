---
type: adr
area: engine
claim: b86b39129d8ff2ca3e230774768ddcb9cde5fd07
name: 0521-sparse-frontier-tables
status: proposed
tags:
  - pattern-matching
  - perf
title: Sparse frontier tables — per-run working memory sized by the pairs a run touches
REFERENCES:
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
  - "[[adr/0516-frontier-rearrival-budget-fallthrough]]"
SUPERSEDES:
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
---

# ADR-0521: Sparse frontier tables — per-run working memory sized by the pairs a run touches

## Context

[ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 6's allocation half gave the frontier-NFA kernel dense
tables indexed by node id: the first-arrival parent table
(`span × states` slots of `Option<Parent>`, 32 B each), the dedup
bitmask, the endpoint and candidate marks (one entry per node of the
store's node-id span), and the re-arrival rule's verdict memo and
chain-proof stamps (another 64 B per node of span). The tables were
allocated once per engaged clause and recycled across the clause's
seeds, and the per-seed reset walked the run's first-arrival pair ledger
— O(pairs touched). A fixed byte ceiling gated the clause before
allocation, on an estimate of `span × states` slots plus the per-node
columns.

Profiling a forced-NFA leg against the per-row walker on identical rows
showed the per-clause allocation was the kernel's whole per-query floor:
about 170 B per node of span per engaged clause, linear in span,
independent of the answer. For a 7-row answer: no measurable gap at a
1k-node span; 17 MB and +0.16–0.19 ms per query at 100k (a pokec-sized
store); 170 MB and +2.5 ms at 1M; 680 MB and +10.4 ms at 4M. Between
87% and 96% of attributable samples were table construction (the
`span × states` slot vector, which must be written because `None` is
not all-zero, and the rule's verdict vector), split about evenly between
the BFS tables and the rule buffers. NFA compilation, seeding, epsilon
closure and the BFS itself together took under 2%. The ceiling's
estimate was also wrong in the other direction: it charged the pair
ledger at `span × states` entries, but the ledger grows lazily with the
pairs actually reached.

The run already kept the information a sparse layout needs: a
first-arrival pair ledger in arrival order. What it lacked was a way to
find a node's per-node facts without a span-indexed array.

## Decision

1. **One sparse node-keyed map, generation-stamped.** A new
   executor-internal `NodeMap<V>` maps a node id to a per-node value:
   open addressing with linear probing, a Fibonacci (multiplicative) hash
   of the id, a power-of-two slot count that starts at 16 on the first
   insertion and doubles before the live count would pass 3/4 of the
   slots. Each occupied slot carries the generation that wrote it (a
   `NonZeroU32`, so an empty slot costs no tag byte); a slot is live only
   under the map's current generation. Within a generation no key is
   ever removed, so a probe stops at the first non-live slot.

2. **The BFS tables.** The run's per-node facts live in one record per
   touched node, in a `NodeMap`: the dedup bitmask (`u64`, one bit per
   automaton state — the compiler's 64-state ceiling is unchanged) and
   one packed `u32` holding the ledger index of the node's newest pair
   plus the endpoint and candidate marks. The record is 16 bytes (12 of
   data, padded to the `u64`'s alignment), so a map slot is 32 bytes: the
   4-byte stamp, 4 bytes of padding, the 8-byte node id, and the record.
   The
   pair ledger stays the BFS queue and level structure, and now also
   carries the parent table: each entry holds the pair, its first
   arrival's parent (`None` only for the seed pair), and the ledger index
   of the same node's previous pair. Looking up a (node, state) parent
   is one record lookup plus a walk along that node's pairs (at most its
   state count). Parents exist only for pairs the run enqueued. The
   record map is the run's only "which nodes did this run touch"
   structure; no dense copy of that fact exists.

3. **The rule's buffers.** The re-arrival rule's verdict memo is a
   `NodeMap<Verdict>` keyed by searched target, and the chain proof's
   visit set is a `NodeMap<()>`. Both are sized by the nodes the rule
   actually asks about.

4. **Reset by generation bump, O(1).** The per-seed reset bumps the
   record map's generation and truncates the ledger. The rule's per-run
   memo reset and its per-walk visit-set reset are the same bump. No
   reset writes untouched memory. A slot array keeps its capacity across
   resets within a clause (the largest seed's footprint so far, never a
   span-sized one). Stale entries are dropped when a slot is reused, when
   the table grows (the rehash carries live entries only), or when the
   map drops. The generation counter wraps once per 2³² resets, and the
   wrap is the one full sweep.

5. **No dense fallback and no crossover policy.** One layout serves every
   run. The large-frontier measurement below (a run touching about 45k
   nodes of a 100k-node span) puts the sparse layout within run-to-run
   noise of the dense one, so a second layout and its switch point would
   add a code path without a measured return.

6. **The ceiling is exact accounting checked at run time, not a span
   estimate.** Nothing is allocated before a run's first arrival, so the
   store's span no longer decides any allocation, and the up-front gate
   over `span × states` is removed. Each run instead checks the exact
   heap bytes its tables and rule buffers hold (the slot arrays' and the
   ledger's allocated capacity) against the unchanged 1 GiB
   `FRONTIER_TABLE_BUDGET` at every level boundary, beside the edge-visit
   work budget, with the same overshoot bound: at most one level. The
   accounted set is the tables and the rule buffers alone — the runner's
   endpoint and candidate lists and its per-level counts are bounded by
   the same touched-pair count and stay outside the charge, a conscious
   scope: they are results, not scratch indexed by the span. Beside the
   byte check, the ledger's own index space is a per-arrival backstop:
   the packed head word reserves its top bits for marks, so an arrival
   past the last indexable pair records nothing and folds into the same
   decline — the ceiling is a decline at every granularity, never a
   panic. A crossing is a run-time decline, `FrontierRunDecline::TableBudget`, with
 the same fall-through as [ADR-0516](./0516-frontier-rearrival-budget-fallthrough.md)'s re-arrival-budget decline: the
   run's buffered rows are discarded, and the DFS, with its own caps,
   answers the clause. The PROFILE record names this decline
   `table-budget` (the `FrontierDeclineReason` vocabulary, beside
 [ADR-0516](./0516-frontier-rearrival-budget-fallthrough.md)'s `rearrival-budget`); like that one it is a run-time
   decline, so the declined run's counters stay recorded. The proven
   witnesses' own chain vectors are not
   counted; each is at most the search depth long, and their number is
   bounded by the rule's shared work budget.

7. The executor's node-id-span accessor, which only the dense tables
   read, is removed.

## Alternatives considered

- **Keep the dense layout but allocate it lazily (zero pages).** Calloc
  hands out untouched zero pages cheaply, but `Option<Parent>`'s `None`
  and the verdict entries are not all-zero, so the largest tables would
  still be written in full. The address space would still be span-sized,
  and the span would still decide whether a clause fits. Rejected: it
  hides the span cost instead of removing it.
- **A classic two-array sparse set (dense array plus span-sized `sparse`
  index).** Its O(1) clear is the property wanted, but the `sparse` array
  is span-sized. Its reads of uninitialized memory are not expressible in
  safe Rust, so it would be zero-initialized (or calloc'd) at span size.
  Rejected: the span-sized allocation is the cost being removed. The
  generation-stamped map provides the same constant-time clear without
  it.
- **A general-purpose hash map (the executor's existing Fx-hashed
  `HashMap`) cleared between seeds.** No new code, but `clear()` writes
  one control byte per bucket, so the reset is O(capacity), not O(1). A
  clause whose first seed touches a million nodes and whose next
  thousands of seeds touch a handful each would pay the large reset on
  every seed. A shrink-when-oversized policy (for example, when capacity
  exceeds four times the last run's touched count) trades that for a
  reallocation and adds a tuning rule. Rejected in favor of the
  generation stamp, which needs neither.
- **A pair-major map keyed by (node, state).** One entry per pair means
  more entries and a separate per-node table for the marks. The
  node-major record keeps a node's states in one bitmask word, so the
  BFS's dedup check-and-set and the accept-on-arrival endpoint read are
  one lookup. Rejected.
- **Parents as ledger indices (a pair index plus the step) instead of a
  full `Parent` copy.** This would shrink a ledger entry from 48 to about
  40 bytes. It changes the parent-table readers' shape (the endpoint
  records' predecessor pairs, the chain walks of the halt probe, the
  rule's chain proof and the witness reconstruction), and the
  measurements below leave nothing to recover. Rejected for this change;
  it remains open as a later refinement.
- **The rule's verdict and visit stamp inside the node record (one map
  for every per-node fact).** The rule holds its buffers mutably for the
  whole run while the BFS holds the tables mutably, and the memo is keyed
  by searched targets, a subset of the touched nodes rather than a second
  copy of the touched set. Merging them would make the rule borrow the
  tables mutably inside the BFS's expansion closure. Rejected: borrow
  restructuring for no measured gain.
- **A dense fallback with a crossover** (migrate to a dense `u64`
  state-mask array once touched nodes exceed span/8, hard-capped so the
  dense array stays within a few tens of MB). Measured unnecessary: on
  the large-frontier shape the sparse layout is within run-to-run noise
  of the dense one, so the second layout's code path, its migration and
  its policy point would buy nothing measurable. Rejected; revisit only
  with a measurement showing a material loss.
- **Keep an up-front gate with per-entry costs updated to the sparse
  layout** (worst case: every node of the span touched in every state).
  The sparse per-touched-node cost (slot-load headroom, ledger doubling)
  is higher than the dense per-node cost, so a 4M-span store would
  decline at 1 GiB even when its runs touch two hundred nodes. The gate
  would again be decided by the span, the fact this change removes. A
  live-node-count bound fails the same way on stores whose live nodes
  are mostly outside the pattern. Rejected in favor of exact accounting
  at run time.
- **Node-major expansion with bit-parallel per-label transition rows, a
  small (state-mask, label) → next-mask cache, and multi-source seed
  lanes (up to 64 seeds packed into bit lanes).** These change the BFS
  inner loop or the seed structure, not the table layout. Out of scope:
  each is its own later decision, measured on its own.
- **Pooling one scratch table set across clauses under a memory
  accountant.** Per-clause allocation is now proportional to the touched
  pairs and is a few hundred kilobytes for small answers. Deferred until
  a measurement shows cross-clause reuse matters.

## Consequences

- Per-query working memory follows the answer, not the store. On a
  synthetic store (200 users, 1000 friendship rels, padded with
  unrelated nodes to the given span), two frontier-served var-length
  `RETURN DISTINCT` shapes allocate the same bytes per query at every
  span. Bytes per query, including the rest of the query:

  | span | dense, `*1..3` | sparse, `*1..3` | dense, `*2..2` | sparse, `*2..2` |
  | --- | --- | --- | --- | --- |
  | 1k | 0.87 MB | 0.78 MB | 0.55 MB | 0.45 MB |
  | 100k | 11.4 MB | 0.78 MB | 11.0 MB | 0.45 MB |
  | 1M | 106.8 MB | 0.78 MB | 106.4 MB | 0.45 MB |
  | 4M | 424.8 MB | 0.78 MB | 424.4 MB | 0.45 MB |

  Query time (p10) is flat on the sparse layout, 0.19–0.21 ms (`*1..3`)
  and 0.10–0.14 ms (`*2..2`) from 1k to 4M, where the dense layout grew
  from 0.19 ms to 6.9 ms and from 0.11 ms to 6.7 ms.
- On a 100k-node store (pokec), the frontier-served shapes measured in
  interleaved main/branch/branch/main order (p10): a two-hop shape is
  13% faster (1.08 → 0.94 ms) and allocates 2.1 MB instead of 12.6 MB
  per query; `*1..3` (about 4.8k endpoints) is within noise (+1.6%); the
  large-frontier `*1..4` (44.7k endpoints, 126k edge visits, about 45% of
  the span touched) is within noise (+2.0%, 101.3 → 103.3 ms). Shapes the
  kernel does not serve do not run the changed code.
- A run touching nearly every node of a small span holds more bytes per
  touched node than the dense layout did per node: a 32-byte slot at up
  to 8/3 slots per live entry, plus a 48-byte ledger entry per pair at up
  to twice the live count after a doubling. This is bounded by the run's
  actual reach and checked against the ceiling.
- A recycled table set keeps the capacity of the clause's largest seed
  until the clause ends, never a span-sized one.
- The ceiling is now a run-time decline (`FrontierRunDecline::TableBudget`)
  instead of an engagement-time one. A run whose tables cross 1 GiB
  starts, then falls through with its rows discarded. Under the old gate
  such a clause never started. The answer is the DFS's either way.
- The ledger index is held in 30 bits (the record's head word also
  carries the two marks), asserted with an `expect`. The ceiling keeps
  the ledger orders of magnitude below 2³⁰ entries. That rests on the
  edge-visit work budget as well as the ceiling: the ceiling is checked
  only at level boundaries, and the work budget bounds a single level's
  arrivals (its edge visits times the automaton's states), which is what
  keeps one level's overshoot past the ceiling well under the 2³⁰ index
  space.
- [ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 6's allocation half is superseded in its layout,
  reset and gate. Its composition with the work budget stands: the
  budget bounds edge visits, the ceiling the tables those visits grow.
