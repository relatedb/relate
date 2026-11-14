# 0186. MATCH-clause shortest paths run on BFS

- **Status**: Accepted
- **Date**: 2026-10-08
- **Deciders**: Oskar Hane (agreed in the LABS-192 planning conversation), CypherLite maintainers (Linear LABS-192)

## Context

ADR-0025, ADR-0034 and ADR-0037 made `shortestPath` /
`allShortestPaths` a breadth-first search. Only the expression form
(`Expr::ShortestPath`, e.g. `RETURN shortestPath(...)` or
`WITH shortestPath(...) AS p`) reaches that search
(`eval_shortest` in `cypherlite-executor/src/eval/shortest.rs`). The
`MATCH` clause form never does:

- The parser (`cypherlite-parser/src/parser/patterns.rs`) lowers a
  top-level `MATCH p = shortestPath(...)` to the selector
  `PathSelector::Shortest { count: Some(Literal(1)), group: false }` with
  `is_shortest_path_function = true`. `allShortestPaths(...)` becomes
  `PathSelector::AllShortest`. Path mode is forced to none.
- `PathPattern::needs_gpm()` (`cypherlite-ir/src/ast.rs`) is true
  whenever a selector is set. `match_one_path`
  (`cypherlite-executor/src/exec/match_clause.rs`) therefore sends the
  pattern to `match_gpm_path` (`exec/gpm/mod.rs`).
- `match_gpm_path` DFS-enumerates every trail up to the hop bound. An
  unbounded `[*]` is bounded by the relationship count.
- Only then does `apply_gpm_selector` (`exec/gpm/selector.rs`) keep the
  shortest trail(s) per (first node, last node) partition. Ties break on
  the rel-type sequence, then DFS order.
- The GQL shortest selectors (`ANY SHORTEST`, `SHORTEST k`,
  `SHORTEST k GROUP`, `ALL SHORTEST`) take the same route.

The cost is the number of trails, which is exponential in the hop
bound. LABS-192 (release build, main `3ecfa775`) lists five
graph-examples statements that run for more than 120 s. Neo4j answers
them in milliseconds:

- `cybersecurity.adoc:215`, `:230`, `:254`:
  `MATCH path = shortestPath((u)-[*..100]->(crownJewel))`
  (953 nodes, 4,698 relationships)
- `stackoverflow.adoc:181`:
  `MATCH path = allShortestPaths((u1:User {display_name:"alexanoid"})-[*]-(u2:User {display_name:"InverseFalcon"}))`
  (6,193 / 11,629)
- `pole.adoc:241` (LABS-170):
  `MATCH path = shortestpath((p1)-[:KNOWS*]-(p2))` (61,521 / 105,840)

In expression form the same statements take 2 ms (cybersecurity:215,
count 4), 235 ms (stackoverflow:181, 726 paths) and 8 ms (pole:241,
20 paths). A hop sweep on cybersecurity:215 in clause form: `*..4`
takes 0.87 s; `*..6` through `*..12` each take more than 30 s.

A shortest-path pattern never shares relationship uniqueness with its
siblings, so a per-pattern search needs no shared used-relationship
set:

- Under the default match mode (DIFFERENT RELATIONSHIPS), the parser
  rejects a GQL selective selector unless its pattern is the only path
  pattern in its `MATCH` (`MultiplePathPatternsWithSelectiveSelector`,
  `parser/clauses.rs`).
- `shortestPath()` / `allShortestPaths()` are exempt from that rule.
  `match_paths` gives them an empty used-relationship seed and restores
  the prior set afterwards.

For selector patterns, `MATCH ... WHERE` filters rows after
`match_paths` (`match_clause.rs`). The planner rewrite does not fold
`WHERE` into GPM patterns.

## Decision

**A `MATCH`-clause shortest-path pattern with a routed selector and the
routed shape below is evaluated by BFS per (start node, end node)
partition, not by enumerate-then-select.**

### Routed selectors

Under match mode DIFFERENT RELATIONSHIPS, with path mode absent, `WALK`
or `TRAIL`:

- `shortestPath(...)`
- `allShortestPaths(...)`
- `ANY SHORTEST`
- `SHORTEST 1`
- `SHORTEST 1 GROUP`
- `ALL SHORTEST`

Counts are literals only. Under DIFFERENT RELATIONSHIPS a relationship
appears at most once per match, so `WALK` and `TRAIL` both mean trails.

### Routed shape

- Two node patterns and one non-quantified variable-length relationship
  `[*m..n]` with `m` = 0 or 1.
- Relationship types absent or a static disjunction (`:A|B`).
- No relationship property map.
- No inline (`WHERE` inside a node or relationship pattern) or
  path-level `WHERE`.
- Endpoint labels absent or a static conjunction (`:A:B`, `:A&B`).
  Endpoint property maps are allowed.
- The relationship variable is not already bound.
- Each endpoint variable is unbound or bound to a node.

Every other shape keeps enumerate-then-select (`match_gpm_path` +
`apply_gpm_selector`) unchanged, including:

- `SHORTEST k` and `SHORTEST k GROUP` with k > 1
- `SHORTEST $k` (any parameter count)
- `ANY` and `ANY k`
- quantified relationships and quantified path patterns (QPPs)
- `REPEATABLE ELEMENTS`
- `ACYCLIC` and `SIMPLE`
- `min_hops` > 1 (the shortest qualifying trail can revisit a node,
  which first-arrival BFS pruning does not see)
- dynamic relationship types or endpoint labels

### Result contract

Per (start node, end node) partition:

- `D` is the shortest trail length in `[min, max]`. No trail in range:
  no row for that partition.
- Single-path selectors (`shortestPath()`, `ANY SHORTEST`,
  `SHORTEST 1`) return one trail of length `D`.
- All-path selectors (`allShortestPaths()`, `SHORTEST 1 GROUP`,
  `ALL SHORTEST`) return every trail of length `D`.
- `[*]` without an upper bound has no hop cap; only the graph bounds
  the search, as under GPM. The 32-hop default of the expression form
  (ADR-0025) is unchanged.
- Start = end: the zero-length path when `min` is 0; the shortest closed
  trail through that node when `min` is 1.
- Which trail a single-path selector returns among equal-length trails
  is unspecified, as in Neo4j. This replaces the GPM
  rel-type-then-DFS tiebreak for routed patterns. Lengths, row counts
  and all-shortest result sets are unchanged.
- Row order is unspecified.

The search does not apply `MATCH ... WHERE` conjuncts. `WHERE` still
filters the selected rows afterwards, as on the GPM route. The
independence barrier of `match_paths` is unchanged.

### Search strategy

The route lives under the GPM engine (`exec/gpm/shortest_route/`) and
is tried at the top of `match_gpm_path`. A pattern its guard rejects
falls through to enumerate-then-select. Its searches come from the
shared module `cypherlite/cypherlite-executor/src/shortest/`, which the
expression form also uses (ADR-0189).

- **Both endpoints bound, start ≠ end:** one search per pair.
  - Single-path selectors use the shared pair search: the shallow
    intersection (ADR-0037) or the bidirectional BFS (ADR-0034) behind
    one dispatch rule.
  - All-path selectors use the layered predecessor-DAG search from the
    start, stopped after the layer that reaches the end.
- **Start bound, or neither bound (one-to-many):** one layered DAG
  search per start candidate. It yields every matching end node it
  reaches with its shortest trail(s).
- **Only the end bound (end-bound):** one layered DAG search per row
  from the end, on the reversed direction. It yields every matching
  start node; its trails are reversed to start → end.
- **Start = end** (the (s, s) partition: both endpoints bound to one
  node, one variable at both ends, or a searched-from node that also
  matches the other endpoint pattern): the zero-length path when `min`
  is 0; otherwise the closed-trail search. It runs one layered search
  per relationship `r` out of `s`, from `r`'s far end back to `s`, never
  over `r`, capped at the best length found so far.

The layered DAG search expands each node once, at its first depth;
all-path selectors also keep same-depth predecessors. Its cost is linear
in the nodes and relationships it reaches, plus the trails it returns.
The closed-trail search costs O(deg(s) × (V + E)) per (s, s) partition:
one layered search per relationship out of `s`. Neither grows with the
number of trails within the hop bound.

## Alternatives considered

- **Keep GPM enumerate-then-select for the clause form.** Rejected: the
  cost is the number of trails, which is exponential in the hop bound.
  LABS-192 measures more than 120 s on a 953-node graph.
- **Prune the GPM DFS at the shortest length found so far.** Rejected:
  the DFS still walks every trail up to the first hit's length, which is
  exponential in that length. Only per-node first-arrival pruning bounds
  the search by the graph rather than by the number of trails.
- **Rewrite the clause form into the expression form at plan time**
  (`MATCH p = shortestPath(...)` →
  `WITH shortestPath(...) AS p WHERE p IS NOT NULL`). Rejected: the row
  semantics differ. The expression form returns one value per input row
  (`null` when unreachable, a list for `allShortestPaths`) and caps `[*]`
  at 32 hops. The clause form returns one row per selected path in each
  (start, end) partition, no row when unreachable, and has no hop cap.
- **Reproduce the GPM rel-type-then-DFS tiebreak on the BFS.** Rejected:
  it needs the full shortest-path DAG plus a lexicographic selection over
  it, and DFS order is an artifact of enumeration. That rules out
  bidirectional search for bound pairs. Neo4j does not specify a tie
  order either.
- **Route only `shortestPath()` / `allShortestPaths()` and leave the GQL
  selectors on GPM.** Rejected: the parser already lowers
  `shortestPath()` to `SHORTEST 1`, the GQL forms pay the same
  exponential cost, and the sole-pattern rule keeps them as independent
  as the function forms.

## Consequences

- The five LABS-192 statements, including LABS-170 (`pole.adoc:241`),
  run in BFS time instead of enumerating trails.
- A routed single-path selector may return a different trail than
  before among equal-length ties. Lengths, row counts and all-shortest
  sets do not change. A test that pins one tied trail checks its length
  or its membership in the shortest set instead.
- **Cycles on undirected patterns.** For start = end with `min` 1, a
  trail may not leave and return over the same relationship. The
  shortest closed trail is a self-loop (length 1), two parallel
  relationships (length 2), or the shortest cycle through the node. A
  BFS that keeps one parent per node and drops later arrivals misses a
  cycle that closes over a relationship between two different first-hop
  branches (a triangle is the smallest case). The closed-trail search
  therefore runs one search per first relationship (Search strategy),
  for directed and undirected patterns alike.
- Neither-bound patterns still run one search per start candidate. That
  is polynomial, not exponential, but scales with the candidate count.
- A new parity test runs routed patterns against GPM enumeration on the
  same graphs and compares per-partition lengths, row counts and
  all-shortest sets. A bounded-expansion regression test asserts that
  the clause form finishes within a bounded number of expansions on a
  graph where enumeration is exponential. Together they carry the
  contract; the TCK and the frozen CIP-eval selector scenarios stay
  green.
- The ADR-0034 bidirectional BFS changes its stop rule (LABS-196).
  After a meet of length `L`, with `df` and `db` the depths of the two
  frontiers:
  - start ≠ end and `min_hops` ≤ 1: stop once `df + db + 1 ≥ L`. There
    the first meet at the shortest length is always a valid trail, so no
    later layer can beat it.
  - otherwise: the per-side rule. Stop once neither frontier can beat
    `L` (`df + 1 ≥ L` and `db + 1 ≥ L`). A `min_hops` cut or a closed
    trail can reject the shortest split and leave a shorter meet to a
    later layer. Per ADR-0189 the pair search is only called with
    start ≠ end and `min_hops` ≤ 1, so the per-side stop rule has no
    caller left.
- Every search checks cancellation per neighbour
  (`try_for_each_neighbor` with `cancel_tick()?`), not once per BFS
  layer. The bidirectional BFS checks in O(1) whether a relationship is
  already on a node's tree trail, against that node's parent edge,
  instead of walking the parent chain for every edge.
- ADR-0189 moves the expression form onto the same shared search module
  and removes `bfs_shortest_all`. The expression form keeps one value
  per row and its 32-hop default for an unbounded `[*]` (ADR-0025).
  ADR-0025 gains Later references notes naming this ADR and ADR-0189.
- No public API change. `docs/engine-architecture.md` and
  `docs/reference/cypher-syntax.md` describe the clause form.

## Later references

ADR-0466: statement admission now rejects an explicit traversal hop bound
above `--max-traversal-depth` (default 500, ceiling 2000). That limit
applies only to an explicit `max_hops`; the uncapped unbounded `[*]`
route this ADR describes is unchanged, and remains bounded by the
relationship count rather than by a hop cap.

ADR-0600: for the keyword selectors, a clause-level `WHERE` that reads the
path is a post-filter of the selected paths, as this ADR first recorded; the
function spellings and inline `WHERE`s filter the search.
