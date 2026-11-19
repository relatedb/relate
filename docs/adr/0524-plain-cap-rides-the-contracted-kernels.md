---
type: adr
area: engine
claim: 3bc8f289368b3375e7ec332e368594686ef0ac9e
name: 0524-plain-cap-rides-the-contracted-kernels
status: proposed
tags:
  - memory
  - pattern-matching
title: The plain limit cap rides the contracted kernels — the rows mode's want halt
REFERENCES:
  - "[[adr/0473-contracted-traversal-kernels]]"
  - "[[adr/0475-gpm-early-conjuncts-and-riders]]"
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
---

# ADR-0524: The plain limit cap rides the contracted kernels — the rows mode's want halt

## Context

[ADR-0473](./0473-contracted-traversal-kernels.md)'s contracted kernels (the fixed-k chain and the hub-star) take a
`MATCH` clause only through the operator ladder's shared admission gate. Since
the batch substrate landed, that gate's one exception has been the
**plain limit cap**: a `RETURN/WITH … [SKIP s] LIMIT n` suffix arms
`MatchCap::Plain` with `want = s + n`, and the cap *yields* to a specialized
operator for exactly the shapes that operator proves — but the operator
**dropped the cap on the floor**. The trailing projection's own LIMIT
truncated afterwards, so the *rows* were the same as before the cap existed;
the *work* was not. The walker's own cap ([ADR-0475](./0475-gpm-early-conjuncts-and-riders.md)'s rider) halts its
enumeration at `want` stored rows; the kernels enumerated the full path
product first and handed the projection a finished container to trim.

The measured cost of that policy: pokec-bench `pattern_long` —
`MATCH (n1:User {id})-[e1]->(n2)-[e2]->(n3)-[e3]->(n4)<-[e4]-(n5) RETURN n5.id
LIMIT 1` — collects every complete binding of the fused 4-hop pattern before
the LIMIT applies. From the average start that is 3.2M bindings (≈1.4–3 GB of
collected rows); from the lane's hub start (out-degree 6,661) it is ~1.25B:
the statement ran 240 s without producing a first row, and the 2026-11-04
pokec-loadtest warmup was OOM-killed at the container's 12 GB cap. The
frontier family ([ADR-0478](./0478-frontier-nfa-qpp-kernel.md)) already serves **capped emission** for the
quantified shapes; the fixed-k shapes had no capped serving at all.

## Decision

Reshape the kernels' rows mode to carry the halt — `KernelMode::Rows {
want: Option<usize> }` (`None` = every binding) — and let the plain cap
**ride** the contracted kernels as that `want`, replacing the dropped-cap
policy:

1. **The want.** The ladder reads `MatchCap::plain_want` — `skip_limit_want`,
   the same `SKIP+LIMIT` count the walker's `RowLimitCap` halts at — and
   passes it into `try_match_kernel`. The ordered and distinct caps still
   never yield (their semantics are the walker's own; the gate keeps them
   there), and a shape the kernel declines keeps the cap at the walker,
   whose sink applies it.

2. **The chain kernel** halts its **final hop's expansion** at `want`
   landed rows, the check derived from the stage itself (`next.rows()`,
   never a second counter) and placed ahead of each landing — so the halt
   is exact for every `want` including zero. Every row the last hop lands
   is a complete binding, so the halt lands exactly `min(want, total)` —
   count-exact by construction. An intermediate hop is never capped.

3. **The hub-star kernel** halts its pairing loop at `want` pushed
   bindings, checked ahead of each push, and stops the hub loop with it.
   Its side buffers still fill per hub — a capped side could starve the
   pairing below `want`.

4. **Row selection** is the kernel's enumeration order — a contract-free
 prefix under LIMIT without ORDER BY, the rule [ADR-0478](./0478-frontier-nfa-qpp-kernel.md) §4 states for the
   frontier's capped emission: LIMIT promises a count, not a selection.
   Which `want` rows arrive may differ from the walker's prefix; both are
   valid. The TCK's order-insensitive comparisons are unaffected, and the
   existing `LIMIT 99` differentials (want above total) keep answering every
   row.

5. **Intermediate stage and buffer widths are unchanged.** The cost gate's
   estimate still owns admission; its estimate-blindness for unlabeled
   interior hops (a 4-hop chain estimated at ~8k rows that walks 3.2M)
   belongs to the estimate-honesty follow-up, deliberately not this
   ADR's. The halt removes the
   final-collection and emission blowup — the measured pathology — not the
   stage machinery's own cost.

6. **The batch scan/filter chain's plain-cap treatment is unchanged** (the
   cap still yields there and is dropped): its emit is scan-bounded, not
   path-product-bounded. Bringing the halt to that rung is a follow-up
   candidate, not this decision.

## Alternatives considered

- **Keep the dropped-cap policy and rely on estimate honesty to
  decline the pathological shapes.** Rejected: honest estimates bound
  *admission*, not the *serving* of capped shapes the estimates do admit —
  a 400x underestimate still engages, and the walker's cap already proves
  the halting semantics the kernel lacked.
- **Decline capped clauses to the walker (remove the plain-cap yield for the
  kernel rung).** Rejected: the walker's per-row machinery loses exactly
  where the kernel's columnar frontier wins; the yield's traversal pull-in
  was the point of the exception. The riding `want` keeps the pull-in
  and adds the halt.
- **Cap every stage at `want` (memory-optimal).** Rejected: starvation —
  the first `want` partial bindings can all die in a later hop, and a
  capped intermediate stage would answer fewer rows than exist, which LIMIT
  forbids. A starvation fallback (re-run uncapped, or at the walker)
  re-introduces the pathology precisely in the sparse-success cases and
  doubles the code paths.
- **Restructure the chain kernel as a depth-first capped drain.** Rejected:
  redundant with the walker's own DFS; the level-synchronous columnar
  frontier is the kernel's value, and the final-hop halt keeps it.

## Consequences

- Capped chain and hub-star clauses serve bounded: the final collection is
  ≤ `want` rows, the emission ≤ `want` rows, and the PROFILE walk tallies
  shrink with the halt (the fan-out differential: 65,641 walked rels where
  the dropped-cap path count is 2.56M — the intermediate stages' widths
  plus one landing).
- LIMIT answers through the kernels are count-exact; prefix selection may
  differ from the walker's (contract-free, see Decision 4).
- The OOM/hang class this decision targets closes for the shapes the
  kernels take; honest interior-hop estimates remain the admission-side
  fix for shapes the cost gate should not have engaged at all.
- The ladder's and the kernels' documentation now state the riding policy
  where they stated the dropped-cap policy ("a cap a specialized operator
  rides is dropped, never silently applied" — retired).
- Pinned by the kernels suite's halt tests: halt-at-want,
  count-exactness through SKIP, late-hop non-starvation, the hub-star halt,
  and the estimate-blind fan-out's bounded walk tally.
