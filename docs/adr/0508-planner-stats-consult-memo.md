---
type: adr
area: engine
claim: 5f812fa4c9ee09c4dc34c2148bf03ad6c7a6f091
name: 0508-planner-stats-consult-memo
status: accepted
status_note: |-
  user ruling 2026-11-05: "invalidation shouldn't be necessary as we have MVCC — as long as we hold a version the memo is also valid for all the transactions" (point 3 confirmed on the project's MVCC/publication-epoch grounds); points 1, 2 and 4 proceed as proposed under the 2026-11-04 "go with PR3" direction. The plan-cache analogue — when cached plans become invalid across versions — is a separate decision (PR 4, the [ADR-0503](./0503-template-keyed-plan-cache.md) cache), with strategies to be researched and tested.
tags:
  - perf
  - planner
title: Per-statement memo for the planner's value-keyed stats consults
REFERENCES:
  - "[[adr/0137-wal-follow]]"
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0350-bound-node-pivot-for-pattern-expansion]]"
  - "[[adr/0351-cheapest-element-start-for-unbound-patterns]]"
  - "[[adr/0474-compound-contains-seed-start-cost]]"
  - "[[adr/0486-range-index-starts]]"
  - "[[adr/0503-template-keyed-plan-cache]]"
---

# ADR-0508: Per-statement memo for the planner's value-keyed stats consults

## Context

The PLAN-phase profile (`research_notes/planner-profiling-2026-11-04.md`,
30 GBL SF0.1 queries, EXPLAIN loop, macOS `sample` at 1 ms) puts
`planner::plan::plan_with_env` at 30% of statement CPU. Inside it,
`rewrite::rewrite_part` is 65% and the `order::order_patterns` closure
39%, driven by `direction_cost`. That time goes almost entirely to stats
consults through the [ADR-0218](./0218-stats-based-match-orientation.md) pattern-ordering ladder and the
[ADR-0350](./0350-bound-node-pivot-for-pattern-expansion.md)/[ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md) start-selection estimates: `PatternStats::contains_rows`
(192 samples) and `best_equality` → `equality_rows` (77). `bind` is
invisible (2 samples), and the planner's own data structures show no
hashing hotspot. The cost is consults, not bookkeeping.

The same consult repeats within one statement for three reasons visible
in `cypherlite-planner/src/order/mod.rs`:

- `order_patterns` is a greedy loop. Each iteration re-runs
  `choose_orientation` over every pending pattern, so a k-pattern MATCH
  prices about k(k+1)/2 orientations, not k.
- `choose_orientation` calls `direction_cost` for both orientations.
  Each call runs `endpoint_estimate` (`best_seek` → `best_equality` /
  `best_range`, then `contains_hits`, then `label_hits`) and a
  degree walk (`apply_degree` → `label_rows`, `rel_type_rows`,
  `degree_tail`). The same `(label, property, value)` question gets asked
  again on every repricing.
- After admission, the plan-tree render (EXPLAIN, PROFILE,
  `plan_body`) builds a second `GraphPatternStats` and asks the same
  questions again. That cost is outside this ADR's scope (see
  Consequences).

A single consult is not cheap. Every planner consult goes through
`GraphPatternStats` (`cypherlite/src/stats.rs`) to the executor's
`GraphStats` (`cypherlite-executor/src/exec/graph_stats.rs`):

- `equality_rows` resolves the bound with `Expr::seek_value` (a `Value`
  clone), then `Graph::pattern_equality_rows` → `range_equality_ids`
  (`graph/declared/mod.rs`). That function drains the pair and builds a
  whole `HashSet<NodeId>` only to take its `len()`.
- `contains_rows` first tries `lookup_text_contains`. That allocates the
  label `String`, computes the needle's grams, and, on a TEXT-indexed
  pair, builds the full candidate `Vec<NodeId>` to count it. Without a
  TEXT index it goes to the sampled estimate
  (`sampled_contains_rows`), which takes the `contains_memo` mutex and,
  on an epoch miss, scans up to `CONTAINS_SAMPLE_BOUND` (1024) nodes.
- `range_rows` ([ADR-0486](./0486-range-index-starts.md)) clones the bound and builds a `RangeSeek`
  before reading the posting window.

The store already has one memo here, and it shows the invalidation
mechanism that actually exists. `Graph::contains_memo`
(`graph/index.rs`, `ContainsMemo`) caches the sampled `CONTAINS`
estimate per **committed epoch**. Its key is `ts`, which is
`Graph::commit_ts()`, the `published_ts` atomic set by `set_commit_ts`
when a commit publishes (`commit.rs` `flush_group` →
`begin_publication`, under the publication write lock). A moved epoch
clears the map on the next store. The map is capped at
`CONTAINS_MEMO_CAP` (64) because a query supplies its keys. There is
**no catalog version**. `cypherlite-catalog` has no version, epoch or
generation counter, and no code path bumps a counter on DDL alone. The
statistics the ladder reads (`label_count`, the RANGE/TEXT postings,
`pattern_rel_type_rows`, the degree histograms) are maintained live by
every data commit. There is no separate "statistics refresh" event. The
other way a graph's numbers change is a `--follow` reload replacing the
graph behind `GraphCell` ([ADR-0137](./0137-wal-follow.md)). That replacement drops any memo the
old `Graph` owned.

The sampled-CONTAINS selectivity fix is the warning case for any memo
here. Without a TEXT index,
q19's `CONTAINS` selectivity was invisible, so the planner priced Tag at
its 16k label rows and produced a ~29x plan gap. The fix made the
estimate measurable and typed its provenance (`ContainsCount::TextIndex`
vs `SampleScan { rows, scan }`, with the [ADR-0474](./0474-compound-contains-seed-start-cost.md) compound scan floor).
A memo that outlives the data or index state it measured would
reintroduce that failure class: an estimate frozen while the store moved
on, for example a TEXT index created after the memo entry and never
consulted.

The template-keyed plan cache ([ADR-0503](./0503-template-keyed-plan-cache.md)) skips parse, lower and
plan entirely on a same-shape hit. A memo only helps statements that
miss the cache. The research note therefore ranks planner-side speedups
below the cache, and this design must compose with it, not compete.

## Decision (proposed — pending user ruling)

1. **Memo key.** Each memo entry is one consult: the method (a typed
   enum tag, not a string), `label`, `property`, the comparison
   `BinaryOp` for `range_rows`, and the *seek operand identity*.
   `PatternStats` implementors only resolve two operand forms (they
   answer `None` for anything else via `Expr::seek_value`): a literal,
   keyed by its literal value, and a parameter, keyed by its parameter
   name. A parameter name is a sound key because the memo never outlives
   one plan call, and within one plan call the statement's parameter map
   is fixed. Operands of any other shape bypass the memo. Their consult
   answers `None` cheaply anyway. Store state (`commit_ts`, index
   existence, label counts, histograms) is **not** part of the key,
   because the placement (2) pins it for the memo's whole lifetime. The
   memo stores the consult's full typed answer, `Option<u64>` or
   `Option<ContainsCount>` with its provenance, and never collapses a
   `SampleScan` to its rows. Per-statement consult counts are small, so
   storage is a linear-scan `Vec` per method, the same reasoning as the
   executor's `NeedleEstimates`. That avoids hashing an `Expr` or a
   float-bearing literal.

2. **Placement.** A planner-local, per-statement memo. It is one wrapper
   type in `cypherlite-planner/src/stats.rs`, beside the `PatternStats`
   trait, that implements `PatternStats` over an inner
   `&dyn PatternStats`. It is built once at the top-level planning entry
   (`plan_with_stats`), threaded through `plan_with_env` and its `NEXT`
   recursion, and dropped when planning returns. Every `PatternStats`
   implementation benefits, including the test fakes and
   `NoPatternStats`. The wrapper has interior mutability through a
   `RefCell`, because the trait takes `&self` and the planner is
   single-threaded per statement. The planner stays pure: no store type
 enters the planner crate, and the dependency direction of [ADR-0218](./0218-stats-based-match-orientation.md) is
   unchanged.

3. **Invalidation trigger.** None needed, by construction. The memo
   lives only for one `plan_with_stats` call. The admission path
   (`gate.rs` `plan_gate_classify`) holds the store's publication **read
   guard** for that whole call, and `published_ts` only advances under
   the publication **write** lock (`begin_publication`). So no commit,
   DDL, index create/drop, or data write can publish while a memo entry
   exists. A `--follow` reload (`GraphCell` replace) is also locked out
   for the same reason. The existing per-epoch `ContainsMemo` keeps its
   role across statements and is unchanged. If a cross-statement layer
   is ever added (see Alternatives), its version must be
   `(graph identity, commit_ts)`, the mechanism that exists today. It
   must never be a DDL-only counter, because data commits move every
   number the ladder reads.

4. **Scope.** The three value-keyed consults: `equality_rows`,
   `range_rows` and `contains_rows`. These are the profiled hotspots
   (`contains_rows` 192 samples, `equality_rows` 77), and each one
   resolves a `Value` and materializes or allocates per call.
   `label_rows`, `rel_type_rows` and `degree_tail` are O(1) counter or
   histogram reads keyed only by interned names. A memo lookup would
   cost about as much as the answer, so the wrapper forwards them
   unmemoized. The forwarding is explicit per method, so the split is
   stated once, in the wrapper, and is not a second policy elsewhere.
   The memo-code node's benchmark may extend scope to the counter
   methods only if the plan-bench harness shows a measurable win.

The memo must be transparent. For every statement, the memoized planner
must produce a plan identical to the unmemoized one. The code node's
acceptance therefore includes three checks. First, a counting fake
`PatternStats` test proves each distinct consult reaches the inner stats
at most once per statement, and that ordering output is unchanged across
the planner's existing order tests. Second, EXPLAIN output is
byte-identical across the 30-query GBL suite. Third, the plan-bench
harness shows a reduction in plan-phase time.

## Alternatives considered

- **No memo.** Keep consulting the store on every repricing. This is the
  zero-risk baseline and stays correct. Rejected because the measured
  cost is real: about 39% of planner time sits in `order_patterns`
 consults, and that is the cost plan-cache ([ADR-0503](./0503-template-keyed-plan-cache.md)) misses pay (first
  sightings, non-parameterizable shapes, every statement while the
  cache is cold or disabled).

- **Make each consult cheap instead of memoizing.** For example, count
  RANGE equality postings without building the `HashSet<NodeId>`, count
  TEXT candidates without the `Vec`, and skip the label `String`
  allocation. This is worth doing and complementary, but it is not a
  substitute. It does not remove the O(k²) repricing repeats of the
  greedy loop, and the sampled `CONTAINS` path still pays grams, the
  mutex and the first-sight scan. It also touches the executor index
  layer's exactness contract (the equality set de-duplicates across
  postings), which is its own decision. Rejected as the answer to this
  ADR. It is recommended as a separate follow-up ticket.

- **Per-statement memo inside the `PatternStats` implementations** (in
  `GraphPatternStats` / `GraphStats`). Rejected for two reasons. First,
  the executor's `GraphStats` also serves per-row start selection
  (`exec/pivot.rs`, `exec/kernels`), so a memo there would change
  executor behavior and need its own lifetime story. Second, putting it
  in the top-level crate's `GraphPatternStats` would leave every other
  implementation unmemoized and split one policy across implementations.

- **Full cross-statement cache, versioned by a catalog/DDL counter.**
  Rejected. No such counter exists, and building one would be
  *incorrect* here: label counts, posting lengths and degree histograms
  move on every data commit, so a DDL-scoped cache would freeze stale
  estimates. That is the stale-estimate failure class.

- **Full cross-statement cache versioned by `(graph identity,
  commit_ts)`.** This would be correct, and it is the `ContainsMemo`
  precedent. Rejected for now for three reasons. Any write moves the
  epoch, so the cache wins only on read-mostly workloads, and those are
 exactly the workloads the [ADR-0503](./0503-template-keyed-plan-cache.md) plan cache serves better by skipping
  planning entirely. Its keys must be *resolved* parameter values, not
  names, so it becomes a query-controlled map needing a byte or entry cap
  and shared-state locking on the plan path. It also adds an invalidation
  surface that the per-statement memo avoids by construction. It can be
 revisited if measurements after the [ADR-0503](./0503-template-keyed-plan-cache.md) cache lands show cache
misses on
  read-mostly stores still dominated by consults.

- **Precompute all stats at DDL time.** Rejected. The value-keyed
  consults depend on the statement's own literal or parameter (the
  equality value, the range bound, the `CONTAINS` needle), so the answer
  space cannot be enumerated at DDL time. Data commits would invalidate
  any precomputation anyway. The value-independent counters
  (`label_rows`, `rel_type_rows`, `degree_tail`) are already maintained
  live and are O(1).

## Consequences

- **This design is a proposal.** Following the project process for
  agent-authored ADRs (2026-11-04), it goes to the user for a ruling on
  all four points (memo key, placement, invalidation, scope) **before
  the memoization code node dispatches**. Status stays `Proposed` until
  then.
- If accepted, the expected win is bounded. At most it is the share of
  `order_patterns` consults that repeat: up to ~39% of planner time,
  ~12% of statement CPU, and only on multi-pattern MATCH parts or parts
  that re-price an endpoint. Single-pattern point queries gain little.
 The plan cache ([ADR-0503](./0503-template-keyed-plan-cache.md)) remains the larger lever, and the memo helps only
its misses.
- No new invalidation machinery, no new shared state, and no
  store-version coupling in the planner. The planner crate gains one
  wrapper type. The `PatternStats` trait contract is unchanged.
- No estimate can outlive the publication epoch it was read at, so
 The sampled counts' provenance typing and the [ADR-0474](./0474-compound-contains-seed-start-cost.md) scan floor still
reach
  the ladder unchanged.
- Out of scope, possible follow-ups: (a) sharing the admission's memo
  with the plan-tree render (EXPLAIN/PROFILE/`plan_body`), which
  currently re-consults under its own read guard and possibly a later
  epoch, so sharing needs an epoch check; (b) the cheaper-consult work
  listed under Alternatives; (c) the executor's per-row
  `GraphStats::estimate` consults.
- Related: [ADR-0218](./0218-stats-based-match-orientation.md) (stats-based orientation, the ladder), [ADR-0350](./0350-bound-node-pivot-for-pattern-expansion.md) /
 [ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md) (bound-node pivot and cheapest-element start, the estimate's
 executor consumers), [ADR-0474](./0474-compound-contains-seed-start-cost.md) (compound `CONTAINS` seed cost),
 [ADR-0486](./0486-range-index-starts.md) (range windows), [ADR-0137](./0137-wal-follow.md) (`GraphCell` replace under
  `--follow`), research note `research_notes/planner-profiling-2026-11-04.md`,
 [ADR-0503](./0503-template-keyed-plan-cache.md) (template-keyed plan cache).
