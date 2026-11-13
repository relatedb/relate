# 0203. Newest-first expansion order (Neo4j relationship-chain parity)

- **Status**: Proposed
- **Date**: 2026-10-11
- **Deciders**: CypherLite maintainers (LABS-255, LABS-258)
- **Related**: ADR-0031 (typed adjacency), ADR-0033, ADR-0035 (ordered
  expand; tie-break superseded here), ADR-0034 / ADR-0037 (shortest-path
  searches), ADR-0144 (differential suite)

## Context

Two differential-suite failures against the Neo4j reference share one
root cause — the order a node's relationships are walked:

- **LABS-255** (openstreetmap:86):
  `shortestPath((clock)-[:ROUTE*]-(Zoo School))` returns an equal-hop
  (7-hop) path whose relationship `distance`s sum to 306.92 where the
  reference sums to 212.19. The two candidate trails differ in exactly
  one hop: two parallel ROUTE relationships (ids 71087 and 71088, same
  direction, same endpoints). The reference walks the one with the
  **higher id**; cypherlite walked the lower one.
- **LABS-258** (legis-graph:69, twitter-trolls:226): `ORDER BY` ties over
  aggregations keep different tied rows than the reference, and
  `collect()` lists come out in a different order. Aggregation groups
  iterate in first-encountered order and `ORDER BY` is a stable sort on
  both engines, so the observable difference is the row order the MATCH
  produced — which the expansion order drives.

Neo4j's store prepends a new relationship to the head of the node's
relationship chain, so reference expansions see a node's relationships
**newest-first** (descending relationship id within a type). Verified
empirically via LABS-255: with per-chunk adjacency ordered by descending
`RelId`, cypherlite's clock→Zoo School sum is 212.19, matching the
reference exactly. CypherLite's typed adjacency chunks (ADR-0031/0033)
were sorted by ascending `RelId` — the ADR-0035 "then `RelId`"
tie-break — i.e. oldest-first.

## Decision

Within each per-node, per-type adjacency chunk, relationships iterate
**newest-first**: the ADR-0035 chunk order keeps missing-`ts` first and
present `ts` descending, but the final tie-break flips from ascending
`RelId` to **descending `RelId`** (`cmp_ts_desc` in
`cypherlite-executor/src/store.rs`). ADR-0035's "then `RelId`" is read
as descending from here on.

This one order is the whole parity contract for expansion-driven row
order, with one addition for the GPM selector:

- `shortestPath` (ADR-0034 bidirectional BFS, ADR-0037 shallow
  intersection) inherits the tie-break from the walk: the equal-hop
  path reached through the newest relationships is found first and kept
  (both searches record the first meet and never replace it with an
  equal-length one).
- Aggregating projections keep their existing semantics — groups in
  first-encountered order (`IndexMap`), `ORDER BY` a stable sort — and
  now see MATCH rows in the reference's expansion order, so tied rows
  and `collect()` lists line up.
- GPM path selectors (`SHORTEST k` / `ANY SHORTEST` over the walk
  engine) break equal-length, equal-rel-type ties by the walk's
  **node-id sequence** (lexicographic) before falling back to
  enumeration order. Neo4j's stateful shortest path pops its priority
  queue by cost and then by node id, so the selected walk is the one
  with the smallest node-id sequence — not the one the (newest-first)
  expansion enumerated first. Evidence: an @neo4j-verified
  scenario (`SHORTEST 1 WALK` closed walk) returns `[1, 2, 1]`
  although node 4's relationship is the newest; and LABS-255's tied
  trails share one node sequence, so their parallel-relationship choice
  is still the expansion order's (newest rel wins). The node-sequence
  key only reorders members that tie on length AND type signature, so
  enumeration order — newest-first — still decides parallel-relationship
  ties, which share the node sequence.

## Alternatives considered

- **Keep ascending ids; treat tie order as unspecified** — rejected:
  the differential suite (ADR-0144) compares exactly these outputs, and
  the order-only diffs keep recurring across datasets.
- **Re-order only the shortest-path searches** (sort hops per node by
  descending id at expansion time) — rejected: LABS-258 shows the same
  parity gap in ordinary MATCH expansions feeding aggregations; two
  orders in the engine would also make row order depend on which
  operator happened to walk the adjacency.
- **Full chain-order emulation**: interleave outgoing and incoming
  relationships of a node by descending id for `-[:T]-` (direction
  BOTH), as Neo4j's single per-type chain does. Rejected for now:
  cypherlite's chunks are per direction, the repros are all
  single-direction or match without it (LABS-255 verified with
  out-then-in chunks), and the merge adds a per-hop allocation on the
  hottest read path. Recorded as a known divergence for nodes where
  both directions of one type interleave by id.
- **Overlay scratch (uncommitted creates) first, reversed** — the
  strict newest-first order for a write-transaction MATCH. Deferred:
  the overlay scratch is explicitly unordered (ADR-0031/0035), the
  differential repros read committed stores, and no differential
  failure is traced to in-transaction order. A later ADR can extend the
  contract if one appears.

## Consequences

- Row order of every un-`ORDER BY`'d expansion, `collect()` list order,
  shortest-path tie-breaks, and aggregation tie rows change to match
  the reference's newest-first walk. Pinned expectations authored
  against the old walk (e.g. the recommendations-walkthrough rows in
  `cypherlite/tests/keyword_aliases.rs`) are re-pinned.
- The ADR-0035 `ORDER BY ts DESC LIMIT` early-stop walk is unaffected:
  `ts` still leads the chunk order; only the same-`ts` tie-break flips.
- Unit tests pin the semantics: `store::tests::typed_adj_chunks`
  (chunk order), `shortest::tests::pair_search_tie_breaks_to_the_newest_relationship`
  (both pair-search strategies), and the end-to-end
  `cypherlite/tests/labs255_258_expansion_order.rs` (parallel
  relationships, equal-hop paths, expansion row order, aggregation
  ties).
- LABS-255's openstreetmap:86 repro matches the reference exactly
  (212.19). LABS-258's legis-graph:69 / twitter-trolls:226 repros move
  to the newest-first tie order; byte-exact confirmation against the
  reference awaits the next docker-backed differential run (no docker
  on the authoring host).

## Later references

ADR-0035: its `RelId` tie-break sentence is superseded by this ADR.
