---
type: adr
area: engine
claim: 6392630dfbad9d25e269684305fd67cb1e89c73c
name: 0599-adjacency-floor-arena
status: proposed
tags:
  - perf
  - storage
title: Adjacency floor layout — node-major arena with exact-size sealed pricing
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0551-sealed-delta-adjacency-store]]"
  - "[[adr/0608-seal-restored-adjacency]]"
---

# 0599: Adjacency floor layout — node-major arena with exact-size sealed pricing

## Context

At SF0.1 no adjacency chunk reaches the fold floor (mean 1.7 out-hops per
chunk), so the sealed tier ([ADR-0551](./0551-sealed-delta-adjacency-store.md)) never engages and the whole out+in
cost is the container and slot floor: 147.4 MiB, of which the payload is
24 B/hop and the rest is per-chunk scaffolding — a 32-byte
`(u32, Vec<AdjHop>)` entry per chunk, its `Vec` header and pointer chase,
and the power-of-two capacity the floor charges every vector. Pow2
rounding hides most of any codec win: a ULEB hop count cuts 21% of the raw
bytes but only 2.8% of the charge. The floor is the census number the
budget is set against, so it must match the layout the tier wants.

## Decision

Restructure the sealed tier's storage into one node-major arena and
charge it at exact size.

- **Delta hop presence boundary.** The delta hop drops its niche-less
  `Option<i64>` for a raw `i64`, presence moving to an `absent` count in
  the chunk entry's padding — a 16-byte hop at the unchanged 32-byte
  stride. This layout, not the arena, moves the SF0.1 census.
- **Node-major arena.** One `Vec<u8>` holds every folded chunk's encoded
  stream, a node's chunks adjacent, so a scan is sequential and pointerless.
- **12-byte entries.** Per node span, a type-sorted `{type_id, off, hops}`
  array — `off` into the arena, `hops` the encoded count — replaces the
  per-chunk `(u32, SealedChunk)` tuple and its `Vec` header. The entry
  carries the hop count and the first raw id, so the codec's leading
  header dies — 80.3% of a mostly-single-hop chunk's encoded bytes — and
  type sorting replaces the linear scan.
- **6-byte node spans.** A dense id-indexed span table gives each node
  its entry range, so no per-node map is needed.
- **Exact-size pricing.** A sealed stream is immutable and sized once at
  its encoded length, never grown by `push` after the fold, so the floor
  charges it for exactly its length, not the next power of two. The delta
  tier and the sealed tombstones still grow by `push` and keep the
  reservation.
- **Fold and compaction.** A fold appends new encodings and marks the
  replaced spans dead; arena compaction rides the checkpoint fold, and the
  delta tier's mutation contracts are untouched.
- **Build-side emission.** The wholesale adjacency build — a restore's and
  an aborted write's alike — seals in its one pass: each (node, type) run
  whose hops reach a 4-hop break-even goes straight into its folded chunk,
 through the same fold the checkpoint's GC pass runs ([ADR-0608](./0608-seal-restored-adjacency.md)). No
  second pass walks the rebuilt graph. The floor differs from the write
  path's 32-hop `COMPACTION_DELTA_FLOOR`: the write path folds a chunk it
  expects to keep growing, so a high floor avoids re-folding, while a
  build is one terminal pass that pays the fold once. Sealing swaps a
  chunk's 24 B/hop delta heap for a 56 B sealed entry plus a ~1.1 B/hop
  stream while its 32 B delta entry is retained either way — net positive
  at 4 hops, a wash at 3, a loss below.

## Alternatives considered

- **Drop the pow2 reservation but keep per-chunk `Vec`s** (exact-size
  sealed alone): rejected — the entry stride and its header dominate
  (88 B per chunk), and packing bytes without the arena keeps the chase.
- **Tune the strides instead of removing the indirection**: rejected —
  per-node scans average 2.7 entries, too few for the stride prefetcher to
  warm, and each entry still chases its own allocation.
- **Change the codec encoding** (varint, miniblocks): rejected — the
  per-chunk fixed cost, not delta encoding, is the bytes lever; the best
  variant buys 0.3–0.6 MB at SF0.1.
- **Persist the arena in the checkpoint dump**: rejected — the dump
  carries no adjacency today, and the in-memory arena reaches the same bar
  at restore without a durable-format change.

## Consequences

- The floor drops to the arena's packed bytes plus the 12-byte entries
  and 6-byte spans; the power-of-two slack on sealed data is gone, and a
  build's emission fits each arena to its exact encoded length.
- The heap census measures the change through its per-side floor split —
  delta bytes, the sealed arena at its exact encoded length, the index
  structures (span table, entries, tombstones), and the dead ranges
  awaiting compaction; the codec becomes a lever only on churned stores.
- The tier's field widths are structural limits, refused at admission
 before the redo is logged ([ADR-0042](./0042-graph-memory-limit.md)'s channel) — the tier's own mutation
  paths are infallible, so the walls they must never hit are enforced
  upstream: a node holds at most `u16::MAX` distinct rel types per
  direction (the span length, counted per group commit so concurrent slots
  cannot split the wall), and one direction's arena at most 4 GiB of
  streams (the entry offsets, each direction checked against its own
  field, priced on the arena's own bytes plus the batch's growth — never
  the store's whole charge, which prices structures no arena holds, so a
  delete is never refused for a bound it shrinks toward). Both fire with
  no size limit configured. No admission guards the wholesale build, so
  its folds ask the same walls before sealing and leave a chunk that would
  cross one in the delta tier — the store opens to its correct rows,
  unfolded, rather than dying in the write guard, and the arena's own
  arithmetic states invariants no path can reach.
- The node-major arena is the base the later supernode mini-pages, zone
  maps, and fold-time sort discriminator build on.
