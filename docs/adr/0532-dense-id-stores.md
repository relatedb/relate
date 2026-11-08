# ADR-0532: Dense id stores for nodes, relationships, and adjacency

Status: Proposed
Claim: 2e4334908f7748992b54e017c4f4b78688226f8c

## Context

Node and relationship ids are dense and monotonic. They are never reused.
The four core stores — nodes, relationships, and both adjacency directions —
still keep each id in a hash map. Every hop and property read pays for a
hash and a probe, and the map's empty buckets grew when the inline row
(ADR-0529) landed in the value: most of that row's resident-memory saving
was absorbed by the buckets.

A snapshot pin holds a reference to the graph across a commit
(ADR-0038, ADR-0137), so a reader and a writer do overlap. The hash map
was also the lock that made that overlap defined.

## Decision

Those four stores are vectors indexed by id. A missing or deleted id is
an empty slot. The slot stays: ids are not recycled, so there is no free
list. Adjacency grows only up to the highest id that has an edge, so a
graph with no relationships keeps empty adjacency stores.

Each store has one read-write lock. A lookup takes the read lock and
drops it before taking any other lock. Insert, removal, and growth take
the write lock, so a slot is never moved while a guard holds it.

The floor charges the vector's reserved capacity times the slot size,
including empty slots. A spilled version chain adds its buffer on the
heap. The per-id hash-map slot is no longer charged for these four
stores. Payloads, tombstones, and `used_bytes == accounted_bytes` are
unchanged (ADR-0202).

An id whose reserved slot array does not fit — the allocator's last
id is the one the tests mint — is one side-map entry, charged as a
hash-map slot plus the value. Ordinary monotonic ids never reach it.

## Alternatives considered

- **A free list of deleted ids.** Reuses slots, but ids are monotonic
  and are the element's identity. Rejected.
- **A lock per chunk, so different ids proceed together.** Finer
  concurrency, more machinery than this store's size justifies.
  Rejected.
- **Leave adjacency in the hash map.** It is the lookup on every hop.
  Rejected.
- **Shrink the vector when a slot is emptied.** Frees the tail only, and
  a hole in the middle still occupies its slot. Rejected; empty slots
  stay in the floor.

## Consequences

Create-only graphs drop the per-id hash-map slot and its empty buckets.
The reserved capacity rounds up to a power of two, so a store just past
a boundary carries spare slots. A deleted id keeps its slot until the
process drops the graph. Readers block on a writer for the whole store,
not one shard; guards stay short, as they were.
