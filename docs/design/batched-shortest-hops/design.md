# batched-shortest-hops

## Destination

The shortest-path searches read adjacency in per-node batches, so pokec q30 (`SHORTEST 1`, 20×20 ids,
undirected `KNOWS*..3`) runs faster than on main (measured: 1.43×, 2352 to 1644 ms), with answers unchanged.
The aspiration is traversal-benchmark parity or better; the measurement says how far this slice gets.

## What the analysis found

Sampled profile of q30 on main (in-process sampler and samply agree on the big buckets; machine load
was 50–75, so only proportions count):

- 80–90% of samples are per-pair bidirectional searches that the fan runs for each pair
  (`fan_level` → `pair_trails` → `bidir` → `expand_layer`). The one-sided multi-source sweep is 16–19%.
  Anchoring, trail reconstruction and aggregation are ~0%.
- Per followed edge: the cancellation poll ~30% (`each_hop_along` calls `cancel_tick()` and
  `note_expand()` per edge; `tokens::poll` reads a thread-local ledger before the sampled counter),
  endpoint lookup `rel_ends_at` ~47% inclusive (SipHash `HashMap<i64,…>`, the `IdVec` side map), visited
  set growth ~5–7%, allocator 3–8%.
- `for_each_typed_ends` (`graph/adjacency.rs`) copies a node's hop ids into a fresh `Vec`, drops the
  store guard, then resolves each rel's endpoints one at a time. The adjacency hop carries only the rel
  id and an optional ts, not the peer. `Direction::Both` also builds a dedup `UsedSet` per node.
- Forcing the pair anchor (`SEEK_PAIR_LIMIT` 1024) is only ~12% faster than the fan: the anchor choice
  is not the cost, the per-edge work is.
- The frontier NFA kernel is already batched but declines q30 (`count(*)` over trails,
  `FrontierDecline::PathCount`) and the shortest route never uses it.

## Scope

- A **shortest-local batched hop reader**: one call per node entry that returns that node's hops
  (peer, rel id) already resolved at the snapshot, in sub-batches sized by a constant. It replaces
  `each_hop_along` in `shortest/pair.rs`, `shortest/multi_source.rs` and `shortest/layered.rs`.
- **Endpoint resolution in one pass per node (or per sub-batch)**, with no per-edge hash lookup. Whether
  a node's hops for all types and both directions fit one batch, or are sub-batched, is decided by
  measurement.
- **Cancellation polled once per node entry** (or once per N nodes, N chosen by measurement); the sampled
  counter stays. Latency bound: a running query stops within roughly 0.5–1 s; kill is an exception path,
  not control flow.
- Visited sets presized or reused across pairs where the profile shows growth cost.
- A small `Graph` primitive underneath, kept minimal so a later generalisation is possible.
- A new ADR (minted late, Status `Proposed`) for the batched read contract and the cancellation cadence.
- Measurement on pokec: q30, q21, q22, k4096h6, one revision per binary, same session, load noted.

## Out of scope

- The write-overlay arm (`try_for_each_hop_overlay`): unchanged; the read-only snapshot path only. Big
  writes combined with big pattern matches in one transaction are rare; graph refactorings need their own
  look at eagerness, seams and frontiers.
- The frontier NFA kernel and the walker.
- Making every traversal use the batched read (`EXISTS {}`, `COLLECT {}`, general patterns): a follow-up
  once this slice proves out.
- Carrying the peer in the adjacency hop (storage-format change).
- Count-only and length-only consumer pushdown (separate ticket, separate design).

## Decisions

1. **Target.** 5× on q30 is the aspiration, not a gate; report the measured factor honestly.
   Alternatives: a fixed gate (rejected: unknown ceiling before measuring).
2. **Where to batch.** Shortest-local first, then decide whether to generalise. Alternative: change
   `Graph` so every traversal gets it (rejected for now: wider blast radius, and the NFA path already
   batches).
3. **Cancellation.** Poll per node entry (or per N nodes), not per edge. Alternatives: per edge (status
   quo, 30% of samples), per chunk of N edges (not needed unless a hub node makes one node's walk too
   long; revisit if measurement shows hub latency).
4. **Endpoint resolution.** One pass per node resolving all rels, no per-edge hash; sub-batch by degree
   if needed. A hasher swap on the side map is the fallback only. Alternative: peer in the hop
   (storage-format change, out of scope).
5. **Overlay and kernels unchanged.**

## Open questions (research nodes for plan)

- Do pokec's rel ids actually live in the `IdVec` side map (`DENSE_SPAN_CAP_BYTES` exceeded), so the
  47% is partly a cap artefact? Confirm by reading, then by a counter in a scratch build.
- Batch size and the all-types/both-directions vs sub-batch trade-off: measure, don't guess.
- Poll cadence N (per node vs every N nodes): measure overhead against kill latency on a hub-heavy
  query; the bar is a kill within ~0.5–1 s.
- What is the traversal-benchmark reference number for q30-like shapes (the traverse-bench gap ticket)?
  Needed to say "parity".
- Can `bidir`'s two `Side` trees and the multi-source frontier share the batched reader through one
  trait without a per-node dynamic call?
