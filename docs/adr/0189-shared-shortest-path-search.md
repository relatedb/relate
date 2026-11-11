# 0189. One shared shortest-path search for the expression and clause forms

- **Status**: Accepted
- **Date**: 2026-10-08
- **Deciders**: CypherLite maintainers (end-of-feature review of Linear LABS-192 and LABS-196)

## Context

ADR-0186 moved the `MATCH`-clause shortest paths and the simple GQL
shortest selectors onto BFS. The expression form (`eval_shortest`,
ADR-0025, ADR-0034, ADR-0037) kept its own searches. An end-of-feature
review found two search engines for one problem:

- **Expression form** (`cypherlite-executor/src/eval/shortest.rs`):
  - `bfs_shortest`: an inline single-source BFS with one parent per
    node, for an unbound end.
  - `bfs_shortest_all`: a queue that carries whole trails, for
    `allShortestPaths`.
  - The ADR-0037 shallow intersection and the ADR-0034 bidirectional
    BFS, for a bound end.
- **Clause route** (`exec/shortest_route/search.rs`):
  - `Bfs::dag`: a layered BFS that keeps shortest-path predecessors.
  - `Bfs::trails`: trail enumeration over those predecessors.
  - `Bfs::closed`: the shortest closed trails through one node.
  - `shortest_pair_path` (in `eval/shortest.rs`) for a single-path
    bound pair.

The review found these problems:

- **The pair-search wrapper is a fake-context adapter.**
  `shortest_pair_path`:
  - builds a `ShortestCtx` from an empty row and empty params with
    `cypher_version: None`;
  - copies `bfs_shortest`'s shallow-versus-bidirectional dispatch rule;
  - threads `best` / `found` / `want` out-params it never reads;
  - returns a `Value::Path` that the route turns back into relationship
    ids, then rebuilds as a path.
- **The expression form's pruned BFS returns wrong results.**
  - `allShortestPaths((s)-[*1..8]-(s))` on an undirected triangle
    returns `null`. The correct answer is 2 trails of length 3.
    `bfs_shortest_all` drops every arrival deeper than a node's first
    depth, so a trail that crosses between the two first-hop branches
    never returns to `s`. The route's `Bfs::closed` finds both trails.
  - With `min_hops` > 1, first-arrival pruning drops the trails that
    revisit a node to reach the minimum. It can return a too-long length
    or `null`. Under the LABS-196 stop rule (`df + db + 1 ≥ L`), the
    reviewer's counterexample `length(shortestPath((s)-[*4..8]->(t)))`
    returns 5 instead of 4. It is pinned as
    `bidir_min_hops_above_one_keeps_searching_after_meet` in
    `exec/shortest_path_tests.rs`. The per-side stop rule fixes that
    graph, not the class: a scratch differential against enumeration
    found 765 too-long lengths or `null`s on the base branch.
- **Cancellation is coarse in the bidirectional search.**
  `expand_bidir_layer` checks cancellation once per BFS layer. Per edge,
  `trail_uses_rel` walks the parent chain. One layer of a dense graph is
  unbounded work between two checks (ADR-0111).
- **ADR-0186 contradicted the code.** It named `bfs_shortest_all` for
  all-path pairs and called the ADR-0034 / ADR-0037 searches unchanged.
  ADR-0186 is unmerged and is corrected in place with this ADR.

Neo4j rejects `shortestPath` with a minimum length other than 0 or 1.
CypherLite accepts it.

## Decision

**All shortest-path graph search for the expression form and the
`MATCH`-clause route lives in one module,
`cypherlite/cypherlite-executor/src/shortest/`. The expression form with
`min_hops` > 1 runs on exact enumeration instead.**

### The shared module

`shortest/` owns:

- **Pair search.** One shortest trail between two distinct nodes, as a
  relationship-id trail (`Vec<RelId>`), or none. One dispatch rule
  picks the search:
  - the ADR-0037 shallow intersection when the pattern is directed,
    `min_hops` ≥ 1 and `max_hops` ≤ 3;
  - the ADR-0034 bidirectional BFS otherwise, with the LABS-196 stop
    rule.
- **Layered predecessor-DAG search.** A single-source BFS that expands
  each node once, at its first depth, and keeps every same-depth
  predecessor for all-path selectors. It serves:
  - one-to-many: every node reached, with its shortest trail(s);
  - all-path: every shortest trail to one end node;
  - closed trails: the shortest closed trail(s) through one node, as one
    search per first relationship out of it;
  - an end-match stop: stop after the first layer that holds a node
    matching an end predicate.
- **Trail helpers**: predecessor-chain enumeration, parent-map
  reconstruction and splicing, the relationship-uniqueness check, and
  building a `Path` from a start node and a trail.
- **The `cfg(test)` expansion counter** that every search bumps and the
  bounded-expansion tests read.

The searches take the graph, the direction, the relationship types and
the hop bounds, plus an end predicate where they need one. They take no
row, parameters, `cypher_version` or `ShortestWant`. They return
relationship-id trails. Callers build `Path` values, or read the length
without one (ADR-0034's `length(shortestPath(...))` shortcut).

Every search checks cancellation per neighbour (`try_for_each_neighbor`
with `cancel_tick()?`), not once per layer. The bidirectional search
checks in O(1) whether a relationship is already on a node's tree trail:
on a shortest-path tree the only trail relationship at the trail's last
node is its parent edge. It compares against that edge instead of
walking the parent chain.

### Callers

- **Clause route** (ADR-0186). It moves under the GPM engine to
  `exec/gpm/shortest_route/` and is tried at the top of
  `match_gpm_path`. Its guard and row binding stay. Its searches come
  from `shortest/`.
- **Expression form** (`eval/shortest.rs`), for a single-relationship
  pattern with static types and labels and no relationship property map,
  the shared module's one followable-pattern check. Other shapes keep
  enumerate-then-min (ADR-0025).
  - `min_hops` = 0 and the start matches the end: the zero-length path,
    as today.
  - `min_hops` ≤ 1, bound end:
    - start ≠ end: the pair search for `shortestPath`; the layered
      search stopped at the end for `allShortestPaths`;
    - start = end: the closed-trail search.
  - `min_hops` ≤ 1, unbound end: a layered search that stops after the
    first layer holding a node that matches the end pattern.
    `shortestPath` returns one trail from that layer.
    `allShortestPaths` returns every trail of that length, to every
    matching node in the layer. When the start itself matches the end
    pattern, the closed-trail search, capped at that length, adds the
    closed trails through the start. The shorter length wins; all-path
    ties merge.
  - `min_hops` > 1: exact enumeration (`eval_shortest_enumerate`), the
    walk ADR-0025 already uses for multi-relationship patterns.

### Removed and unchanged

- `bfs_shortest`'s inline BFS and `bfs_shortest_all` are removed.
  `shortest_pair_path`, the fake-context adapter, is replaced by the
  pair search.
- The expression form's 32-hop default for an unbounded `[*]` is
  unchanged, on the shared module and in enumeration.
- The expression form's row contract is unchanged: one value per input
  row, `null` when no trail qualifies, a list for `allShortestPaths`.
  How several start candidates combine is unchanged for `min_hops` ≤ 1,
  except for one `allShortestPaths` case named under Consequences.

## Alternatives considered

- **Keep two search engines.** Rejected: they solve the same problems
  and already disagree. The expression form misses the triangle's
  closed trails that the route finds. Every fix, such as the LABS-196
  stop rule or per-neighbour cancellation, lands twice or drifts. The
  route reaches the pair search only through a fake-context adapter.
- **Use the layered search for `min_hops` > 1 too.** Rejected: the
  layered search keeps each node at its first depth only. A shortest
  trail of at least `min_hops` relationships can pass a node deeper
  than its first depth, or revisit it (around a cycle), so it is not in
  the DAG. Searching (node, depth) states instead still does not track
  relationship uniqueness, so it is not exact either. ADR-0186 keeps
  `min_hops` > 1 off the clause route for the same reason.
- **Keep the pruned BFS for `min_hops` > 1.** Rejected: it returns
  wrong lengths or `null`. The reviewer's counterexample
  `length(shortestPath((s)-[*4..8]->(t)))` gives 5 instead of 4 under
  the `df + db + 1 ≥ L` stop rule. The per-side stop rule fixes that
  graph, but a scratch differential still found 765 wrong results on
  the base branch. First-arrival pruning cannot see trails that revisit
  a node.
- **Reject `min_hops` > 1 in `shortestPath`, as Neo4j does.** Rejected:
  it is a dialect change that breaks queries CypherLite accepts today,
  and it needs its own ADR. Exact enumeration keeps them working.
- **Put the shared search in `eval/shortest.rs` or in the route.**
  Rejected: either way one form reaches into the other's module. That
  is how the fake-context adapter came about. A neutral `shortest/`
  module depends only on the graph.

## Consequences

- The expression form changes behaviour:
  - `allShortestPaths((s)-[*1..8]-(s))` on an undirected triangle
    returns 2 trails of length 3 instead of `null`.
  - `min_hops` > 1 results become exact: no too-long lengths and no
    false `null`. Enumeration takes the shortest over all start
    candidates together.
  - `min_hops` > 1 now costs enumeration: the number of trails within
    the hop bound, exponential in that bound (32 for an unbounded
    `[*]`). Cancellation still bounds it (ADR-0111). Neo4j rejects
    `min_hops` > 1 for `shortestPath`, so only non-portable queries pay.
  - Start = end with `min_hops` 1 runs the closed-trail search:
    O(deg(s) × (V + E)) per start node, capped at the best length found.
    It replaces linear searches: the bidirectional BFS for
    `shortestPath`, and `bfs_shortest_all`, which misses the triangle,
    for `allShortestPaths`.
  - Which trail `shortestPath` returns among equal-length trails may
    change. It is unspecified, as in ADR-0186.
  - `allShortestPaths` no longer returns a non-shortest trail from a
    later start candidate. Such a candidate can reach an end node in
    fewer relationships than the length the first candidate set, and
    again in exactly that length. The old search added the longer trail;
    the new one stops at the shorter depth, and the length filter drops
    it. This is deliberate.
  - A relationship property map (`[*1..3 {k: 1}]`) moves to
    enumeration, which checks it. The BFS follows types only and ignored
    the map.
- The fraud and identity baseline loadtests use `min_hops` 1 with both
  ends bound (`TRANSFER*1..3`, `REPORTS_TO*..8`). They stay on the pair
  search (the shallow intersection and the bidirectional BFS). Their
  semantics are unaffected.
- The pair search is only called with start ≠ end and `min_hops` ≤ 1.
  There the LABS-196 stop rule (`df + db + 1 ≥ L`) is exact, and the
  per-side stop rule has no caller left.
- Tests pin the triangle result and the `min_hops` > 1 counterexample on
  the expression form. The bounded-expansion regression tests keep
  reading the shared counter.
- No public API change. ADR-0186 is corrected in place to name the
  shared module. ADR-0025 gains a Later references note.
  `docs/engine-architecture.md` names `shortest/`.

## Later references

- ADR-0575 narrows the expression form's endpoint contract: a
  shortest-path expression requires both endpoint node patterns to name a
  bound variable, so the unbound-end and open-start shapes in the Callers
  section are rejected at compile time. The clause route and the shared
  search module are unchanged.
