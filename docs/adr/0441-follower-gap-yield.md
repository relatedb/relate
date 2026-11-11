# ADR-0441: Yield a follower gap instead of loading a second graph

Status: Proposed
Claim: 60d7f923d88571613b63820efdcc05aab830f637

## Context

ADR-0137 reloads a follower that cannot keep applying: a checkpoint
recycled WAL it had not applied, a trimmed tail moved under its cursor,
or a redo batch applied only a prefix. The reload loads the snapshot and
the surviving WAL beside the installed graph, then swaps. Queries already
pinned to the old graph finish on it. That second graph is the memory
cost. A host whose process is sized for one graph is killed while the two
copies overlap.

The missing commits exist only in the new snapshot, so the destination
graph still has to be materialized. Streaming the snapshot does not
remove the overlap. The host can instead stop serving, drop the engine
after in-flight work releases it, and open one new follower.

## Decision

1. **`FollowGapPolicy` on `OpenOptions`.** `Reload` is the default and is
   ADR-0137 unchanged: `WalCatchUp::Gap` and a torn redo batch call
   `follow::reload`. `Yield` does not load a snapshot. It records a
   permanent follow failure whose error is `Error::resource_limit`
   (`Error::FOLLOW_GAP`, limit 0, `retryable: true`) and returns that
   error. The follow loop stops. `Database::needs_reopen` reports the
   latch. `connect` fails with that error via `check_healthy`, which the
   HTTP layer already maps to 429 with `metrics.retryable: true`.
2. **The loaded graph stays.** `Yield` does not swap it out and does not
   build another one. A torn apply is fenced by the same latch, so a
   query that has not yet passed `check_healthy` does not read the torn
   graph. A statement already past that check runs to completion on it.
3. **The host reopens.** CypherLite does not drop its own `Database` or
   open a replacement under `Yield`. The embedding process observes
   `needs_reopen`, finishes or closes work that holds the `Arc`, drops
   the engine, and opens a new follower. Embedded callers that do not
   do that keep `Reload`.

## Alternatives considered

- **Keep reloading in place for every host.** Rejected: the overlap is
  a second copy of the graph, and a process sized for one copy is killed
  before the swap frees the first.
- **Retain WAL until every follower acknowledges an LSN.** Rejected for
  this decision: a correct ack crosses processes or files and is the
  start of a replication protocol. A host that can restart its own
  engine does not need it.
- **Reload on every checkpoint.** Already rejected by ADR-0137. A caught-up
  follower still does not reload.

## Consequences

- `Reload` callers, including the default and the existing gap tests, are
  unchanged.
- A `Yield` follower that hits a gap stops tailing and refuses new
  connections until the host drops it. Freshness after that is the new
  engine's open, not a swap inside the old one.
- `Database::demote` is not this path. It still does not reload the graph.
