---
type: adr
area: engine
claim: 258b2fb58369a29c498717c7d8f6635db7ad637e
date: 2026-11-21
deciders: Relate maintainers
name: 0651-plan-cache-hit-verification-without-front-reparse
status: proposed
tags:
  - perf
  - planner
title: A plan-cache hit verifies eagerly and retries on type errors, never re-running the front
REFERENCES:
  - "[[adr/0462-value-nesting-depth]]"
  - "[[adr/0503-template-keyed-plan-cache]]"
  - "[[adr/0504-bolt-connection-thread-fast-path]]"
---

# ADR-0651: A plan-cache hit verifies eagerly and retries on type errors

## Status

Proposed — refines ADR-0503's cache-hit admission contract.

## Context

ADR-0503's hit path re-ran the whole front end on every cache hit — a
full parse-and-lower of the statement with its parameters, inside the
PLAN window — as a safety net: one compiled plan serves a template
family, and lowering is value-dependent. Measured on warm statements
(sampling profile over a repeated mixed workload), that re-parse is
8–16 µs per statement, a third to a half of the whole warm statement
cost, and the single largest fixed cost the plan cache was supposed to
remove.

Working through what the re-parse actually caught shows its live
coverage reduces to small, nameable classes, most already closed
elsewhere:

- **Dynamic operands** (labels, property keys whose value reaches the
  site through a bound parameter) — ADR-0503's insert gate already
  refuses to cache any plan carrying one, so no hit can serve them.
- **Literal type divergence** — the auto-parameter name carries the
  type (`$__AUTO_INT_n__` vs `__AUTO_FLOAT_n__`), so an integer and a
  float over the same site are different templates, never a shared
  plan.
- **Value-dependent folds** — a temporal constructor's argument stays
  in the template verbatim (ADR-0503's function-argument guard), so a
  different literal is a different entry; a constructor over a
  parameter never folds at lowering.
- **Literal nesting depth** — the normalizer's own depth cap refuses
  to extract a list nested past the parser's limit, and the caps are
  aligned with the value-depth bound.
- **Float/integer sites** (a slice bound or list index served a bound
  float where the compiled plan wants an integer) — already a targeted
  plan walk, kept.
- **Type analysis over bound list elements** (a quantifier predicate
  like `x % 2` over a list the statement binds as an extracted
  literal) — the one live class with no targeted check: the compile
  rejects the statement, but a cached plan evaluates the predicate at
  runtime and reports the runtime error class instead.

## Decision

A cache hit never re-runs the front. It verifies eagerly where the
facts live, and retries once where they cannot:

1. **Eager checks, per hit**: the bound float/integer site walk over
   the plan, and the value-depth guard at the payload boundary where
   the normalizer's extracted literals become parameter values. A
   refusal at either check declines the hit — the statement runs its
   own original compile, which reports the error as the uncached run
   would.
2. **Optimistic execution with one uncached retry**: a cache-served
   statement whose execution fails with a type-error-class failure
   (the engine's typed type-error family, never a scan of the rendered
   message) runs once more uncached, and that outcome — the compile
   error, or a clean run — is what the caller sees. The retry lives in
   every dispatch arm (worker body, inline fast path, escalation), so
   no path can skip it.

The retry is safe because the insert gate admits only read-only plans:
the failed attempt had no side effects. The filter is narrow so a
query failing on runtime data never executes twice; only the class
where compile and runtime verdicts can diverge pays the second run.

## Consequences

- Warm hits stop paying the front's cost: the PLAN window on a hit is
  the generation check, the eager checks, and the admission gates.
- Error-class fidelity is preserved: a value-dependent compile verdict
  surfaces as the compile's own error, not the runtime twin, because
  the retry re-runs the statement as written.
- A statement that trips the retry runs twice — read-only, once, and
  only on the type-error class; correct statements never notice.
- The alignment of the parser's nesting cap and the value-depth bound
  is now load-bearing for the cache: the payload-boundary guard is the
  backstop, and a future change to either cap must consider both.
- The insert gate's dynamic-operand exclusion is load-bearing in the
  same way — relaxing it would re-open a class this design no longer
  checks.
