# ADR-0387: Statistics-backed counting operator

Status: Proposed
Claim: 204418c19ccef4830179f93a5cec094ecf9b88a1

## Context

Counting whole populations is a common query shape (`MATCH (n:Label) RETURN count(*)`,
`MATCH (a:A)-[:R]->(b:B) RETURN count(*)`, `MATCH ()-[r:T]->() RETURN count(r)`),
and today every one scans its pattern: the executor enumerates candidates and the
aggregation counts rows. The graph already maintains two count families for the
planner's estimates (ADR-0218): per-label node counts (`label_index` sizes via
`pattern_label_rows`) and per-rel-type counts (`rel_type_counts` via
`pattern_rel_type_rows`). LABS-351 added per-(label, rel-type) degree histograms on
the same write-path seams, with the memory-accounting discipline the security review
forced (every query-controlled map charges `used_bytes` on entry creation).
What is missing for counting is (a) the `(start-label, rel-type, end-label)`
directional pair counts and (b) the planner/executor recognition of count-only
query shapes, so a count answers from the maintained statistics instead of a scan.

## Decision

Add a statistics-backed counting operator (LABS-370):

1. **Directional counts**: a `(label, rel_type, end_label)` count map maintained on
   the same seams as the degree histograms (the `create_rel` bump with both endpoint
   labels' live ids, the label-mutation rebucket pair, the `rebuild_adj` fold),
   charged to `used_bytes` on entry creation with the rest of the retained structure
   (the LABS-351/security discipline). The count family lives in one `degree_stats`
   module (extracting the histogram family from `graph/mod.rs` in the same change —
   resolving the deferred structural finding).
2. **Count-only recognition**: a MATCH whose projection is exactly `count(*)`,
   `count(node-var)` or `count(rel-var)`, with no residual predicates, no grouping,
   no DISTINCT, and no LIMIT/SKIP on the counted pattern, is planned as the count
   operator: it reads the maintained counts and emits one row. The operator is
   self-describing in PROFILE (0 db hits).
3. **One count family**: the counts serve the operator, the ADR-0218 ladder's
   estimates, and the LABS-348 hub-join gate — one maintained truth.

## Alternatives considered

- **Estimate-only counts (planner-side, no operator)** — rejected: the counts would
  inform plans but every count query still scans; the user-visible win (0-db-hit
  counts) is the point.
- **Lazy/exact-on-read counting (no maintenance)** — rejected: correctness would
  require a scan anyway; the maintained counts are exact by construction and the
  maintenance cost rides seams that already exist.
- **Reusing the degree histograms for pair counts** — rejected: the histogram is
  bucketed degree data per (label, type); pair counts are exact (label, type,
  end-label) totals — different facts, both maintained.

## Consequences

- `MATCH (n:Label) RETURN count(*)` and its pair/rel variants answer in O(1) from
  the statistics; PROFILE shows the count operator with 0 db hits.
- The statistics are exact (maintained, not sampled), so results are byte-identical
  to the scan; the equivalence is pinned by tests and guarded by TCK/differential.
- The count map is charged to `used_bytes`; the capacity cap covers it.
- The planner's estimates and the LABS-348 gate gain the directional counts.

## Later references

- ADR-0589 adds a label-set count map to this count family and answers
  grouped label aggregates from it.
