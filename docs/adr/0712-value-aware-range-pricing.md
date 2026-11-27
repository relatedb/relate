---
type: adr
area: engine
claim: 4e6ea42ef37c846b4a73ddcd104330316cb6a720
date: 2026-11-27
name: 0712-value-aware-range-pricing
status: proposed
tags:
  - perf
  - planner
title: A range bound priced from its value, with a per-hit check that the cached plan's price still holds
REFERENCES:
  - "[[adr/0503-template-keyed-plan-cache]]"
  - "[[adr/0508-planner-stats-consult-memo]]"
  - "[[adr/0687-plan-cache-token-footprint-invalidation]]"
  - "[[adr/0694-default-selectivity-defaulted-estimate]]"
  - "[[adr/0696-per-key-value-statistics]]"
  - "[[adr/0700-plan-cache-key-stats-epoch]]"
---

# ADR-0712: A range bound priced from its value, with a per-hit check that the cached plan's price still holds

## Context

A range predicate on an unindexed key (`p.age > 90`) prices from the key's
numeric extremes when the per-key statistics are built
([ADR-0696](./0696-per-key-value-statistics.md)): the window spans its
share of `min..max`, values assumed uniform. That covers a **literal**
bound. A bound that is a caller's **parameter** (`p.age > $min`) still
prices from the default table ([ADR-0694](./0694-default-selectivity-defaulted-estimate.md)),
a third for one side and a tenth for a window, whatever the data, because
the planner has no value to price from.

Two facts shape what can be done:

- The plan-time statistics adapter is built with the statement's parameter
  map, so a value *is* readable at planning time (equality pricing already
  reads it).
- The plan cache ([ADR-0503](./0503-template-keyed-plan-cache.md)) keeps one
  plan per auto-parameterized template and serves it for every later
  literal or parameter value. A plan priced from the first value is
  therefore reused for values that may select a very different share of the
  rows: `age > 99` then `age > 1` is one template and one orientation. The
  literal range pricing and the heavy-value equality pricing already have
  this property; a parameter bound would add one more way to hit it.

So the question is not only how to price a parameter, but how a value-priced
plan stays honest when it is served for another value.

## Decision

1. **Capture the selectivity a plan was priced at.** When planning prices a
   range or equality term from a value, the planner records the term's
   selectivity bucket (the base-2 logarithm of the fraction, rounded) on the
   plan's footprint ([ADR-0687](./0687-plan-cache-token-footprint-invalidation.md)), beside
   the counts it already carries. A plan that priced no value records none
   and is unaffected.
2. **Re-check the bucket on every hit.** A template hit recomputes the
   bucket of each recorded term for the hit's values — min/max arithmetic
   against the already-published key statistics, no scan and no planning —
   and serves the plan when every bucket matches. A mismatch re-plans from
   the cached AST, exactly as an epoch mismatch does
   ([ADR-0700](./0700-plan-cache-key-stats-epoch.md)), and the re-plan stores
   the new buckets. Bucket width is one factor of two in selectivity: a
   change the cost model could act on, never noise.
3. **Price a parameter from its value when it has one.** At plan time, with
   the value, a parameter bound prices exactly like a literal of that value
   (same window arithmetic, same basis), and its bucket is recorded per
   decision 1.
4. **Price from the distribution when it has none.** With no value (an
   `EXPLAIN` of a parameterized statement run without parameters, a
   parameter the caller has not supplied), a bound with usable key extremes
   prices at its expectation over uniform bounds: one half for a one-sided
   range, one third for a window, with the same sketch-level basis. A key
   with no usable extremes keeps the default table. No bucket is recorded, so
   such a plan is never re-planned for value changes: it was priced for no
   value in particular.
5. **Bound the churn.** A template whose recorded buckets alternate keeps
   re-planning at most a fixed number of times; after that the entry is
   marked value-volatile, re-prices from the distribution (decision 4) and
   records no buckets, so a workload that sweeps one parameter across the
   whole range converges on one plan instead of re-planning per hit.

## Alternatives considered

- **Price parameters from the default table only (status quo).** Simple and
  stable, and the plan never depends on a value. Rejected as the end state
  because the default is a fixed fraction of the label: a parameter bound is
  as often selective as not, so the start choice is a coin flip on exactly the
  windows (`age > $min`) agents write most.
- **Price from the first execution's value and never revisit.** Cheapest to
  build. Rejected: one unlucky first value pins an orientation for the life of
  the entry, and nothing in the entry says the price was value-dependent.
- **Re-plan on every execution of a value-dependent template.** Always right
  for the value; pays a plan per hit, which erases the cache for the
  statements this targets. Rejected on cost.
- **Custom then generic plans (compare after N executions, as some
  relational systems do).** Principled, but needs per-template execution
  history and a generic-plan cost the planner does not compute. Rejected for
  now; the bucket check is the same idea reduced to one comparison the
  planner can already make, and the volatile marker in decision 5 is its
  generic-plan fallback.

## Consequences

- A value-priced plan can no longer silently serve a value whose selectivity
  is a factor of two or more away from the one it was priced at; the hit path
  pays one small arithmetic check per recorded term.
- The plan footprint gains a field, and the template hit path gains a
  re-plan reason; both live in `relate` and the planner, and the bucket
  arithmetic reads key statistics through the existing adapter, so this
  touches the planner, the plan cache and the stats trait but no storage
  format.
- Literal ranges and heavy-value equalities get the same protection as a side
  effect, closing a gap that exists today.
- An unparameterized `EXPLAIN` shows the distribution-priced figure with its
  sketch basis, never a value-priced one.
- Cost model detail (bucket width, the churn bound) is tunable without a new
  decision; the contract is that a recorded bucket is re-checked and a
  mismatch re-plans.
