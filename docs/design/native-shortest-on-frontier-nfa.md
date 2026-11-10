# Shortest selectors on the frontier NFA — design note

Status: research note, not an ADR. No decision is taken; the route does not
change. Date: 2026-11-10. Question: can the frontier NFA / QPP engine
(ADR-0478, ADR-0536, re-arrival kernel ADR-0516) answer shortest selectors
natively instead of the separate BFS in `relate-executor/src/shortest/` and
`exec/gpm/shortest_route/` (ADR-0186, ADR-0189, ADR-0556)?

**Answer in one line.** Yes for shapes the route does not take today
(multi-rel patterns, quantified bodies), forward-only first; no for the one
var-length rel the route already serves, where the NFA is the same BFS with
heavier bookkeeping. Bidirectional is feasible but gated on guard-free NFAs
and on LABS-1285. No spike was built: the selector has no entry into the
frontier rung today, so a before/after would measure plumbing, not the
search (see section 6).

## 1. What exists

- The route admits exactly `node - one var-length rel - node` with
  `min <= 1`, a followable rel and static endpoint labels
  (`exec/gpm/shortest_route/guard.rs`, `routed_hop`, `route_shape`). Any
  other selector shape (a QPP with two rels, `(a)-[:R]->()-[:S*]->(b)`)
  falls to enumerate-then-select.
- The matcher declines every selector clause for the frontier rung
  (`exec/kernels/matcher.rs`, the `needs_gpm()` test), and
  `frontier_match.rs` names selectors as a decline.
- The frontier run is a level-synchronous BFS over deduplicated
  (node, state) pairs (`kernels/frontier.rs`, `frontier_tables.rs`). The
  first arrival of each pair is recorded with its predecessor pair and rel
  (`Parent`, `Step`). `frontier_emit.rs::chain_of` rebuilds a seed-first
  chain from that ledger for one endpoint. The first accepting arrival of a
  node is at the shortest accepting walk length for that node. That is the
  selector's definition.
- `run_with_halt` takes a halt probe at every level boundary, which is the
  shape ADR-0555 wants for a halted consumer.

## 2. Reversed transitions and a meet check

Reversing is mechanical on the compiled `FrontierNfa`: flip each
`NfaTransition.direction`, swap `from`/`to`, make the accepting set the
start set and the start state the accepting set. Two real costs:

- **Multi-seed.** `accepting[]` is an epsilon-closed set, so the backward
  run starts from several states at the end node. The seed pass
  (`frontier_run.rs::seeds`, `close_epsilons`) already seeds a start pair
  plus its epsilon closure; it generalizes to a state set, and the
  `u64` mask per node (`STATE_CEILING` 64) is unchanged. The reverse
  epsilon edges must be closed on the backward side too.
- **Guards and predicates do not reverse.** `state_guards` are arrival
  checks (reversible: they are per node and state), but `predicates`
  (the forced rung's class F, group `WHERE` over variables of earlier
  states) need the forward path. Reverse is therefore admitted only for
  NFAs with empty `predicates`, which is what the auto rung compiles.

Meet check: a meet is a pair `(v, s)` the forward run reached at depth `df`
and the backward run reached at depth `db`, with the same state identity
(the reverse NFA keeps state ids), giving a walk of `df + db`. The existing
stop rule in `shortest/pair.rs::bidir` carries over: expand the side with
the smaller layer, stop once `df + db + 1 >= best`. Bounds: a bounded
quantifier is unrolled into states, so its count lives in the state and
needs no extra level bookkeeping; an unbounded `+` carries `min_hops`
separately, so keep the route's `min <= 1` precondition.

The real difference from `bidir`: that function's correctness argument is
"the two tree halves are node-disjoint, so the meet is a trail". In the
product graph the halves are pair-disjoint, not node-disjoint. A node can
be visited in two states and a rel reused (undirected, same type in two
body positions). So a meet is a candidate walk, and the existing
re-arrival rule's proofs (node-simple representative chain, else the
bounded trail-existence search, `rearival/mod.rs`) must run on the
assembled chain. WALK mode (`Pool::Walk`) skips that.

## 3. Path recovery without per-node rows

The ledger already is the predecessor table, keyed by (node, state) and
sized by pairs touched, not by store span, and bounded by
`FRONTIER_TABLE_BUDGET` with a decline to the DFS. `chain_of` reads it per
endpoint, so no per-reached-node row is built. Gaps:

- **ALL SHORTEST** needs every same-depth predecessor. The ledger keeps one
  first parent. Add a side column for later same-depth arrivals (the
  `Dag.more` shape in `shortest/layered.rs`), allocated only for `all`, and
  charged to `heap_bytes` so the table budget sees it.
- **Streaming.** `layered::dag_layers` already emits each finished layer's
  trails through `on_layer`. The frontier equivalent is the level boundary
  hook of `run_with_halt`: a node's predecessors are final when its level
  completes. Each emitted row is built from `chain_of` and pushed into the
  `RouteSink`. Check before building: the frontier rung today buffers rows
  until the run ends ("its buffered rows are discarded" on a table-budget
  decline), which ADR-0555 forbids for an operator that streams. A
  selector rung must stream per level and, on a table-budget decline mid
  stream, cannot silently restart (rows already emitted). It needs a typed
  error or a pre-flight bound, and that is a design question.
- The pair search needs one chain, assembled from forward ledger to the
  meet then backward ledger reversed, as `pair.rs::meet_trail` does.
- The `all` closed-trail case (`Search::closed`) is not an NFA run and
  stays on the BFS.

## 4. Composition with LABS-919 and LABS-920

- **LABS-919** (shortest as physical operators of the GPM evaluator): the
  frontier engine is a second implementation behind the same operator. The
  guard's `Shape` would carry either the simple var-length hop or a
  compiled NFA, and both feed the one `RouteSink`. The operator choice is
  a cost rule: the single-rel shape stays on `shortest/` (ADR-0556 measured
  that dense arrays regressed on this memory-bound search, and a 32-byte
  map slot per pair is the same trade); the NFA serves shapes the guard
  rejects today. This fits Decision 1 without changing it.
- **LABS-920** (expression form lowers to pattern + selector): reachability
  of the NFA from `shortestPath(...)` comes free once the expression lowers
  to a `PathPattern` plus selector. The NFA must not be the reason to
  delay or reorder that work. The matcher's `is_shortest_path_function`
  decline would become a selector-aware entry.

## 5. Tie-break parity (LABS-1285 exists)

LABS-1285 records that `pair::shallow` keeps the oldest parallel rel
because its `.collect()` keeps the last entry of a newest-first list,
against ADR-0203's newest-first order. Parity facts:

- The frontier ledger keeps the first arrival in expansion order. With
  adjacency order newest-first and a queue in discovery order, forward-only
  frontier matches `layered::dag` first-predecessor semantics for a single
  transition, up to the arm iteration order inside a state (transition
  order, then hops), which would need a pinned test.
- A bidirectional frontier picks a different witness than `bidir` or
  `shallow`, since the meet depends on the expanded side. No tie-break is
  bit-identical across these, so "which shortest path" must be fixed by the
  LABS-1285 decision first, or ANY SHORTEST stays "any" and only the
  length and the ALL SHORTEST set are contractual. Do not build the
  bidirectional frontier before that ruling.

## 6. Spike and measurement plan

Not built. Reaching the frontier rung needs the matcher's selector decline
lifted, a selector-aware contract, and a streaming emission, which is the
feature, not a spike. A cheap proxy would count edge visits for a forward
run against `bidir` on the same pair, but it would mislead: the interesting
number is wall time on cache-cold neighbourhoods, which ADR-0556 shows is
decided by memory behaviour. Plan if taken up: (1) lift decline behind a
preview flag for forward-only, any-shortest, one-state NFAs; verify rows
equal the route on the TCK shortest fixtures; (2) run
`cargo run --release -p relate-query-latency --bin pokec_profile -- small`
(synthetic data only) for the shortest_path group and q11, before and after;
(3) only then multi-rel shapes. Any real-data claim needs real data.

## 7. Recommendation

- **No-go** on replacing `shortest/`: for one var-length rel the NFA is the
  same level BFS with a larger per-pair footprint.
- **Go, narrowly**: native selectors for the shapes the route rejects
  (multi-rel, QPP bodies), forward-only, guard-free NFAs, streaming per
  level; bidirectional as a follow-up after LABS-1285 and a proof that the
  product-graph meet passes the trail check.
- If that route is taken, it is an ADR (new operator selection and a
  streaming contract for the frontier rung); this note is not one.

## Open questions

1. Intended tie-break (LABS-1285): newest-first, or "any" for ANY SHORTEST?
2. Mid-stream table-budget behaviour for a streaming selector rung.
3. Is there a real workload with multi-rel or QPP shortest patterns? The
   evidence for this extension is structural, not measured.
