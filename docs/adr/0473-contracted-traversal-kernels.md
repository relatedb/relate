---
type: adr
area: engine
claim: 8d20b3491db8ecc22a763783a01aa8d9c6848a48
name: 0473-contracted-traversal-kernels
status: proposed
tags:
  - pattern-matching
  - perf
title: Contracted traversal kernels over the batch substrate
REFERENCES:
  - "[[adr/0218-stats-based-match-orientation]]"
  - "[[adr/0468-soa-batch-substrate]]"
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
---

# ADR-0473: Contracted traversal kernels over the batch substrate

## Context

[ADR-0468](./0468-soa-batch-substrate.md) landed the structure-of-arrays substrate and its first consumer —
the scan-filter chain (`exec/batch.rs`, LABS-359). Layer 3 of the execution
architecture (2026-10-31 session)
is the contracted traversal kernel set: a small number of compiled
traversal operators that recognize whole `MATCH` clause shapes at plan time
and run them as tight loops, instead of re-entering the per-row walker for
every binding. The same session's measurements bound the win: q30's exists
answer cost ~4x more in per-hop bookkeeping than a hand loop with the same
plan; the PathDB categorization
shows the fixed-k
chain and hub-star classes are the dominant *answering* shapes of the
bounded-pattern workload (the frontier-NFA class is the separate follow-up
feature).

The design constraint that shaped this ADR is the combinatorial one: the
natural generalization — one kernel per (hop count) × (filter axes at each
hop) — is a template grid of near-identical loops, and every axis added to
a hop multiplies the set. The pgrust/Gavity analysis (same session) showed
JIT and columnar-native planning do not transfer to our workload (sub-ms
embedded traversals, WASM build forbidding runtime codegen, MSRV 1.88);
their *methodology* — compile the program, don't enumerate its variants —
does.

What already exists and must not be rebuilt: the aggregate folds
(`agg_fold`), the exists probe (`exists_probe`), the scan-filter chain, and
the LABS-348 hub join. The kernels complement them: every shape a kernel
declines falls through to exactly today's path with identical rows.

## Decision

Add a contracted-operator layer (`cypherlite-executor/src/exec/kernels/`)
with three parts, all fail-through:

1. **A structural matcher** (`kernels/matcher.rs`), plan-time and linear in
   pattern size, that recognizes exactly two traversal families and
   compiles everything the kernel runs on:
   - the **unrolled fixed-k chain** — one pattern of 2..=4 fixed single
 hops, walked from the planner-oriented selective end ([ADR-0218](./0218-stats-based-match-orientation.md)'s
     `reverse_path`), with a scan segment at that end; and
   - the **hub-star** — exactly two patterns of 1..=2 fixed single hops
     sharing one hub node variable at an end of both (the q19/q01/q26
     comma-pair family), optionally sharing a terminal variable at the
   opposite ends (a bounded cycle — q30's shape), enumerated hub-at-a-time
   with a per-hub local combination. The scan-shaped single node stays the
   scan-filter chain's family; the chain kernel's scan segment is that
   chain over id slots.
   Every `WHERE` conjunct must decode into a closed predicate vocabulary
   (`kernels/program.rs`): a static label conjunction plus at most one
   property predicate per variable — comparison (the shared `binary` and
   its operator table, mirrored comparisons oriented), null check, or the
   single-entry property map the planner folds every `var.key = value`
   into. Two predicates on one variable, functions over the row, `OR`,
   `NOT`, var-length or quantified hops, property maps beyond one entry,
   repeated variables, `OPTIONAL`, a carrying driving row, or a cap/sink
   projection riding the clause decline; the per-row walker keeps it,
   answers identical (the differential suites pin that).

2. **Monomorphic predicate programs, never per-row `dyn Fn`**: the kernels'
   loops are generic over a sealed `NodePred` trait with three
   implementations (labels, one comparison, one null check); each stage
   dispatches once — one `match` over the compiled program — and every
   per-candidate check inside the loop is a statically dispatched,
   inlined call. Row-invariant bounds evaluate lazily, once per stage, at
   the first candidate that reaches the comparison — an erroring bound
   surfaces exactly when the per-row path's would. The frontier is typed
   column buffers (structure-of-arrays at fixed arity: the scan origin,
   one rel-id and one node-id column per completed hop), and the breaker
   projects through the `Chunk` type layer (`Chunk::from_columns` →
   `Chunk::to_rows`), the scan-filter chain's own discipline. The chain's
   rel-uniqueness rule is an inline lineage scan (at most four rels,
   never a per-row `HashSet`); the hub-star's cross-side rule is the
   per-pair disjointness check (at most four rels again).

3. **A cost gate** (`kernels/mod.rs`) over the planner's own estimate
 machinery ([ADR-0218](./0218-stats-based-match-orientation.md)'s endpoint ladder and degree tails through the live
   `GraphStats`, never a second statistics implementation). The chain
   engages under an affordability ceiling on each stage's estimated width.
   The hub-star engages only when the hub-driven plan's estimated work is
   no worse than the terminal-driven alternative — the plan the per-row
   walker's cheapest-start ladder already runs — with existence mode
   allowed a 2x slop because its per-hub combination early-exits at the
   first hit (kept from the exists probe: side B streams and the first
   combining arrival answers).

Engagement points: the row-returning `MATCH` arm in the dispatcher (after
the scan-filter chain's attempt), and the existence path
(`patterns_exist`, ahead of the exists probe's depth-first seek) with the
boolean-count fold's counted expression. The LABS-348 hub join stays for
the shapes outside this vocabulary — multi-row driving tables and
uncompilable predicates — where its measured floor still applies; the
kernel's flat per-hub combination is what subsumes its design (no grouped
bookkeeping, so no 128-row minimum).

Two measured engine fixes ride the same decision, both on the kernels' hot
path and paid by every consumer of the funnel: label-only `lookup_nodes`
no longer materializes each candidate's full node (property map included)
to check two labels — the id-based label check answers, labels interned
once; and the prepared programs intern their labels once per stage instead
of per candidate.

Results at GBL SF0.1 (host, release, the suite's own methodology): q30
p50 **1.87 ms → 0.12 ms** (target ≤ 0.5 ms, answer `[true]`, the exists
kernel with early exit); q25 5.2 ms (CI baseline) → 0.031 ms; q19
8.9 → 3.3 ms (the label-only funnel fix; it still routes to the aggregate
fold); every other query improved or held. For ten sampled queries whose
plans match no family (q2, q4, q5, q6, q8, q21, q22, q27, q28, q29),
`EXPLAIN` output is byte-identical to the pre-change binary and results
are identical — the kernels live below the planner and decline by
construction.

## Alternatives considered

- **A full template grid, one kernel per filter-axis combination**
  (hop count × predicate kind × predicate position): rejected — it is N²
  near-identical loops, every new predicate axis multiplies the set, and
  the compile-time/authored-code cost is exactly what the compile-don't-
  enumerate principle exists to avoid. The monomorphic `NodePred` set
  gives the same inlined per-candidate checks with three implementations.
- **Runtime dispatch over a large operator table** (a predicate-program
  enum walked per row, the vectorized-systems `SelectionVector` model at
  clause granularity): rejected — every hop pays a table lookup and an
  indirect branch per candidate, and the fall-through risk grows with the
  table: one mis-dispatched shape is a wrong answer, not a slow one. The
  matcher's closed proofs plus one dispatch per stage keep the failure
  mode a decline, never a misinterpretation.
- **JIT / runtime codegen** (the pgrust/Gavity approach, HyPer-style):
 rejected in [ADR-0468](./0468-soa-batch-substrate.md) already for this workload — sub-ms embedded
  traversals, WASM build forbidding runtime codegen, MSRV 1.88 — and
  nothing in the kernel measurements reopened it: the hand-loop gap was
  closed by removing machinery and interning, not by compiling plans.
- **Terminal-driven hub intersection with a hash by hub** (the LABS-348
  join's shape generalized): rejected as the hub-star's primary plan — the
  per-hub sides are O(degree) small, so the local combination is two tiny
  buffers and a linear scan with no hash table, and existence mode keeps
  its early exit per hub; the cost gate still declines whenever the
  terminal-driven plan estimates cheaper, which is exactly when the
  walker (whose cheapest-start ladder starts there) should keep it.

## Consequences

- Matched shapes answer through tight loops with the walker's own check
  order (candidates from the one funnel, labels-before-predicates, the
  lineage and cross-side uniqueness rules, three-valued `WHERE` logic) —
  the differential suites, the openCypher TCK (3892/3892), and the
  cip-eval ratchet gate the equivalence.
- Emit order is the kernels' own (hub-major, stage-major) where the
  walker's is its walk's; every consumer above this layer compares
  order-insensitively (the TCK's any-order steps, the latency suite's
  answer checks). A pinned test that assumes the walker's order on an
  engaged shape would surface immediately in CI.
- Error parity follows the scan-filter chain's stated caveat: rows kept
  and dropped are identical, but a pathological `WHERE` whose conjuncts
  error on disjoint candidate subsets can surface a different conjunct's
  message than the walk order would.
- The predicate vocabulary is deliberately one-predicate-per-variable;
  richer programs (two conjuncts on one variable, function predicates)
  are extension points that must widen the *closed* set — a new `NodePred`
  implementation plus a matcher arm — never a per-row interpretive
  fallback.
- The frontier-NFA kernel for recursive QPP shapes is out of scope and
  charted as the follow-up feature; its design lives in the PathDB
  research note.
- First review round, repaired (engagement proofs unchanged — no shape
  keeps the walker or the probe that the Decision admits): the exists-mode
  counted read evaluates across **every** pairing of each side-B row, not
  the first — only the terminal is shared across a B row's pairings, and
  only in the shared-terminal mode, so a mid-node counted expression
  varies across them (pinned by
  `exists_counted_reads_every_pairing_of_a_b_row`); and the interned label
  admission falls back to the overlay's string arm for a label the base
  interner has never recorded while a write scratch is mounted —
  write-path labels stay strings in the overlay, interned only at commit
  (pinned by `scratch_labels_admit_through_the_kernel`).

## Later references

[ADR-0478](./0478-frontier-nfa-qpp-kernel.md) adds the frontier family to this kernel layer: a third matcher
arm answering the recursive QPP shapes (a Thompson NFA compiled from the
`QuantifiedPathPattern` IR, level-synchronous BFS over deduplicated
(node, state) pairs) under the same fail-through discipline — every shape
its answer-shape gate declines keeps the per-row walker, answers
identical.
