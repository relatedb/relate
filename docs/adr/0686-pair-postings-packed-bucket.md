---
type: adr
area: engine
claim: 8dd8a0b969f8512061f9b20ead9838c97ed4fa23
name: 0686-pair-postings-packed-bucket
status: proposed
tags:
  - executor
  - index
  - memory
  - perf
title: The lazy equality pair stores one word per bucket and presizes its build
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0063-lazy-string-index-postings]]"
  - "[[adr/0185-relationship-equality-postings]]"
  - "[[adr/0355-fxhash-core-maps-and-interned-domain-reads]]"
  - "[[adr/0614-equality-match-lazy-postings-serve]]"
---

# 0686: The lazy equality pair stores one word per bucket and presizes its build

## Context

[ADR-0614](./0614-equality-match-lazy-postings-serve.md) made the first equality `MATCH` build a `(label, key)` pair
and keep it. The pair maps each distinct property value to the ascending
ids that carry it; a bucket is `One(id)` inline or `Many(Vec<NodeId>)`,
and integer-equivalent values key an `i64` table beside the general
`GroupKey` table. That layout already removed most of the pair's cost
(a hash set per value, about 213 B per node, became about 43 B).

What is left, measured with a counting allocator on 200,000 unique keys
after `shrink_to_fit` (heap bytes per node, table plus keys):

| key | bytes per node |
| --- | --- |
| unique integer | 43.3 |
| unique float | 127.1 |
| unique string, 12 and 36 chars | 159.1 and 183.1 |

An integer entry is a 32-byte `(i64, IdBag)` plus hashbrown's control
byte, in a power-of-two bucket array at 44–87% load. The id the entry
exists to hold is 8 of those bytes. The `IdBag` enum is 24 bytes only
because `Many` carries a `Vec` header that a unique key never uses.

The build also peaks above its steady state. The table grows by
doubling while the build posts, so each rehash holds the old and new
bucket arrays at once. Measured on unique integer keys:

| nodes | build peak | steady state |
| --- | --- | --- |
| 46,000 | 3.24 MB | 2.16 MB |
| 200,000 | 12.98 MB | 8.65 MB |

The build already knows its input size: `pair_candidate_ids` returns the
label's ids as a `Vec` before the first post. The store holds no
per-key distinct count, so the candidate count is an upper bound on the
number of buckets, exact for a unique key.

## Decision

1. **One word per bucket.** A bucket is a `u64` slot. A non-negative id
   with no company is stored directly. A bucket of two or more ids has
   bit 63 set and the remaining bits index a slab of `Vec<NodeId>` held
   by the pair; the slab recycles the slot of a bucket that demotes to
   one id or empties. A persisted node id is non-negative and below
   `i64::MAX` (`validate_persisted_id`), so bit 63 is free; a virtual
   (negative) id has no store entry and is never posted. The slot
   constructor states that invariant with `expect`, not a fallback. The
   integer table entry becomes 16 bytes. The bucket protocol (`post`,
   `unpost`, ascending iteration, demotion at one id, an emptied bucket
   leaving the table, `shrink_to_fit`) and every read signature are
   unchanged; `bucket()` still returns a sorted `&[NodeId]`, borrowing a
   single inline id through the slot itself.
2. **Presize the build when the key looks unique.** The first posts of
   a build count distinct buckets over a fixed prefix of the candidates.
   When the prefix is nearly all distinct, the table that took the first
   key reserves the remaining candidate count once; otherwise it grows
   as today. Only the table that received the first key reserves, so a
   mixed pair never reserves the 96-byte-entry general table for
   integers. The existing `shrink_to_fit` after the build trims any
   over-reserve.
3. **Seeded hashing and the key tables stay.** Both tables keep the
   seeded policy of [ADR-0355](./0355-fxhash-core-maps-and-interned-domain-reads.md); no key is dropped in favour of a
   hash tag, so `keys_sorted` and the aggregate walks still read real
   keys.

## Alternatives considered

- **Reserve the candidate count unconditionally.** A low-cardinality key
  over 10 million nodes would reserve 330 MB of bucket array for five
  values, resident control bytes included, only to shrink it afterwards.
  Rejected for the sampled form above.
- **A 16-byte enum (`Box<[NodeId]>` for `Many`).** Reaches 16 bytes
  without an encoding, but the entry stays 24 bytes. Rejected: half the
  gain of the one-word slot for a similar change.
- **Store only a hash tag and the id, verified through the node.** About
  10–20 B per string key, but unposting needs the old value's hash, and
  `keys_sorted` and the aggregate walks need real keys; a tag collision
  would corrupt a count. Rejected here; a separate decision if string
  keys prove to dominate.
- **A dense direct table, a `u32` slot per value when the key span is
  within about twice the entry count.** About 4 B per node for dense
  integer keys. Deferred, not rejected: it adds a third representation
  and a migration rule, and the memory-shape ids are dense enough to
  flatter it. Measured and decided separately.
- **Roaring or delta-coded buckets for large `Many` lists.** No shape
  with a low-cardinality key exists to show a gain. Deferred until one
  does.

## Consequences

- A unique integer key costs 22–24 B per node instead of 43 B, measured
  with a counting allocator: 200,000 keys settle at 4.46 MB (was
  8.65 MB) and peak at 4.49 MB (was 12.98 MB); the ring shape's 46,000
  keys settle at 1.11 MB (was 2.16 MB) and peak at 1.15 MB (was
  3.24 MB). The build peak equals the steady state for a unique key.
- A unique 12-character string key falls from 159 to 138 B per node. A
  five-value key over 200,000 nodes reserves nothing and costs 8 B per
  node.
- The saving is about 21 B per unique integer node, about 1 MB at the
  memory-shape ring size. That is below the lane's one-to-two-megabyte
  run-to-run noise, so the lane cannot resolve it; the heap
  measurement is the evidence.
- A `Many` bucket costs a slab slot (24 bytes) beside a 16-byte entry,
  8 bytes more than today's inline `Vec`. Such buckets are few by
  construction; the id lists they hold dominate their cost.
- Float, string and other `GroupKey` buckets keep their 96-byte entry
  until a later decision shrinks the key; this ADR does not change their
  per-node cost.
- The pair stays outside the [ADR-0042](./0042-graph-memory-limit.md) logical charge, as under
  [ADR-0614](./0614-equality-match-lazy-postings-serve.md); no loadtest envelope moves.
- The slab is one more piece of pair state that the bucket protocol owns
  alone, so no other module reaches it.
