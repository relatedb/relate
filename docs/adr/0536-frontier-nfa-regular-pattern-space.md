# ADR-0536: The frontier NFA over the regular-pattern space

Status: Proposed

Claim: 7fc50840830bff8d0f272e1481181b2c4586d4ee

## Context

ADR-0478 scoped the frontier-NFA kernel to the var-length spellings: the
NFA compiler accepts one pattern with exactly one top-level `Quantified`
element — the element every var-length spelling lowers to (ADR-0471) —
and the ladder rung's cheap pre-gate shares that count fact. The
restriction is an accident of that scoping, not a property of the
mechanism: the compiler is a regular-expression compiler over a closed
atom vocabulary (plain one-hop rels over static type unions with
per-transition direction, plain interior nodes, bounded/unbounded
quantifiers under the one-cycle interval rule).

The shapes the restriction excludes are the pattern ladder's measured
worst rows — the pattern-engine spike's same-machine traverse-bench
comparison (pokec medium, 100k vertices, one client, 2026-11-07; the
artifacts `bench-medium-relate.json` and
`bench-medium-traverse.json`, the ratios below being the
throughput-qps figures of those artifacts' named query rows): the fixed
3-hop and 4-hop distinct-endpoint chains at 52x and 347x, the 5-node
mixed-direction chain at 246x, the bounded cycle at 16x. Profiling the
fixed 3-hop shape shows zero samples in any kernel: the frontier rung
declines structurally (no `Quantified` element), and the shared
admission gate below it — no cap and no sink projection — excludes the
chain/hub-star kernels from every standalone `MATCH … RETURN` statement,
whose RETURN is the clause's sink projection. The frontier rung sits
above that gate precisely because it serves sink projections: it is the
only rung that can take these shapes, and the diagnostic switch's
forced rung changes nothing because the structural proof declines before
the switch's cost-gate bypass.

## Decision

1. The compile's structural rule generalizes from "exactly one
   top-level `Quantified` element" to **one regular pattern over the
   closed vocabulary with at most one range-quantified region**: any
   number of exact-count regions (`min == max`, zero included — a pure
   concatenation chain), at most one region whose count is a range or
   unbounded, plain rels elsewhere. The soundness argument is
   unchanged: an exact-count region contributes a fixed length and
   unrolls acyclically, so the global hop interval still pins at most
   one cycle.
2. **Exact counts unroll; ranges keep the cycle.** A quantifier with
   `min == max` compiles through the existing structural unroll (k
   copies, linear states) instead of the cycle form. Per-level frontier
   sets are then the unrolled automaton's states — the (node, state)
   pair with state = depth — carried by the existing ever-visited pair
   set and sparse tables (ADR-0521); no new data structure. In an
   exact-only pattern every pair is reachable at exactly one level, so
   the below-min candidate class cannot arise (accepting states sit at
   depth k) and the dedup filter never blocks an in-bounds expansion.
   The state ceiling and the table budget's run-time decline bound the
   k-fold state cost.
3. Everything else rides unchanged: the matcher's gates (spine, pinned
   bindings, conjunct vocabulary, carrying row, mode), the consumer
   decode, the pools, the emission, the work budget, the cost gate, and
   the re-arrival rule — which is also the default-pool
   rel-uniqueness argument for fixed chains (a node-simple
   representative chain is rel-distinct; the rest take the bounded
   search, exactly as for quantified shapes). The ladder pre-gate
   shares the compile's generalized count fact, one spelling.

## Alternatives considered

- **Lower fixed chains into `k..k` quantified elements at the front
  end**, the ADR-0471-style rewrite. Rejected: one engine already
  exists and the IR already carries the shape; the rewrite churns
  EXPLAIN plan text and adds a lowering seam while still needing
  Decision 2's unroll policy to avoid the candidate-search cost.
- **Keep the cycle form for `k..k` and rely on the below-min candidate
  rule.** Rejected as the serving policy: exact (pinned by the
  differential suites) but every sub-k accepting arrival becomes a
  candidate answered by a bounded search after the BFS — on a dense
  graph the candidate set is the whole sub-k ball around the seed.
  Unroll trades the searches for k-times the states under the existing
  ceilings.
- **Per-level bitsets as a second table.** Rejected: ADR-0478 already
  rejected per-level dedup for re-reach exactness; the unrolled-state
  construction delivers per-level precision through the one
  ever-visited pair set.
- **A special-case fixed-chain trigger.** Rejected at the maintainers'
  steer: the general acceptance rule covers fixed chains as the
  zero-region case, and the same construction serves mixed shapes
  (an exact-count region beside a range region).
- **Loosen the shared gate so the chain/hub-star kernels take sink
  projections.** A companion route, not an alternative: it grows the
  kernels' decode vocabulary by a projection fold where the frontier
  already owns pair-shaped sink answers; keeping it a separate decision.
- **Defer.** Rejected on the measured ladder gap: these shapes have no
  other rung today.

## Consequences

- Fixed-length chains, mixed-direction chains, and mixed exact/range
  patterns reach the BFS; the four measured shapes become acceptance
  cases in the latency harness. Memory stays O(pairs), pairs bounded by
  touched nodes × states, with the 64-state ceiling declining chains
  past roughly 63 hops and the table ceiling declining at run time.
- Existing `k..k` spellings change cost shape (candidate searches to
  states), never answers; the differential suites pin identity, and new
  arms pin unrolled-vs-cycle equivalence on the same graphs.
- Endpoint-identity cycles (a repeated variable closing the pattern)
  still decline; an arrival-time identity guard is the recorded
  follow-up. Compound-group alternation has no IR (the vocabulary's
  alternation is the type union on one transition). Multi-pattern
  hub-stars stay the chain/hub-star kernels' family.
- Interior admission keeps the rung policy: anonymous interiors serve
  under the auto rung, labeled ones decline (the forced rung compiles
  them as per-state guards).
- Engine-internal only: no public surface moves, and the diagnostic
  switch's forced rung gains the same shapes.
