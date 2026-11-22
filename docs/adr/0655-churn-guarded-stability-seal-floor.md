---
type: adr
area: engine
claim: d361d9248900e683786e7d93d68733b82c86f3e7
date: 2026-11-21
deciders: Relate maintainers
name: 0655-churn-guarded-stability-seal-floor
status: proposed
tags:
  - storage
title: The stability seal floor is the seal's break-even (4), and a chunk seals only after holding its length across two fold passes
REFERENCES:
  - "[[adr/0551-sealed-delta-adjacency-store]]"
  - "[[adr/0599-adjacency-floor-arena]]"
  - "[[adr/0608-seal-restored-adjacency]]"
  - "[[adr/0635-adaptive-stability-seal-floor]]"
---

# ADR-0655: The stability seal floor is the seal's break-even (4), held across two passes

## Context

[ADR-0635](./0635-adaptive-stability-seal-floor.md) made a never-sealed
chunk seal below the knee once its length held across one checkpoint
interval, at a floor of 8. Two facts have moved under it.

**The floor was a header-overhead guess, not a derivation.** ADR-0551's
sweep picked 8 to keep the seal clear of the band where the entry header
outweighs the stream it buys; it never priced the tier's own strides. They
have since moved: the delta hop is 16 B (`AdjHop`), the delta chunk entry
is 40 B (the stability baseline's own word included), the sealed chunk
entry measures 56 B, and the sealed arena's stream charges ~1.1 B/hop.
Sealing retires 16 B per hop against 56 B plus ~1.1 B/hop, while the 40 B
delta entry is retained either way and cancels: 16L ≥ 56 + 1.1L holds
first at L = 4 (3.76 rounded up), a wash at 3 and a loss below. A floor of
8 leaves the 4–7 band delta for no reason the tier's arithmetic supports.

**One quiet interval is the smallest possible evidence of a pause.** The
rule reads the chunk's length at two consecutive passes, so a chunk that
pauses for exactly one interval and then resumes is accepted as steady.
Measured on the spread shape — 100,000 chunks of 5 hops, one transaction
of 500,000 relationships — the first checkpoint notes every chunk (sealed
hops 0) and the second seals all of them. The next insert then trips the
25% ratio gate at delta = 2, and from that seal every two further hops pay
a full decode and re-encode of the chunk, an arena append and a whole-arena
compaction. Over the 32 hops the knee would have taken to seal once, that
is one seal and sixteen refolds.

## Decision

**The floor is the seal's break-even: `STABILITY_SEAL_FLOOR` is 4.** A test
computes it from the live strides — `size_of::<AdjHop>()`,
`size_of::<DeltaChunk>()`, the sealed entry's 56 B and the arena's
~1.1 B/hop in tenths — so a stride change moves the break-even and fails
the build.

**A chunk seals on stability only after holding its length across two fold
passes.** The pass that first sees a banded chunk notes its length; the
next pass that finds the same length marks it held; the pass after that
seals it. The guard is a `u32` noted length plus a held flag on the chunk
— hop counts are `u32`-bounded (`arena::hop_count`), and both fit the
padding before the hop vector's own word, so the entry stride stays 40 B:
0 B per chunk, both pinned stride tests unmoved. The hold is derived in
one place (`set_baseline`: a second pass at the same length, and 0 never
holds), never handed in by a caller.

Unchanged: the knee arm (`COMPACTION_DELTA_FLOOR` = 32), the ratio gate,
the horizon reclaim, and the wholesale build's own gate
(`RESTORE_SEAL_HOPS` = 4, [ADR-0599](./0599-adjacency-floor-arena.md) /
[ADR-0608](./0608-seal-restored-adjacency.md)) — a rebuild or restore has
no churn to guard: its chunks are installed unnoted, so each starts its
hold from its first post-open pass.

## Measurement

One transaction of 500,000 relationships on the update-shape harness
(`update_bench`, research-gated), the same binary shape on both sides.
Census figures are deterministic and byte-exact.

| metric | before (floor 8, one pass) | after (floor 4, held) | delta |
| --- | ---: | ---: | ---: |
| spread settled out-delta bytes | 19,945,728 | 7,145,728 | -64.2% |
| spread settled in-delta bytes | 19,945,728 | 7,145,728 | -64.2% |
| concentrated settled in-delta bytes | 17,423,040 | 7,145,728 | -59.0% |
| concentrated unfolded out-delta bytes | 13,114,272 | 8,199,072 | -37.5% |

The settled rows are taken after three checkpoint passes with no writes
between; with the guard the second pass only confirms the held length, so
the spread's `settled2` census still reads 19,945,728 B per side. The last
row is the hop reservation's own win, unchanged by this decision.

The spread write is the churn scenario: the guard's whole point is that it
costs the bulk write nothing, because the write time spans `begin` to the
last statement, before the commit, and the fold it refuses to arm runs
after it. The wall clock is not a census: this harness's own run-to-run
spread on this host is 2.4× (the floor-8 and unguarded floor-4 trials
recorded 3.40 s and 6.84 s on identical input), so the evidence is an
interleaved A/B of the previous head (floor 8, one pass; both sides carry
the hop reservation) against the merged head, three runs per side on fresh
stores under a load average of 10–12 on 16 cores: 4.72 / 4.94 / 5.28 s
against 4.86 / 4.75 / 5.08 s — parity within that spread, best-of-three
4.72 s against 4.75 s.

## Alternatives considered

- **An insert-since-last-pass mark on the chunk.** The same protection for
  the growth case, plus the delete-and-reinsert-same-length churn the
  length check cannot see — but it needs a write on the hot insert path
  (`insert_hop`, `push_chunk`, `reposition`, `from_runs`), for a case the
  ratio gate never reads (it compares sealed length, not membership), and
  it is free only packed into another field's spare bit. More surface for
  no measured gain.
- **Put the floor back to 8, or a hop-cost-derived 6.** Re-derives the knee
  instead of gating the pause, and gives up the memory win the floor buys:
  the 5-hop spread chunks stay delta at 199 B/chunk (19,945,728 /
  100,000) against 71 B/chunk (7,145,728 / 100,000) once sealed.
- **Widen the ratio gate (delta × 4 → delta × 8).** Halves refolds but
  delays every legitimate merge, the horizon tombstone reclaim included,
  and treats the churn after the seal rather than the premature seal.
- **A per-chunk fold counter or pass-count backoff.** Extra state (at least
  8 B/chunk with padding) and a new tuning constant for behaviour no
  requirement asks for.

## Consequences

- A steady sub-knee chunk seals three checkpoint intervals after its last
  write — one pass to note, one to confirm, one to seal — where
  [ADR-0635](./0635-adaptive-stability-seal-floor.md) said two. A chunk
  that pauses for only one interval never seals from that pause.
- Settled residency moves: the spread shape's 5-hop chunks still seal
  (19,945,728 B of delta per side before, 7,145,728 B after the settle
  passes), and the concentrated shape's in-side (5 hops on each of 100,000
  endpoints) settles at 7,145,728 B against 17,423,040 B. The guard
  changes when a held chunk seals, not whether.
- The held flag is a within-session hint, never durable state: the
  checkpoint payload carries the sealed stream, the unsealed delta hop ids
  and the tombstones only, and installs chunks with a clear baseline
  through `DeltaChunk::from_hops`, so an installed chunk starts its hold
  from its first pass after the open.
- Confluence and idempotence are unchanged: the guard moves when a fold
  fires, never its result. A fold the arena walls refuse keeps the
  baseline, so the next pass compares the same length.
