---
type: adr
area: engine
claim: a2a4455a251195fa32dc4e6e9ffabb8a99143255
date: 2026-11-22
deciders: Relate maintainers
name: 0656-batched-hop-read-for-shortest-search
status: proposed
tags:
  - executor
  - pattern-matching
  - perf
title: The shortest-path search reads a node's hops in batches resolved in one pass
REFERENCES:
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0186-match-clause-shortest-path-bfs]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
---

# ADR-0656: The shortest-path search reads a node's hops in batches resolved in one pass

## Context

The shortest-path search expands a node by walking its adjacency one hop at
a time: each hop takes the adjacency guard, resolves the relationship's
endpoints, picks the peer, and ticks the cancellation counter. On hub nodes
the per-hop guard and endpoint lookups dominate the expansion. The search
also dedups `Direction::Both` with a per-node set of used relationship ids,
allocated for every node entered.

## Decision

- **Graph primitive** (`relate-executor/src/graph/adjacency.rs`, beside
  `try_for_each_hop_resolved_at`):

  ```rust
  pub(crate) fn try_for_each_hop_batch_resolved_at<E>(
      &self, id: NodeId, dir: relate_ir::Direction, ts: u64,
      types: &RelTypes, buf: &mut HopBuf,
      f: impl FnMut(&[(NodeId, RelId)]) -> Result<(), E>,
  ) -> Result<(), E>
  ```

  It copies the node's hop ids under the adjacency guard into the buffer's
  reusable id vector and drops the guard, then resolves the endpoints of
  the ids in slices of at most `HOP_BATCH` and calls `f` once per slice
  with the visible (peer, relationship id) pairs, `hop_other` picking the
  peer. The paging state lives inside the call; the caller owns only
  `HopBuf` (the id vector and the batch vector) and reuses it across
  nodes. A clone of a `HopBuf` is empty, so copying a search never copies
  a node's hop list.
- **Batch size:** `HOP_BATCH` = 256 hops.
- **Search reader** (`relate-executor/src/shortest/mod.rs`, replacing
  `each_hop_along`): `each_hop_batch_along(dir, node, f)` calls
  `graph.cancel_tick()?` once per node entry (never per edge), then invokes
  `f` once per sub-batch of at most `HOP_BATCH` (peer, relationship id)
  pairs through the primitive above, and calls `note_expands(n)` with each
  batch's length (test builds only). `each_hop`, `hops` and `hops_along` become thin wrappers. The
  buffers (`HopBuf`) live in the search (`Search::scratch`), taken out
  for the duration of a node's read so a re-entrant read gets its own, and
  are not allocated per node. The write overlay (a statement's own scratch)
  collects its whole walk into the buffer, then hands it out in the same
  `HOP_BATCH` slices.
- **Cancellation polling:** the reader is the one owner of the per-node
  tick: it ticks once per node entry and no caller ticks again before
  calling it (callers keep a tick only where they enter a node without
  calling the reader). The tick is the sampled counter of
  [ADR-0111](./0111-engine-bounded-execution.md), with no unsampled
  per-node check: the token is read on every 1024th entry. The kill latency
  is therefore at most 1024 node entries, each one node's adjacency walk;
  it does not grow with the number of nodes expanded.
- **`Direction::Both` dedup:** no per-node used-id set. The walk reads the
  outgoing list, then the incoming list; a self-loop is the only
  relationship present in both, and it is recognised from the resolved
  endpoints (start == end == node), so the incoming pass skips any hop with
  start == end. Hops whose endpoints do not touch the node are dropped by
  `hop_other` as today.
- **Residual:** a reused relationship id repeated inside one list (WAL
  redo replay keeps the old hop) is not collapsed by `Both`, exactly as the
  directed arms never collapsed it; the per-edge `Both` walk did collapse
  it. A differential test over the reused-id fixture pins this: batched
  and per-edge agree once the batched `Both` result is deduplicated.
- **Memory:** the resolved pairs are bounded by `HOP_BATCH` per search. The
  buffer's id vector holds one node's hop ids (8 bytes per hop), the copy
  every adjacency walker already makes under the guard; it is reused, never
  allocated per node, and grows only to the largest degree seen
  ([ADR-0555](./0555-operators-stream-rows-never-accumulate.md)). Bounding
  it too needs a resumable read inside the adjacency tier, which is not
  part of this change.
- **Measured:** pokec q30 runs 1.43 x faster than on main (2352 to 1644
  ms, minimum of three runs, machine load 13 to 18); q21 1.15 x, q22
  1.13 x and k4096h6 1.09 x, with row output byte-identical. The 5 x
  speed-up the design aspired to is not met by this slice.

## Alternatives considered

- **Keep the per-hop walk and only drop the per-node set.** The per-hop
  guard and endpoint resolution remain the cost; rejected.
- **Tick cancellation per batch or per hop.** More polling cost for a
  latency bound the per-node tick already gives; per-edge ticking is the
  overhead this removes; rejected.
- **A hash set for `Both` dedup.** Allocates and hashes for the rare case
  and the self-loop is decidable from endpoints; rejected.
- **Resolve a node's whole adjacency into one fresh vector per node.**
  Allocates per node and the resolved pairs grow with degree on hubs; the
  fixed batch over a reused buffer bounds them; rejected.

## Consequences

- Hub expansions pay one guard acquisition and one resolution pass per
  256 hops, with no per-node allocation.
- Cancellation stays cooperative at node granularity, as before, with the
  reader the one place that polls.
- Other adjacency readers keep their per-hop path unchanged.

## Later references

- [ADR-0111](./0111-engine-bounded-execution.md) (cooperative cancellation
  sampling): the search's reader samples once per node entry through the
  statement's shared tick counter.
- [ADR-0555](./0555-operators-stream-rows-never-accumulate.md): the batch
  buffer is the bounded form this decision relies on.
