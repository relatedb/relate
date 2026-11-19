---
type: adr
area: engine
claim: 3dd0cd80a1fafe80c07768e651fad00edfd01f8a
name: 0617-dense-slot-reservation-ladder
status: proposed
tags:
  - memory
  - storage
title: Dense id-slot reservation — the smaller of a 1.5 ladder and the doubling
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0202-graph-floor-memory]]"
  - "[[adr/0532-dense-id-stores]]"
---

# ADR-0617: Dense id-slot reservation — the smaller of a 1.5 ladder and the doubling

## Context

The dense id stores ([ADR-0532](./0532-dense-id-stores.md)) hold node, relationship and adjacency rows in
id-indexed slot arrays: a `Vec` whose reserved capacity rounds up to the next
power of two. A heap census of an LDBC SF0.1 restore (327,588 nodes,
1,477,965 relationships) puts the relationship slot array at 288 MiB —
144 B per slot, 2,097,152 reserved for 1.48 million live rows, 70% fill — and
the node array at 40 MiB. The power-of-two slack (~86 MiB on relationships
alone at this scale) is the single largest bookkeeping term in the resident
store, and it grows with scale: the reservation model is also what the memory
floor charges ([ADR-0042](./0042-graph-memory-limit.md), [ADR-0202](./0202-graph-floor-memory.md)), so the slack is billed to every admission
check.

The slack is worst exactly where ids arrive monotonically: a restore (or any
bulk load) lands just past a power of two and keeps the doubling for the
store's whole life. A write past the restore span — one relationship from a
write-ahead tail — re-reserves the next doubling, so sizing the array exactly
at restore time from the manifest's next id does not survive the common
snapshot-plus-tail reopen.

## Decision

The dense span arrays — the four id-indexed stores of [ADR-0532](./0532-dense-id-stores.md) and the
sealed tier's per-node span tables — reserve the smaller of two ceilings
taken at or above the needed length: the 1.5 growth ladder (1, 2, 4, 7,
11, …) and the old doubling. A pure ladder regresses a span sitting just
under a doubling rung it overshoots — an SF1-sized relationship array
would grow ~160 MiB over the doubling — so the reservation is pointwise
never above the doubling and never above 1.5x the span. Growth stays
amortized O(1) with a worse copy constant: the merged rung sequence
interleaves an occasional small step between the big ones. The floor
charges the same value: the dense-span byte helpers (the one spelling the
placement gate, the admission estimate and the census read) price the
reserved capacity, so the charge stays identical to the reservation. The
dense-span byte cap and the side-map degradation are unchanged; a span the
doubling refused but the ladder admits now goes dense, and admission
prices it through the same helpers.

A crossing reserves its rung exactly, never through the allocator's
amortized doubling: a doubled buffer freed back down to the rung parks in
the allocator's footprint — measured +400 MB of peak and steady RSS on the
SF0.1 restore — where the doubling's own amortized buffer was always the
next rung and never freed mid-crossing.

Ordinary push-grown vectors — label and property vectors, workspace rows,
adjacency delta and entry columns — keep the power-of-two reservation of
[ADR-0042](./0042-graph-memory-limit.md)/[ADR-0202](./0202-graph-floor-memory.md). Their slack is bounded and small; the measured problem is
the span arrays.

## Alternatives considered

- **The pure 1.5 ladder** — rejected: pointwise it can exceed the
  doubling (SF1 relationships ~160 MiB).
- **Exact-fit reservation when the loader knows the row count** (the
  manifest carries the next ids) — rejected: one write past the restore
  span re-reserves the next rung, so the saving vanishes on the common
  snapshot-plus-tail reopen.
- **A sparse tail for the last partial block** (the side map [ADR-0532](./0532-dense-id-stores.md)
  defines, on the array's own tail) — rejected: per-id placement
  complexity for the last few percent.
- **Moving every reserved vector off the doubling** — rejected: the
  census names the span arrays; ordinary vectors' slack is not the
  measured cost.

## Consequences

Measured on an LDBC SF0.1 restore (327,588 nodes, 1,477,965
relationships): the relationship slot array 2,097,152 → 1,574,801 slots
(288.0 → 216.3 MiB), the node array 524,288 → 466,607 (40.0 → 35.6 MiB),
the floor account 739.7 → 660.3 MB; no span grows past the doubling. The
synthetic memory-shape pins move with the account (down, or unchanged
where the doubling stays the smaller) and the two fill-to-cap counts fit
more nodes. The baseline logical-byte envelopes move inside their compare
band. [ADR-0532](./0532-dense-id-stores.md)'s reservation paragraph is superseded for the span arrays;
its side map and dense-cap gate stand.
