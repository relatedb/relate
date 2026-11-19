---
type: adr
area: engine
claim: 4f32f55b8c6bb6add2fad3379fa0e257ab8e90d2
name: 0608-seal-restored-adjacency
status: proposed
tags:
  - storage
title: 0608 — Sealing restored adjacency
REFERENCES:
  - "[[adr/0551-sealed-delta-adjacency-store]]"
  - "[[adr/0599-adjacency-floor-arena]]"
---

# 0608 — Sealing restored adjacency

## Context

The sealed-delta adjacency store keeps each `(node, rel type)` chunk in one
of two representations: a delta list of hops, or a folded, bit-packed
stream ([ADR-0551](./0551-sealed-delta-adjacency-store.md)). Folding a chunk is an `O(chunk)` re-encode, so it runs
at the checkpoint's GC pass — the one moment the store is already paying a
whole-graph pass, and the moment a chunk's versions are known to be
durable.

A store opened from a checkpoint does not go through that pass. The restore
rebuilds adjacency from the durable rows and re-derives every chunk as
delta; the sealed maps start empty and stay empty until the next
checkpoint's GC pass. For a store that is checkpointed and then reopened
read-mostly, that is the whole steady state: the folded representation the
tier exists to provide is discarded on every open and never restored.

Measured on the rig's mid-degree shape, whose adjacency sits above the
32-hop floor: a checkpointed store charged 19% less for its live logical
graph than the same graph in delta form, and a reopen of that store landed
on the delta figure exactly — the tier's win was live-only, and a
read-mostly workload never saw it. Interleaved A/Bs of the fix confirm it
end to end, and the saving is the folded share of the graph: 16% on a
256k-relationship store whose hops cluster into few large chunks
(53,729,658 vs 63,958,458 bytes), 5% on one whose 256k hops land on 256k
distinct endpoints, where only one side folds (109,432,203 vs 115,592,507).
Both are byte-reproducible, the reopened charge lands on the checkpoint's
own to the byte, and the open pays no more than the same re-encode the
checkpoint GC pass pays.

## Decision

The wholesale adjacency build emits the durable shape. A whole-graph
rebuild — the restore's, and an aborted write's — derives each `(node,
type)` run from the live rows in one pass, then routes every chunk the
fold predicate would seal through the fold itself: the same gate, the
same sort, the same chunk order the checkpoint's GC pass produces, with
the delta side keeping the type's slot and no hops. There is no second
pass and no second charge recompute: the one build produces the final
layout, and a rebuilt graph and a checkpoint-folded one cannot drift.

Safe by construction rather than by a new guard: the rebuilt rows are the
live ones, so nothing a fold could hide is still needed; the sealed maps
are cleared before the build, so no tombstone or merge case can arise; and
both representations spell the same hops, so no reader can tell which one
it reads. The fold predicate's horizon arm is unreachable on a build — it
prices only tombstone reclaims, and a fresh build has none.

The WAL tail replayed after a checkpoint keeps its delta, exactly as it
does in a live store between checkpoints. Sealing the tail is not part of
this decision.

## Alternatives considered

**Fold after the rebuild, in a second pass.** The first implementation:
ending the restore with the checkpoint GC's own fold call. Rejected: on a
fresh rebuild the fold is degenerate — no tombstones, empty sealed maps,
only the floor arm — so the second serial per-node walk re-encodes chunks
the build pass could have emitted folded, and the charge is recomputed
twice. The build-side emission deletes the second pass and the second
recompute, and covers the abort path's rebuild as well.

**Seal in the open flow's post-replay site, after the graph is final.**
Rejected: two open paths (snapshot-only and snapshot-plus-replay) would each
need the call, and the storage layer reaches the engine only through the
import trait, so it would also need a new trait method. The rebuild is the
single site both paths already share, and the tail's delta is the
steady-state shape either way.

**Leave the fold to the next checkpoint.** Rejected: that is the defect —
a read-mostly reopen never reaches a checkpoint, so the representation the
store was closed with is never rebuilt.

**Fold eagerly at every open regardless of the floor.** Rejected: the floor
exists because folding a small chunk costs more than it saves. Sealing uses
the same predicate the checkpoint GC uses, so a reopened store lands on the
shape a live store of the same rows holds.

**Persist the folded representation in the checkpoint.** Rejected for now:
the durable format is a row-oriented snapshot of nodes and relationships,
and the fold is derived state the engine owns. Encoding adjacency into the
durable form is a format decision with its own costs, not a prerequisite
for using the tier on reopen.

## Consequences

A reopened store carries the memory shape its checkpoint was closed with,
so a read-mostly workload pays the delta representation only for the WAL
tail — and an aborted write's rebuild keeps its folded chunks too, instead
of falling back to delta until the next checkpoint. The folded encode
happens inside the build pass, so an open runs no extra whole-graph pass
over it. The durable format, the fold predicate, the floor and the sealed
representation are unchanged; a store's on-disk bytes and its query
answers are the same with or without the emission.

## Later references

- [ADR-0599](./0599-adjacency-floor-arena.md)'s node-major floor arena carries this decision: the build seals
  through the arena's fold in its one pass, and the gate it keeps is that
  tier's 4-hop restore break-even, not the write path's 32-hop floor. A
  reopened store therefore lands at or below the checkpointed charge, and
  on it only when every chunk sits over the 32-hop live floor — the
  accepted drift.
