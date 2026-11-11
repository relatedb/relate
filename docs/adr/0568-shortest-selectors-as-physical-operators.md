# ADR-0568: Shortest selectors are physical operators of the one pattern evaluator

Status: Accepted
Claim: 594510b89903e28db6a7f97010b824b587221308

## Context

Shortest-path search lives in three places. The `MATCH`-clause selectors
(`shortestPath`, `allShortestPaths`, `ANY SHORTEST`, `SHORTEST k`,
`ALL SHORTEST`) run through the pattern evaluator. ADR-0186 routes the
shapes it can follow onto a breadth-first search, and ADR-0189 and
ADR-0556 share and extend that search; every other shape falls back to
enumerate-then-select. The expression form (`shortestPath(...)` inside
`RETURN` or `WITH`) is a separate expression node with its own search
driver and its own candidate-order rules (ADR-0203). The frontier NFA
(ADR-0473, ADR-0478, ADR-0536) serves quantified patterns, but selector
clauses never enter it.

The cost of the split is measured, not hypothetical:

- Enumeration is exponential in the hop bound. ADR-0186 recorded
  statements running over 120 s that the BFS answers in 2 ms on a
  953-node graph; on a medium social-network graph, enumeration never
  finishes.
- The route is shape-gated, so spelling matters. On the real
  soc-pokec graph the `SHORTEST 1` selector form timed out after more
  than 900 s until its auto-parameterized count was fixed to take the
  route, and answers in about 15 ms now. The postfix quantifier spelling
  of the same question still declines the route.
- Two forms of one feature diverge in semantics, which ADR-0189 already
  paid for once (wrong results in the expression form's pruned search).

## Decision

1. **Selectors are physical operators.** Every shortest selector is a
   physical operator of the one pattern evaluator, chosen by plan shape.
   The ADR-0186 route, with ADR-0189's shared search and ADR-0556's
   pair search, is formalized as the engine's BFS operator. Enumeration
   followed by selection is the fallback operator for shapes the BFS
   cannot follow, such as multi-relationship or quantified bodies. The
   routed shapes, selector set and result contract of ADR-0186 are
   unchanged; this decision adds no behaviour. Operators obey ADR-0555
   (streaming) and charge retained state per ADR-0463.
2. **The expression form normalizes onto the pattern machinery.**
   The front end lowers the shortest expression onto the same pattern
   plus selector form the clause uses, and the expression form's private
   search driver is deleted: one evaluator, one set of semantics. The
   unified engine takes the global shortest across all start and end
   candidates. Two accidental behaviours of the expression form are
   removed: `shortestPath` stopping at the first start candidate that has
   a trail, and `allShortestPaths` letting the first candidate fix the
   length cap (a result that depended on candidate order). An expression
   with a minimum length above one already takes the global shortest, so
   this makes the form consistent with itself. Preserved: one value per
   input row; null when nothing qualifies; a list for `allShortestPaths`;
   closed-trail behaviour when start equals end; the zero-length path when
   start matches end; exact enumeration for a minimum length above one;
   the 32-hop default for an unbounded variable-length relationship.
3. **Witness choice is unspecified.** Among equally short paths, which
   one a single-path selector returns is not part of the contract. No
   Neo4j-parity or newest-first tie-break is promised; ADR-0203's order
   stays an implementation detail of enumeration, not a guarantee.
   Lengths, row counts and all-shortest result sets stay exact.
4. **Native shortest on the frontier NFA is a separate research track.**
   The design note on that question stands. It does not gate this
   decision and a later ADR must carry any change of the operator set.

## Alternatives considered

- **Enumerate-then-select everywhere.** One simple operator, but cost is
  exponential in the hop bound: over 120 s against 2 ms in ADR-0186's
  measurement, and non-terminating on a medium social graph. Rejected:
  unusable on the graphs the engine targets.
- **The route as a bypass beside the walker.** Keep the BFS as a shape
  gated side path, as today. Rejected: it leaves parallel engines with
  documented semantic divergence (ADR-0189 Consequences) and spelling-
  dependent performance cliffs.
- **Keep the expression and clause forms permanently.** Rejected for the
  same reason: two search drivers to keep correct, with every fix paid
  twice.
- **Pin a tie-break contract (newest-first or Neo4j order).** Rejected:
  it constrains every future search operator for a property the language
  does not specify and users cannot rely on across systems.

## Consequences

- One operator set to optimize and test; later speedups (such as
  ADR-0556's pair search) reach every spelling the front end maps to it.
- Multi-candidate expressions change results: the global shortest
  replaces the first-candidate answer, and the test pinning candidate
  order is rewritten. The early exit was a speed feature for large
  candidate sets; the implementing change measures the cost.
- Tests that pinned a witness must pin length or count.
- The hop default and minimum-length handling move to the front end's
  lowering and are preserved.
- Spellings the route still declines, such as the postfix quantifier,
  stay on the fallback operator until widened or served natively by the
  research track.
- Implementation lands after acceptance, behind the existing
  differential tests of both forms.
