# ADR-0538: Traversal-rung observability in PROFILE and plan args

Status: Proposed
Claim: 84c55d6c5b438793bd88770b7ecf1591b0a7ac5c

## Context

A `MATCH` clause's traversal path is chosen at run time, inside one plan
operator: the unrolled fixed-k chain kernel, the hub-star kernel, the
frontier-NFA rung, or the per-row walker. The plan tree therefore renders
the same operator and details regardless of which path executes, and the
diagnostic `RELATE_PATTERN_ENGINE` switch changes routing without changing
any EXPLAIN text. The only engagement signal was a test-only counter, so
answering "which path ran this clause" took a sampling profiler and hand
symbolication.

The PROFILE metric sink already carries per-clause facts — phase clocks,
store-read kinds, the walk's chosen start, the frontier family's run
counters — from the executor to the plan tree and the wire. Engagement is
the same kind of fact and belongs in that channel, not beside it.

## Decision

1. **One typed vocabulary.** `KernelRung` names the rung that served a
   clause: the chain (with its hop count), the hub-star, the frontier-NFA
   rung, or the per-row walker. It lives in the executor's shared context
   module beside the other PROFILE vocabularies, and its stable names are
   single-sourced for the wire.
2. **Recorded per clause, from the clause's own operator decision.** The
   operator ladder returns the rung that took the `MATCH` clause — and the
   walker when every rung declines — and the clause's PROFILE tick records
   that returned value; a `SEARCH` `MATCH`, which calls the walker
   directly, records the walker too. The rung is therefore the clause's
   own fact: a nested evaluation (`CALL`, `EXISTS`, `FOREACH`) cannot
   contribute or overwrite one, and no per-row work runs. A clause with no
   `MATCH` operator carries no rung.
3. **Reported on the plan tree and the wire.** The public per-clause
   metric set gains the rung; the wire emits it (with the chain's hop
   count) only when a rung served the clause, and the text plan's details
   cell marks it. Presence-only, like the existing metric facts. Every
   surface that carries the structured plan — the Bolt summary, the HTTP
   Query API v2 envelope, the CLI's machine modes — carries the rung
   through the shared plan presentation, so no surface-specific encoding
   exists or is needed.
4. **The static half is the engine mode.** Every `MATCH` operator of an
   EXPLAIN and a PROFILE tree carries the store's pattern-engine mode
   (`auto`, `nfa`, or `walk`) as a plan arg. This is the one engagement
   fact that is decidable without executing.

Deliberately not done: a plan-time prediction of the kernel rung in
EXPLAIN. The ladder's riders — the entering row count, the cap and sink
projection, and the batch chain ahead of the kernels — are runtime facts,
so a prediction can disagree with execution; stating the mode and
reporting the executed rung keeps the plan honest.

## Alternatives considered

- **A parallel observability channel (event log, callback, separate
  sink).** Rejected: duplicates the metric sink, and every surface would
  need a second transport for one field.
- **A per-row engagement counter.** Rejected: taxes the hot loop for a
  fact that is fixed per clause.
- **Predicting the rung at plan time.** Rejected: the gates read runtime
  inputs, so the prediction can disagree with what ran; a plan that lies
  is worse than one that states only the static mode.
- **Encoding the rung in the operator name.** Rejected: operator names
  are the shared Neo4j vocabulary and downstream consumers match on them.

## Consequences

The question "which path ran this `MATCH`" is answered by PROFILE, in
production builds, without a profiler; a gate regression that silently
returns a shape to the walker is visible as a changed rung. The cost is
one value carried through the ladder and recorded per profiled clause; a
non-PROFILEd statement pays nothing beyond the ladder's own dispatch, and
the hot loop is untouched. The engine mode is diagnostic-only and retires
with the switch it names; the rung vocabulary is additive and can grow
with the rungs.
