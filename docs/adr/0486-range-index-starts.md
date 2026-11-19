---
type: adr
area: engine
claim: d70a258c334a02931e879d6e91694fe6fba37f49
name: 0486-range-index-starts
status: proposed
tags:
  - index
  - planner
title: Range-index seek starts — priced in the ladder, named in the plan tree
REFERENCES:
  - "[[adr/0024-match-rewrite-planner]]"
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0351-cheapest-element-start-for-unbound-patterns]]"
  - "[[adr/0359-single-plan-renderer-at-the-boundary]]"
  - "[[adr/0363-query-notifications]]"
  - "[[adr/0375-profile-walk-start-and-store-read-kinds]]"
  - "[[adr/0474-compound-contains-seed-start-cost]]"
  - "[[adr/0493-range-seek-crossover]]"
---

# ADR-0486: Range-index seek starts — priced in the ladder, named in the plan tree

## Context

RANGE indexes already serve comparison predicates at execution time —
opportunistically and invisibly. The executor's candidate funnel
(`node_candidates` → `leaf_seek` recognizing `Seek::Range`, the index's
`lookup_range_seek` serving it, the residual re-applying the predicate)
seeks a range-indexed `(label, property)` pair whenever a `WHERE` conjunct
on the start element's variable allows it; the plan never knows. Three
surfaces pay for that invisibility:

- **EXPLAIN lies about the executed shape.** The [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) plan tree
  narrates `NodeByLabelScan` + `, WHERE filter` (or the fused `Expand`)
 while the funnel seeks — the same executor-side reorientation [ADR-0218](./0218-stats-based-match-orientation.md)
  rejected for orientation, now visible in the tree.
- **The ladder never sees range selectivity.** [ADR-0218](./0218-stats-based-match-orientation.md)'s one ladder —
 feeding both the planner's pattern orientation and [ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md)'s
  cheapest-start (`GraphStats::estimate`) — prices a seekable range start
  at its whole label count, so it loses anchors and pattern order to
 unseekable sampled filters: the mirror of [ADR-0474](./0474-compound-contains-seed-start-cost.md)'s measured q8
  pathology, where the unseekable side stole the anchor.
- **PROFILE cannot name an index-seek start.** `StartReason`'s closed
  vocabulary has no index-seek reason — `SeekAnchored` is the LABS-161
  relationship-seek anchor — so "did the index engage" stays invisible.

The anchored-equality point read is half-served today: [ADR-0024](./0024-match-rewrite-planner.md)'s fold
puts it in the property map, `equality_rows` prices it, and
`match_estimate`'s `equality_estimate` arm folds the estimate to the hit
count — but the tree still narrates the fused scan. The comparison window
has neither pricing nor a name.

## Decision

Range-index-backed starts become first-class plan facts: priced as one
new level in the one ladder, named as seek operators in the plan tree.
The funnel itself (`node_candidates` → `Seek::Range`) is untouched — it
already serves these predicates; this decision adds pricing
([ADR-0218](./0218-stats-based-match-orientation.md)/[ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md)) and naming ([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)/[ADR-0359](./0359-single-plan-renderer-at-the-boundary.md)) around it.

- **Counting — `PatternStats::range_rows`.** A new trait method beside
  `equality_rows`/`contains_rows` in `cypherlite-planner/src/stats.rs`:
  `range_rows(&self, label: &str, property: &str, op: BinaryOp,
  bound: &Expr) -> Option<u64>`. The four comparison ops only — `Eq`
  stays `equality_rows`' domain; `IN` and `STARTS WITH` are deferred
  (below). The bound resolves via `Expr::seek_value`; the count is the
  range index's posting-length window, taken through a `pub
  Graph::pattern_range_rows` helper beside `pattern_equality_rows`
  (`cypherlite-executor`'s `graph/index.rs`) that delegates to the same
  `range_seek_cardinality` the executor's `Seek::cardinality` reads —
  one count, both consumers. The method ships with a default `None` body
  in the trait (the `degree_tail` precedent: the trait is public, and a
  required method would break external implementors). `None` means: no
  RANGE index on the `(label, property)` pair, an unresolvable bound, no
  order key on the bound, or unbounded temporal postings.

- **Ladder rank — between equality and CONTAINS.** In
  `endpoint_estimate`'s `or_else` chain
  (`cypherlite-planner/src/order/mod.rs`): bound variable (`Measured(1)`)
  > equality point read > range window > CONTAINS > label rows > rank
  sentinels. Consumed as `Estimate::Measured` — an exact index count
  paying no scan floor, unlike the sampled CONTAINS'
 `max(scan, rows × degree)` compound ([ADR-0474](./0474-compound-contains-seed-start-cost.md)) — taking the min across
  the start element's servable comparison conjuncts (the
  `equality_hits`/`contains_hits` min policy) via a `range_hits`
  extractor mirroring `contains_hits`. Equality stays above: a point
  read is the degenerate tightest window on the same index. Range sits
  above CONTAINS: an exact seekable count outranks a sampled
  extrapolation carrying a scan floor — the seekable side must win the
 anchor (the mirror of [ADR-0474](./0474-compound-contains-seed-start-cost.md)'s q8 pathology). [ADR-0218](./0218-stats-based-match-orientation.md)'s one ladder
 feeds both the planner's orientation and [ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md)'s cheapest-start
  (`GraphStats::estimate`), so the level lands in plan order and runtime
  start selection together.

- **Predicate set — the v1 floor is `<`, `<=`, `>`, `>=`** on a
  range-indexed `(label, property)` pair of the start element: a
  residual `WHERE` conjunct on its variable, operand-oriented the way
  `range_seek_expr` orients (property on either side, op mirrored).
  `IN` is deferred: it is the multi-value form of the anchored point
  read, its Neo4j plan narration is version-varying (`NodeIndexSeek`
  over a list vs `MultiNodeIndexSeek`) and it needs its own probe and
  its own count semantics (a sum over list values); the funnel already
  serves it today (`RangeSeek::In`), so deferral costs pricing and plan
  visibility only, no execution behavior. `STARTS WITH` is deferred: a
  prefix window over the range index's string order with its own count
  machinery (`order::prefix_ids` has no cardinality twin —
  `Seek::cardinality` returns `None` for text seeks) and a different
  details spelling; also already funnel-served today.

- **Plan visibility — named operators at tree-build time**, on the
 EXPLAIN/PROFILE-only path ([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) decision 3), extending the
 [ADR-0024](./0024-match-rewrite-planner.md) fold/orient → [ADR-0218](./0218-stats-based-match-orientation.md) tie-break → [ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md) cheapest-start
  chain. A single-pattern MATCH (the `[single]` shape gate
  `match_estimate` already runs) whose start element — element 0 of the
  post-rewrite pattern, the element the rewrite chose — passes
  `single_static_label` with an index-servable predicate replaces the
  fused `NodeByLabelScan`/`AllNodesScan`/`Expand` narration: the
  operator becomes the seek's name, the details become
  `RANGE INDEX <var>:<Label>(<prop>) WHERE <prop> <op> <bound>` (bound
  as spelled: `$param` or the literal's text — this engine has no
  auto-parameterization; no `, cache[...]` suffix — no property cache,
 [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)'s emit-nothing rule). A multi-element pattern keeps its
  pattern text after the seek line (the hops the fused operator still
  walks); the `, optional`, `, SEARCH`, `, WHERE filter` suffixes stay
  as today. The anchored-equality point read — the folded property map
 of [ADR-0024](./0024-match-rewrite-planner.md)'s fold, the `equality_estimate` arm that already folds
  the estimate to the hit count — renders `NodeIndexSeek` with the
  min-count pair's details (`equality_estimate`'s min policy); when an
  indexed equality and an indexed comparison both sit on the start
  element, the equality names the operator (it outranks in the ladder).
  Everything else — multi-pattern clauses, dynamic label expressions,
  unindexed pairs, declined counts — keeps today's fused narration.
  EXPLAIN states the planned start; a runtime pivot or cheapest
  displacement (fed by the same new level through `GraphStats::estimate`)
  stays PROFILE's `WalkStart` fact, the existing `[start #k, …]` details
 suffix disambiguating [ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md)). The comparison-conjunct
  recognizer is spelled once in the planner's order module and consumed
  by the tree-build — the planner/executor twin of `contains_prop` vs
 `string_seek_expr`, the duplication [ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md) tracks.

- **Operator names — `NodeIndexSeek` and `NodeIndexSeekByRange`**: the
  Neo4j vocabulary the 2026.09.0/2026.01.4 Docker probe observed (over
 the HTTP Query API v2) and the names [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)'s own vocabulary list
  already anticipated. `NodeIndexSeek` names the anchored-equality point
  read; `NodeIndexSeekByRange` names the v1 comparison window. The HTTP
  `operatorType` `@neo4j` suffix is the database name, not vocabulary:
 the engine tree carries the bare name and [ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)'s surfaces keep
 their existing decoration. [ADR-0359](./0359-single-plan-renderer-at-the-boundary.md)'s one boundary renderer needs no
  layout change — only new operator names and details values — and the
  cli/bolt/http spec operator tables document the two names in the same
 change ([ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) alignment). No `Unique` arm: no index in this engine
  backs a uniqueness constraint.

- **`estimated_rows` — the seek's own count**: `equality_rows` for the
  point read (today's `match_estimate` fold, unchanged) and `range_rows`
 for the window, under [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) phase-3 semantics — a pre-execution,
  statistics-derived hint, never a measurement — as the Neo4j probe
  confirmed: the posting-length count is an upper bound, not
  snapshot-filtered (Neo4j's own probe estimate ran 2.97 against 49
  actual, and estimates are statistics-dependent). PROFILE keeps actual
  `Rows` beside it and the 10x `EstimationMiss` policy is unchanged. A
  declined count (`None`) leaves both the label-rows estimate fallback
  and the fused operator name — estimate and narration gate together on
  the same `Some`/`None`.

- **`MissingIndex` gating — unchanged**, [ADR-0363](./0363-query-notifications.md)'s equality-only
  domain. The notification still tests the folded property-map pairs of
  every node payload against `range_indexed_properties`
  (`plan_node/notifications.rs`): an index-backed start never fires one
  for its own pair (a served seek implies the index exists); an
  unindexed anchored equality still fires it beside the fused scan
  narration exactly as today. v1 comparison predicates do not enter the
  domain: an unindexed comparison keeps today's silence — its diagnosis
  surface is the fused scan narration plus the label-rows estimate plus
  PROFILE's overshoot warning. Extending the notification's predicate
  domain would be a client-visible contract change beyond this
  feature's bar.

- **Fallbacks — the plan never claims a seek the funnel would
  decline.** No-stats (`NoPatternStats`): `range_rows` answers `None`
  via its default body, the ladder skips the level, the tree keeps the
  fused narration — stats-free plans are bit-identical to today.
  Declined seek (unbounded temporals): `range_rows` returns `None`
  exactly where the executor's index declines — the bound has no order
  key, or the pair's range index carries unbounded temporal postings (a
  stored temporal outside the order-key window is unposted, so the
  index cannot answer the comparison; the one decline
  `lookup_range_seek` and `range_seek_cardinality` already share) — and
  the plan falls back the same way: the ladder prices
  contains/label/rank sentinels, the tree keeps the fused
  `NodeByLabelScan`/`Expand` narration, and the runtime funnel keeps
  its scan-plus-residual answer (correct, unpriced, unnamed). The
  estimate's decline conditions are the executor's, spelled once by the
  index layer.

## Alternatives considered

- **Status quo: executor-side opportunistic seeking** — the
  `node_candidates` funnel (`leaf_seek` recognizing `Seek::Range`,
  `lookup_range` serving it, the residual re-applying the predicate),
  invisible to the plan tree. Rejected: EXPLAIN lies about the executed
  shape — the tree narrates `NodeByLabelScan` + `, WHERE filter` while
 the funnel seeks, the same reason [ADR-0218](./0218-stats-based-match-orientation.md) rejected executor-side
 reorientation; the [ADR-0218](./0218-stats-based-match-orientation.md)/[ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md) ladder never sees range
  selectivity, so a seekable range start prices at its whole label count
  and loses anchors and pattern order to unseekable sampled filters
 (the mirror of [ADR-0474](./0474-compound-contains-seed-start-cost.md)'s measured q8 pathology); and PROFILE's closed
  `StartReason` vocabulary cannot name an index-seek start —
  `SeekAnchored` is the LABS-161 relationship-seek anchor — so "did the
  index engage" stays invisible.

- **Model the fact via the LABS-369 `WalkStart` field instead of a
  named operator** — extend `StartReason` with an index-seek reason (or
  ride the sought pair on the per-clause metric record), leaving the
  tree's operator names unchanged. Rejected: `WalkStart` is
 PROFILE-only — recorded per walk at the metric tick ([ADR-0375](./0375-profile-walk-start-and-store-read-kinds.md)) while
 EXPLAIN stamps no metrics ([ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) decision 2) — so EXPLAIN would
  keep narrating a label scan for an indexed point read on the very
 surface client tooling renders ([ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)/[ADR-0359](./0359-single-plan-renderer-at-the-boundary.md)); `estimated_rows` and
  the `MissingIndex` pairing are tree-build facts a metric record
  cannot carry; and it adds a second vocabulary for one fact beside the
  operator names the tree already carries — the parallel-state shape
  the code-shape rules forbid.

- **Split the tree per physical operator** — a real `NodeIndexSeek`
  child feeding an `Expand` parent (LABS-343's per-operator tree
 split). Rejected: [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md) phase 3 made the tree clause-granularity
  by decision, and the split is its own tracked later phase; the
  clause-operator rename plus details replacement makes the tree honest
  with no structural change, keeping v1 inside the phased plan.

## Consequences

- EXPLAIN and PROFILE name index-backed starts (`NodeIndexSeek`,
  `NodeIndexSeekByRange`) with the serving index in the details, and
  the one ladder prices the range window — so a seekable start beats
  unseekable sampled filters for the anchor in both the planner's
  pattern order and the executor's runtime start selection; they read
  the same level and cannot disagree.
- The count is spelled once (`range_seek_cardinality`, behind
  `Graph::pattern_range_rows`): the plan's estimate and the executor's
  seek cardinality cannot drift, and the plan's decline conditions are
  exactly the executor's — the plan never narrates a seek the funnel
  would not serve.
- `PatternStats` is a public trait: `range_rows` ships defaulted to
  `None` (the `degree_tail` precedent), so external implementors keep
  compiling; an implementor that does not override it leaves the level
  silent and plans fall back to today's pricing and narration.
- The estimate is a statistics-derived upper bound (the posting-length
  window, not snapshot-filtered rows): PROFILE can legitimately fire
  the 10x `EstimationMiss` warning on a heavily filtered window — the
  same semantics the Neo4j probe showed (2.97 estimated vs 49 actual).
- `IN` and `STARTS WITH` stay unpriced and unnamed (deferred with the
 reasons above), and `MissingIndex` stays equality-only ([ADR-0363](./0363-query-notifications.md)): an
  unindexed comparison keeps today's silence — its diagnosis surface is
  the fused scan narration, the label-rows estimate, and PROFILE's
  overshoot warning.
- The planner-side comparison-conjunct recognizer joins the
 planner/executor twins [ADR-0351](./0351-cheapest-element-start-for-unbound-patterns.md) tracks (`contains_prop`
  vs `string_seek_expr`); the tree-build consumes the planner's one
  spelling, never a second.
- The operator vocabulary is client-visible: the cli/bolt/http spec
  operator tables document the two names in the same change as the
 code ([ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) alignment), and [ADR-0359](./0359-single-plan-renderer-at-the-boundary.md)'s one boundary renderer
  renders them with no layout change.
- Pinned by tests at the levels the decision touches: ladder levels in
  `cypherlite-planner/src/order/tests.rs` (stub stats) and
  `cypherlite-executor/src/exec/graph_stats_tests.rs` (toy graphs);
  narration in `cypherlite/cypherlite/tests/explain_profile.rs` and
  `cypherlite/cypherlite/tests/notifications.rs`, plus the
  `docs/api/{cli,bolt,http}/spec.md` operator tables.

## Later references

- **[ADR-0493](./0493-range-seek-crossover.md)** adds the selectivity crossover this decision lacked: a
  range window at or above one quarter of its label's rows (above a
  small absolute floor) declines the seek — both the ladder's range
  level here and the executor funnel's `Seek::Range` serve consult the
  one policy. The ranking below stands unchanged for windows under the
  crossover.
