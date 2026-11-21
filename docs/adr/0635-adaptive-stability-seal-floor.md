---
type: adr
area: engine
claim: 4eabb62fc0d04d96fc770940cd869bd313f2efa6
date: 2026-11-20
deciders: the maintainers
name: 0635-adaptive-stability-seal-floor
status: proposed
tags:
  - storage
title: "The never-sealed seal floor adapts: a chunk whose length held across a checkpoint interval seals at 8"
REFERENCES:
  - "[[adr/0551-sealed-delta-adjacency-store]]"
  - "[[adr/0608-seal-restored-adjacency]]"
---

# 0635: The never-sealed seal floor adapts — a chunk whose length held across a checkpoint interval seals at 8

## Context

[ADR-0551](./0551-sealed-delta-adjacency-store.md)'s floor of 32 gates only a
never-sealed chunk's **first** seal; after that the 25% ratio rule adapts.
Restored stores pre-seal at 4 hops ([ADR-0608](./0608-seal-restored-adjacency.md)).
The population left uncovered is the **long-running store's steady mid-degree
chunks**: degree 8–31, never reaching 32, held as 24 B/hop delta forever —
exactly the mid-degree graphs the tier was built for. The existing
mid-degree memory shape could not measure this (all rungs at or above 32),
and the sweep's static lowerings were all rejected (the numbers below).

## Decision

The checkpoint fold pass **notes** every floor-gated sub-knee chunk's hop
count (`DeltaChunk::stable_len`; the delta chunk stride grows 32→40 B,
charged to the delta tier, and the baseline dies with the chunk at its
seal). A chunk whose length is **unchanged at the next pass** and at or
above `STABILITY_SEAL_FLOOR` (8) seals then: a chunk that stopped growing
is at its steady state, so the knee's reason for waiting — a still-growing
chunk would only re-fold later — is gone. Growing chunks keep the 32 floor;
the sub-8 band stays delta (header overhead); the ratio rule, tombstone
horizon and the wholesale build's own gate are unchanged. The fold's
below-floor guard admits exactly the stability-eligible chunks — one
shared predicate, so a chunk the pass marked due is never turned back at
the guard.

Measured on the straddling mid-degree shape (1,000 nodes / 91,552 rels,
base = the same graph without the rule): the base's out side stalls at
96.7% hop coverage — 752 of 1,000 chunks, the 8/16-rung band never sealing
across three checkpoints; with the rule, 100% at the second checkpoint,
used bytes 17,977,809 → 17,949,169. The band is thin by the shape's
construction (4 of 16 rungs); a sub-knee-dominated graph converts more.

## Alternatives considered

- **Global floor 16 or 8.** Rejected: ADR-0551's sweep — +71% chunks for
  +0.88pt, and it seals still-growing chunks early.
- **Floor on per-node total degree.** Rejected: the sweep's axis B —
  +0.7–1.4pt for 2–4.3× the sealed chunks.
- **Seal everything at stability.** Rejected: 8–13× the bytes; sub-8-hop
  chunks are pure header overhead.
- **Drop the floor for write-hot chunks.** Rejected: a hot chunk is a
  growing chunk; sealing it early only re-folds at the ratio rule — the
  wasted-work pattern the knee exists to prevent. Stability is the
  complementary reading: the chunk that stopped growing is the safe one.

## Consequences

- A steady sub-knee chunk seals two checkpoint intervals after its last
  write — one pass to note its length, one to compare against it. The rule
  reads the length at consecutive passes: any change that persists across
  the interval invalidates the note, so a growing or shrinking chunk never
  seals on a stale baseline — churn *inside* an interval that returns to
  the noted length still seals, at a length the chunk holds at both
  passes.
- +8 B per unsealed delta chunk, charged and gone at the seal; the Small
  shape pins moved (ring/dense ring +16,000, fanout +8,000). Baselines
  re-freeze through [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)'s
  CI dispatch.
- The mid-degree ladder straddles both floors (16/8 rungs beside
  512–32; 1,456 hops per 16-node group, average 91); every derived shape
  pin moved with the table.
- The heap census exposes the sealed hop share per side (`sealed_hops`) —
  the coverage numerator.
- Confluence and idempotence unchanged: the rule moves **when** a fold
  fires, never its result (ADR-0551's argument); restore keeps
  ADR-0608's own gate.
