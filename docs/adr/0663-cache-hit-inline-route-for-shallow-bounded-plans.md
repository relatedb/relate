---
type: adr
area: engine
claim: 49ca09d741edfeb7b3e64696e8d19eeda5372912
date: 2026-11-22
deciders: Relate maintainers
name: 0663-cache-hit-inline-route-for-shallow-bounded-plans
status: proposed
tags:
  - perf
  - planner
title: A cache hit on a shallow bounded plan runs inline on the caller's thread, no worker handoff
REFERENCES:
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0504-bolt-connection-thread-fast-path]]"
  - "[[adr/0651-plan-cache-hit-verification-without-front-reparse]]"
---

# ADR-0663: A cache hit on a shallow bounded plan runs inline on the caller's thread

## Status

Proposed — refines ADR-0504's inline fast path; rides ADR-0651's hit
admission.

## Context

ADR-0259 runs every statement of a connection on a parked 64 MiB query
worker: the publish-and-wait handoff costs ~6-7 µs, measured as the
largest single component of the warm per-statement floor — 11.6 µs for a
bare `RETURN 1`, 15.9 µs for an indexed point read returning one
property, identical through the Python wheel and the raw C ABI, so the
cost is engine-side, not boundary.

ADR-0504 already built the inline alternative: a connection whose caller
asserted the worker's own stack budget
(`assume_worker_stack_on_caller_thread`) admits and executes bounded-work
statements on the calling thread. Embedded callers — the Python wheel,
the C ABI, any `Database::query` on a default-sized thread — cannot make
that assertion, so every one of their statements pays the handoff,
including the hottest family: point lookups and index streams with a
simple return or fold.

The assertion exists because the fast path's *admission* runs on the
caller's thread, and parse, lowering and planning recurse proportionally
to statement nesting — recursion the parser caps at a maximum nesting
depth of 500, a cap tuned for the worker's 64 MiB, not for a default
2 MiB thread. A pathological but legal statement (a 500-deep expression)
would overflow a small caller stack in the front end.

## Decision

Split the assertion's two roles. The stack-safety role moves into a plan
fact; the wait-shape role ADR-0504 already solved.

1. **A cache hit carries no front** (ADR-0651): its admission is the
   generation check, the eager verification and the gates — non-recursive.
   The deep-recursion class the stack assertion guards cannot occur on a
   hit; only the *miss* parses.

2. **A new plan predicate, `plan_is_shallow_bounded_work`**, beside
   ADR-0504's `plan_is_bounded_work` in the planner: a bounded-work plan
   (already at most one fixed-length hop per pipeline, no nested engines,
   no var-length, no writes — ADR-0504's own residue) whose every
   expression is within a small fixed depth bound. Execution of such a
   plan recurses only over its expression trees (evaluation) and its
   result values (packing), both then bounded by structures any thread
   can carry; the payload boundary's value-depth guard (ADR-0651) already
   refuses over-deep bound values on hits, and the write path's
   value-depth bound is the ceiling for stored-value recursion.

3. **The unasserted dispatch route**: a caller-thread pre-probe runs the
   admission's non-recursive prefix — health, the failed-transaction
   gate, the preview read, and **stage 1 of the cache lookup only** (the
   raw string, a map probe). Stage 2's normalizer is part of the front:
   it walks the statement to its nesting depth, the recursion the
   worker's 64 MiB stack exists for — the probe never runs it. A raw hit
   runs inline only when every safety conjunct holds:

   - the entry's compiled plan is shallow bounded work (the predicate
     above) — execution then recurses only over depth-bounded trees;
   - the entry's carried lowered AST is shallow too (`query_is_shallow_bounded`):
     it is the replan's input on a stats-generation mismatch, and it can
     nest deeper than the plan compiled from it (the planner collapses
     shapes — nested parentheses plan to their innermost expression);
   - no expression of the plan references an auto-extracted placeholder
     (`__AUTO_*__`, ADR-0503): the dispatch epilogue re-parses the
     statement's raw text to restore authored column names for exactly
     those plans, recursion with the statement's own nesting.

   A hit that passes runs the whole statement inline under the fast
   path's panic guard (`catch_inline_panic`): no handoff, no worker.
   Everything else — a miss, a bypass, a rejected hit, a declined or
   unbounded hit — escalates to the worker exactly as today, the lookup
   redone there (one map probe on a cold path).

4. **No gate moves**: the pre-probe is the admission's own prefix, and
   the post-admission body — reservation, admission guard, dispatch,
   epilogue — is the one body every path shares (`run_admitted`),
   unchanged. ADR-0259's panic containment holds on the inline arm
   through the same guard the asserted fast path uses.

## Consequences

- The point-lookup family (index seek, optional one fixed hop, a simple
  projection or a count/sum/avg fold) rides inline from its second
  execution: the warm per-statement floor loses the handoff. First
  executions still parse on the worker — the plan cache is what makes
  the family hot, consistent with ADR-0503/0651's design.
- Stack-safety for this class is now a plan fact, not a caller
  assertion. The predicate's expression-depth bound, the payload
  value-depth guard and the write-path value-depth bound are
  load-bearing: relaxing any of them re-opens overflow on small caller
  threads. Tests pin that a deeply nested but bounded-work plan still
  escalates on a hit.
- A pre-planned kernel for the family (seek → optional hop → fold, no
  pipeline staging) becomes reachable inline on hits; this ADR is the
  routing decision that makes it landable, and that kernel joins the
  executor's kernel ratio guard when it lands.
- The pre-probe duplicates nothing: `admit_statement`'s prefix becomes
  the shared spelling both the worker body and the pre-probe call, so a
  future gate added to admission still runs on every path.
- The route serves the parameterized family (raw-keyed, explicit
  parameters): statements whose literals were auto-extracted stay on the
  worker for now — the epilogue's authored-name restore re-parses their
  raw text.
- The optimistic-hit retry leg (ADR-0651: one uncached re-run when a
  cache-served statement fails with a type-error-class failure), whose
  re-admission parses the raw text, escalates to the worker from this
  route's inline arm (`RetryHost`): the parse recursion is the same
  class the probe exists to keep off the unasserted thread.
- Statements whose literals were auto-extracted keep the worker until
  the epilogue's authored-name restore is reparse-free (authored names
  carried on the entry); that widening is the recorded follow-up.
- The stack-safety proof is pinned by the at-cap suites: the existing
  at-cap-on-2-MiB test now also covers the re-run shape (a raw hit of a
  collapsed deep statement), and the route's own suite proves the family
  runs inline on that same 2 MiB stack while cold, deep, unbounded and
  placeholder-bearing statements stay on the worker.
