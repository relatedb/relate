---
type: adr
area: engine
date: 2026-09-18
deciders: CypherLite / MemoryDB merge
name: 0063-lazy-string-index-postings
status: accepted
tags:
  - index
title: Lazy prefix/trigram postings with incremental dirty catch-up
REFERENCES:
  - "[[adr/0018-concurrent-snapshot-reads]]"
  - "[[adr/0026-eager-residual-match-predicates]]"
  - "[[adr/0030-string-prefix-trigram-indexes]]"
  - "[[adr/0040-create-drop-index]]"
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0207-packed-text-trigrams]]"
---

# 0063. Lazy prefix/trigram postings with incremental dirty catch-up

## Context

[ADR-0030](./0030-string-prefix-trigram-indexes.md) maintains prefix (`STARTS WITH`) and trigram (`CONTAINS`) postings
**eagerly**: every node create / SET / label change gram-splits every string
property (~36 heap-allocated grams × label variants per value) and inserts
them into the always-on maps. Node 51 (`51-bulk-create-profile`, memdb-merge)
measured the cost on the reference host (M3 Max, release, 1M nodes + 1M rels
single-statement `UNWIND … CREATE (p:Person {name:"Tomas "+id, …})-[:KNOWS]->(p)`):

- commit-phase `apply_redo → install_node` string-index maintenance was
  ~86% of wall time: **45.8–55.6 s** per 1M load (`:memory:` and file-backed
  alike), **17.8–18.9 GB** peak RSS;
- a pre-string-machinery binary runs the same load in ~4.7 s, and the same
  shape without the string property in ~10.4 s.

The eager design also forces [ADR-0042](./0042-graph-memory-limit.md)'s logical-size accounting to enumerate
grams on every write (charge = f(distinct grams)), so even accounting-only
paths paid the split.

The fraud `search_customer` use case
(`MATCH (c:Customer) WHERE c.name CONTAINS $q`) depends on these postings:
deleting them would regress it to an O(label) scan. The postings are
**superset accelerators**: every seek re-verifies snapshot visibility and the
actual predicate (`graph.rs` residual filter, [ADR-0018](./0018-concurrent-snapshot-reads.md) / [ADR-0026](./0026-eager-residual-match-predicates.md)), so stale
or missing-soon entries are a performance question, never a correctness one —
as long as the postings remain a superset of the true matches visible at any
live snapshot.

## Decision

Make prefix/trigram postings **lazy and incremental**:

- Postings live per interned `(label_id, key_id)` pair
  (`Graph::string_pairs`). A pair is materialized only when a `CONTAINS` /
  `STARTS WITH` seek first targets it: build scans the label index (or all
  nodes for the `*` bucket) and indexes **every version** of each candidate,
  exactly the union eager maintenance would have posted since GC.
- The write path never grams. A mutation that can add a string match
  (create with a string property, `SET` of a string value, `SET` of a label
  on a node with string properties) just inserts the node id into the
  pair's `dirty` set — and only if the pair is already built, so unsought
  pairs cost one missed hash probe. Overwrites away from the needle, label
  removals, and deletes do not even dirty: they leave stale postings, which
  the residual re-verification already filters.
- The next seek of a built pair **drains** its dirty set: re-post the
  drained nodes' current version-chain values (idempotent set inserts),
  then answer from postings. Steady-state mixed workloads (fraud
  `onboard_customer` → `search_customer`) pay O(dirty) catch-up, not an
  O(label) rebuild and never a label scan.
- Ordering rule: dirty-mark **after** applying the version, before
  publishing the commit. A seek whose snapshot includes the write
  happens-after the dirty insert (pin acquire → pair lock), so no match is
  ever missed; a concurrent in-flight write is either invisible at the
  seek's snapshot or found in the dirty set.
- `rebuild_indexes` (open / replay / abort) just drops the pairs; they
  rebuild on demand. GC prunes dead-version postings per pair as before.
- [ADR-0042](./0042-graph-memory-limit.md) accounting stays a deterministic formula of the live node, but
  the trigram term no longer enumerates grams: it charges 1–3-gram **window
  positions** (an upper bound of distinct grams) per `(label, key)`. This
  keeps `used` identical between the incremental counter and the full live
  walk, keeps reads from moving `used`, and keeps the charge independent of
  which pairs happen to be materialized. `logical_used_bytes` stays within
 the [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) gate band of the frozen loadtest baselines.

Secondary, from re-profiling after the primary fix (sampled `apply_redo`):

- Label and equality postings key on interned `(label_id, key_id)` ids
  instead of cloned `String`s, and `install_node` / `install_rel` index and
  charge straight from the `LiveNode` / `LiveRel` parts they already hold —
  no `Node` re-materialization, no endpoint materialization for the
  existence check. The accounting core is shared between the `Node` and
  `LiveNode` forms, so the incremental counter and the live-walk oracle
  cannot drift apart.
- `Wal::append_record` borrows the redo ops (`&[RedoOp]`) instead of taking
  an owned clone; the in-memory WAL backend retains its own copy, the file
  backend only encodes. Identical on-disk bytes.
- `flush_group` drops the second `slot.ops.clone()`: the accepted slots
  themselves answer the intra-batch conflict check and the `commit_ts`
  chain, replacing the `unapplied` shadow list.

Measured on the reference host (release, node-51 shape):

| Stage | 100k `:memory:` | 1M `:memory:` | 1M file |
| --- | --- | --- | --- |
| before | 3.51 s (apply 2.92) | 45.84 s (apply 39.54) | 46.54 s (apply 39.71) |
| after | 0.82 s (apply 0.29) | **9.85 s (apply 4.22)** | **9.43 s** |

Peak RSS at 1M: 18.9 GB → 13.6 GB `:memory:`, 15.7 GB → 10.6 GB
file-backed. Graph contents after the 100k load are pinned identical by
`bulk_create_100k_lands_identical_contents`; TCK is unchanged
(3896 pass / 1 documented skip of 3897, allowlist byte-identical).

## Alternatives considered

- **Keep postings eager, optimize the grams** (interned gram ids, faster
  hasher) — rejected: the commit phase would still pay O(labels × grams)
  per string property on every write; bulk loads that never run `CONTAINS`
  keep buying postings they never use (the 86% tax shrinks but stays).
- **Delete the string indexes, scan the label** — rejected: fraud
 `search_customer` regresses to O(label) per query ([ADR-0030](./0030-string-prefix-trigram-indexes.md) exists
 precisely because that dominated the mix); the [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) loadtest gate
  would fail.
- **Full rebuild of a pair on any dirty write** — rejected: with ~1 onboard
  per search in the fraud mix, every seek would pay an O(label) rebuild;
  measured-friendly incremental drain costs O(dirty) instead.
- **Charge only materialized pairs in [ADR-0042](./0042-graph-memory-limit.md) `used`** — rejected: reads
  would move the write budget (a first `CONTAINS` could push a full
  database over its limit), and the counter would depend on seek history.
  The logical budget charges the implied always-on formula either way.
- **Global dirty generation instead of per-pair dirty sets** — rejected: a
  string write anywhere would invalidate every built pair; per-pair sets
  confine catch-up to what the write actually touched, and unbuilt pairs
  stay free.

## Consequences

- Bulk `CREATE` of 1M nodes + 1M rels with string properties drops from
  ~46 s to ~9 s on the reference host; load paths that never seek strings
  never pay for string indexes at all.
- First `CONTAINS` / `STARTS WITH` seek of a `(label, key)` pair pays a
  one-time O(label) build; later seeks pay O(|dirty since last seek|) plus
  the postings lookup. Writers briefly block on a pair that is mid-build.
- [ADR-0030](./0030-string-prefix-trigram-indexes.md) gains a *Later references* note: postings are lazy, the access
 methods and superset semantics are unchanged. [ADR-0040](./0040-create-drop-index.md)'s "postings stay
 always-on" now reads as "always available, built on demand". [ADR-0042](./0042-graph-memory-limit.md)'s
  budget charges the implied postings by formula, not by materialization.
- The in-memory WAL retains one op copy (as before); the file WAL path
  encodes borrowed ops. Group-commit batching, fsync, and conflict
  semantics are unchanged — only the redundant clones are gone.

## Later references

[ADR-0207](./0207-packed-text-trigrams.md): a `TEXT` index stores only width-3 windows, packed as `u64`
keys with sorted posting lists. The budget term is one posting per
window (`char_len - 2`), not the 1–3-gram window sum described above.
Needles shorter than three characters scan.
