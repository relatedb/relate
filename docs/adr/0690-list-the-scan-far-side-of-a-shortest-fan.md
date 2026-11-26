---
type: adr
area: engine
claim: b7b2e40d97c053047f191e3ece333bbd1d9db13a
date: 2026-11-25
deciders: Relate maintainers
name: 0690-list-the-scan-far-side-of-a-shortest-fan
status: accepted
tags:
  - executor
  - pattern-matching
  - perf
title: A shortest fan lists its scan-territory far side so the shared sweep applies
REFERENCES:
  - "[[adr/0567-shortest-route-endpoint-listing-is-capped]]"
  - "[[adr/0612-batch-the-shortest-fan-with-a-multi-source-sweep]]"
---

# ADR-0690: A shortest fan lists its scan-territory far side so the shared sweep applies

## Context

A shortest-path pattern whose endpoints are both label-only, such as
`(rack:Rack)-[:HOLDS|ROUTES|CONNECTS*]-(router:Router:Egress)`, has no
property map for the route to seek. Both sides resolve to scan territory,
the fan runs from the start candidates, and the far side stays unlisted
([ADR-0612](./0612-batch-the-shortest-fan-with-a-multi-source-sweep.md) names an unlisted far side as a reason the
per-origin layered fan runs). Every origin then walks its whole ball, tests
each reached node against the end pattern, and never stops early, although
the end side is often small: a few dozen egress routers against hundreds of
racks. The multi-source sweep, the far-set bit test and the early stop all
need a far list the route never built.

## Decision

- When a fan of at least two origins has a far side in scan territory (no
  property map or seekable conjunct, so no probe answered it), the route lists it on the fan's
  behalf through the capped candidate funnel, bounded by the full-listing
  limit of [ADR-0567](./0567-shortest-route-endpoint-listing-is-capped.md). The list becomes the fan's far set, so
  the batched sweep, the bit test and the early stop apply unchanged.
- A single origin lists nothing: it has no sweep to share, and an input-row
  loop would otherwise re-list the side once per row.
- The listing never refuses. A side past the limit, a shape whose scan
  answers a superset, or a property map that reads a variable the walk binds
  later stays unlisted, and the fan tests each reached node against the end
  pattern exactly as before. An over-bound probed side is not listed here:
  ADR-0567's rule that such a listing is never materialized unasked stands.
- A listed far side with no member matches nothing, so the route answers the
  empty pair without running a search.
- Answers do not change. The far list is the funnel's exact answer for the
  end pattern, the same one a probed side's listing uses, and every
  partition still passes the end-pattern admission and the pair predicates.

## Alternatives considered

- **Always list, refusing past the limit.** Rejected: a query that runs
  today would fail with a capacity error.
- **A bidirectional search per pair when neither endpoint is bound.**
  Rejected: it multiplies searches by the product of the sides, which the
  shared sweep avoids.
- **Resolve the label union adjacency per hop.** Rejected as a separate
  concern: the per-origin cost is dominated by the ball each origin builds,
  not by reading the relationship-type union.

## Consequences

- A label-to-label fan with a small end side runs through the shared sweep
  and stops each origin once every end node is reached.
- The end label's bucket is read once per input row, up to the listing limit;
  the start candidates were already listed in full.
- An end label no node carries answers without a search.
