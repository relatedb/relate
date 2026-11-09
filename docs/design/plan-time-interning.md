# Plan: resolve token names to ids once per plan (ADR-0541)

Accepted (ADR-0541). Open questions resolved by default: one token space; an absent label skips the store read at execution only (no plan-time folding). Motivation and measurements:
the expansion hot loop spends time hashing label strings per candidate, and
the planner's statistics probes resolve the same names again.

## Where resolution sits

```
parse -> lower -> [cached, store-independent statement]
                         |
                 resolve tokens (new)   <- store interner, per plan compile
                         |
                       plan  (planner reads ids for stats/index probes)
                         |
                      execute (reads ids; string arm only for write overlay)
```

The cache re-plans on every commit (ADR-0503), so ids never go stale.

## Slices

1. **Token type and resolver seam.** Id type plus resolver trait in the
   lowest shared crate; store implements it. No behavior change.
   Acceptance: seam unit tests; boundary check passes.
2. **Resolution step in planning.** Collect names from the statement,
   resolve once, attach the name beside its three-state `Token`
   (`Unresolved`, `Absent`, `Id`) to pattern operators. EXPLAIN
   output byte-identical. Acceptance: plan-display tests unchanged; cache
   byte accounting test covers the added field.
3. **Id-keyed statistics and index probes** in the planner's seam.
   Acceptance: the plan-choice test corpus picks identical plans.
4. **Executor consumers, one PR each:** path fold, walker `node_ok`,
   property-key reads, kernels (drop per-stage resolve), shortest-path.
   Acceptance per PR: query-latency A/B (SF0.1) not worse; differential
   tests unchanged.
5. **Write overlay.** Tests for statements creating labels, types and keys
   they then match in the same statement, and for absent names.
6. **Cleanup.** Remove the per-stage and per-hop resolvers and the
   name-keyed hot-path forms nothing calls.

## Risks

- Overlay-created names (string arm) must keep working.
- A plan reused across stores or generations would carry wrong ids; guard
  with a debug check on the plan's store identity and generation.
- Cross-crate churn: keep slice 1 and 2 small so later PRs are mechanical.

