---
type: adr
area: engine
claim: c001a0eeed23c94d901e3935802acdbe358f44a1
date: 2026-11-22
deciders: Relate maintainers
name: 0658-delta-hop-reservation-quantum
status: proposed
tags:
  - storage
title: Delta hop vectors reserve at a 256-hop quantum above the power-of-two ladder band
REFERENCES:
  - "[[adr/0551-sealed-delta-adjacency-store]]"
  - "[[adr/0599-adjacency-floor-arena]]"
  - "[[adr/0655-churn-guarded-stability-seal-floor]]"
---

# ADR-0658: Delta hop vectors reserve at a 256-hop quantum

## Context

The delta tier reserves every hop vector at the next power of two
(`reserved_len`, [ADR-0617](./0617-dense-slot-reservation-ladder.md)'s ladder). For
the steady state — chunks of one to five hops — the ladder is tight: a
5-hop chunk reserves 8 slots. Above it the ladder overshoots without
bound: a chunk grown by inserts to 4097 hops holds 8192 reserved slots,
64 KB of dead capacity for one hop, and a supernode-scale chunk carries
that margin until the fold seals it.

The segmentation literature that motivated looking here (Teseo,
Sortledton) attacks per-edge allocation and pointer chasing with unrolled
linked blocks — a cost this tier does not have. Hops are one
`Vec<AdjHop>` per chunk; batch times are flat across a 500,000-relationship
transaction and allocation never appears on the profile. The measured cost
is the reservation itself.

## Decision

**Above the 256-hop boundary, a hop vector reserves 256-granularity, not
the power-of-two ladder:** `hops_reserved_len(len)` is
`min(reserved_len(len), len.div_ceil(256) * 256)`. The ladder stays for
the ≤256-hop majority, where it is tight. One policy serves both the fit
(`fit_hops_reserved` at every hop-vector allocation site —
`insert_hop`'s post-insert normalization, `install_chunk`'s restore-path
install) and the charge (`adjacency_bytes` prices each chunk's hops at
`vec_heap_hops`), so the census, the admission estimate and the actual
capacity cannot drift apart.

The 256 is imported evidence, not a derivation: the measurement below
shows the win concentrated exactly where the supernode shapes sit, and
nothing in the tier's arithmetic prefers one quantum over another — any
value that keeps the steady-state band on the ladder and bounds the
overshoot to the quantum serves. A supernode chunk now over-reserves by
at most 255 hops (4 KB) instead of up to 100% of its size.

## Measurement

One transaction of 500,000 relationships on the update-shape harness
(`update_bench`, research-gated), interleaved A/B on identical input;
census figures byte-exact.

| metric | ladder | quantum | delta |
| --- | ---: | ---: | ---: |
| concentrated out-delta unfolded (100 chunks × 5000 hops) | 13,114,272 B | 8,199,072 B | -37.5% |
| concentrated effective B/hop (out, unfolded) | 26.2 | 16.4 | -37% |
| spread delta bytes (5-hop band) | 19,945,728 B | 19,945,728 B | 0 (by design) |
| small-tx throughput (4 threads) | 21,390/s | 21,089/s | -1.4% (noise) |
| mixed p50 / p99 | 48 / 67 µs | 48 / 69 µs | 0 / +3% (noise) |
| fold time | 0.38 s | 0.38 s | 0 |

The win scales linearly with supernode degree (~51 KB saved per
10,000-hop hub between folds); everything in the ladder band is untouched
by construction.

## Alternatives considered

- **Unrolled linked blocks per the literature.** No measured cost it
  addresses: allocation is off the profile, and blocks would trade the
  one-`Vec` read path for pointer chasing.
- **A delta page pool / arena for hop vectors.** Rejected by measurement:
  the sealed arena is already the managed home for settled hops
  ([ADR-0599](./0599-adjacency-floor-arena.md)), the sub-floor population
  is sealed by [ADR-0655](./0655-churn-guarded-stability-seal-floor.md)'s
  floor, and a pooled append-growth design amplifies the mid-write
  seal/re-insert churn class that decision's guard exists to prevent.
- **Cap the ladder (reserve exactly `len` above a boundary).** Exacts are
  fine for wholesale-built chunks but re-fit on every insert past the
  boundary; a quantum amortizes the realloc the exact fit would pay per
  hop.

## Consequences

- A 4097-hop chunk reserves 4352 slots (the regression test pins 5→8,
  256→256, 4097→4352), and `adjacency_bytes(std::iter::once(4097))`
  charges the same 4352 — the charge and the capacity share
  `hops_reserved_len`, so a change to either is one edit.
- Restored chunks reserve at the quantum too: `install_chunk` fits the
  payload's hop vector on install, so an open-time supernode chunk prices
  the same as one grown by inserts.
- The steady state (1–5 hops), the spread shapes and every read path are
  unchanged — the quantum only narrows dead capacity above 256 hops.
