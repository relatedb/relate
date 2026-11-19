---
type: adr
area: engine
claim: dc2d98e2e4490fc4d53384cc7eb1f383dcc10314
name: 0516-frontier-rearrival-budget-fallthrough
status: proposed
tags:
  - pattern-matching
title: The re-arrival budget's tri-state verdict and the frontier's run-time fall-through
REFERENCES:
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
SUPERSEDES:
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
---

# ADR-0516: The re-arrival budget's tri-state verdict and the frontier's run-time fall-through

## Context

[ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 3 gave the frontier BFS its pool-exactness seam: the
re-arrival rule's bounded trail-existence search, capped by a shared
work budget (`REARRIVAL_WORK_BUDGET`) across all searches of one run.
Decision 3 documented the budget corner as a *conservative rejection*:
a search the budget cut answered "no trail", and the endpoint was
dropped.

That corner is a live silent wrong answer (LABS-952). Under an exact
quantifier `*k..k` the NFA is a one-state cycle, so the dedup filter
marks every reachable (node, state) pair at the walk's first levels and
**every** accepting arrival is observed below `min` — every endpoint is
a below-min candidate, and the shared budget is the clause's whole
exactness. On the measured repro (a seed with an N-clique plus a short
tail, `MATCH (s:User {id:'s'})-[:R*4..4]->(n:User)
RETURN count(DISTINCT n)`), N = 8/16/24 answers exactly, but at
N = 28/32/36 the first hopeless candidate's search (a node with no
4-trail explores the clique's ~N⁴ distinct-rel walks) drains the shared
budget; every remaining candidate's search then returns "no trail"
without searching, and the frontier answers 0 where the truth is N+1.
`WHERE n.id = 'u'` returns no rows; `collect(DISTINCT n.id)` returns
`[]`. The mechanism is a conflation: the search's early-exit channel
(`Stop::Exhausted`) was flattened into the same `None` as a proven
miss, and the per-target memo cached the cut as a rejection, so one
drained budget silently poisoned every later verdict of the run.

## Decision

The search verdict becomes **tri-state end to end** — `Proven(witness)`
/ `Disproven` / `Exhausted` — wired through the existing internal
`Stop::Exhausted` channel, no new mechanism:

1. The step core (`rearival_search::seek`) unwinds on `Stop`: the
   budget cut propagates as `Err(Stop::Exhausted)`, distinct from a
   fully-explored miss's `Ok(())` (a cut position still records no
   dead-end memo — an unproven miss was already never a dead end); the
   rule's driver maps the channel onto the `Verdict` exactly once.
2. The memoized per-target `VerdictEntry` stores the three-way
   `Verdict`: a cut search caches as `Exhausted`, **never** as
   `Disproven`, so the memo can never later reject on a search that
   proved nothing.
3. `admits()` answers admission only: a `Disproven` rejects, an
   `Exhausted` neither admits nor rejects — the cut surfaces through
   the rule's exhaustion flag. The runner's candidate pass admits only
   on `Proven`.
4. Any `Exhausted` in a frontier run declines the run's outcome
   (`RunOutcome::Declined`, the emission's deferred-pass rule included)
   — the run's counters still record, but no run reaches a consumer.
   The rider wiring (`frontier_run`) — the row arm (both the capped and
   the distinct-endpoint consumers) and the EXISTS arm
   (`try_frontier_exists`) — matches the outcome, **discards every
   buffered row**, and declines with the new typed
   `FrontierDecline::RearrivalBudget`, so the ladder serves the clause
   from the next rung unchanged. No partial rows ever escape.

This supersedes [ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 3's budget corner: the conservative
rejection is gone. The depth ceiling (`REARRIVAL_DEPTH_CEILING`) is
unchanged — a miss at the depth cap is a disproof *within the bounds*,
exact for every bounded quantifier; only the work budget's cut is
"no verdict". The fall-through cost is bounded: per clause, the runs of
the seeds walked so far are discarded, and the DFS that takes over has
its own walk-step budget, so the failure direction is loud (the DFS's
own authored budget error), never a silent wrong answer.

## Alternatives considered

- **Fail loud with an authored budget error** (the
 `WalkBudgetExceeded` shape of [ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 6). Rejected: the
  walker answers these shapes exactly — a decline costs a re-walk, an
  error costs the query. The family's direction is fail-through
  (Decision 6's allocation half chose decline over an authored error
  for the same reason), and the DFS's own caps keep the re-walk
  bounded.
- **Raise or unbound the work budget.** Rejected: it moves the cliff,
  it does not remove it — some N always crosses any fixed budget, and
  an unbounded search re-opens the blowup the budget exists to cap.
  The tri-state makes every budget correct, not just a big enough one.
- **Per-candidate (or per-search) budgets instead of one shared pool.**
  Rejected: the run's total rule work becomes candidates × budget —
  unbounded in the pair set — which is precisely the bound the shared
 pool exists to give ([ADR-0478](./0478-frontier-nfa-qpp-kernel.md) Decision 3: a run never spends more on
  the rule than the one budget).
- **Keep the conservative rejection and document the divergence.**
  Rejected: a documented wrong answer is still a wrong answer — the
  measured repro returns 0 for a count of N+1, and the divergence class
  (any `*k..k` over a dense enough cyclic type) is ordinary user data,
  not a pathology.

## Consequences

- The LABS-952 repro answers exactly at every N: the run declines on
  the drained budget and the DFS serves N+1; inside the budget (small
  N) the frontier still serves — the fall-through is the budget corner
  only, never a needless stand-down (both halves pinned in
  `frontier_rider_tests`).
- A cut search can no longer poison a run's later verdicts: the memo
  holds `Exhausted`, and one cut anywhere discards the run's rows —
  the answer is the DFS's, exact by construction.
- Runs that never touch the budget are byte-identical in behavior:
  `Proven`/`Disproven` are the old `Some`/`None`.
- The frontier family gains its first run-time decline; the decline
  vocabulary (`FrontierDecline`) names it, and the differential suites
  keep the fall-through honest (engagement counters, the recorded
  decline reason, row identity with the stood-down DFS).
- The TCK stays the net either way: a declined clause runs the
  unchanged GPM per-row DFS — same rows, same errors.
