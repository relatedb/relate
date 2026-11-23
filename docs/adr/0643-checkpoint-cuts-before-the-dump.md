---
type: adr
area: engine
claim: 38de81eeb1bc2e93cbc86f3996cd355f420a3699
date: 2026-11-21
deciders: the maintainers
name: 0643-checkpoint-cuts-before-the-dump
status: proposed
tags:
  - checkpoint
  - wal
  - perf
title: The checkpoint cuts before its dump; commits never wait for it
REFERENCES:
  - "[[adr/0020-segmented-wal-concurrent-checkpoint]]"
  - "[[adr/0038-lock-free-snapshot-pins]]"
  - "[[adr/0041-timed-checkpoint]]"
  - "[[adr/0094-preflight-wal-acceptance]]"
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0137-wal-follow]]"
  - "[[adr/0183-coalesce-checkpoint-shards]]"
  - "[[adr/0481-schema-as-files]]"
  - "[[adr/0644-checkpoint-backpressure-budget]]"
---

# 0643. The checkpoint cuts before its dump; commits never wait for it

## Context

A checkpoint's dump reads the whole graph and writes a snapshot; on a
four-million-node store it takes about 450 ms. Commits waited for it end
to end: the commit path entered `admit_publication`, which closed
checkpoint admission and then waited on the checkpoint's busy flag until
the dump and its recycle finished. Measured on a release build with a
12,000-statement autocommit run and one WAL-rotation checkpoint mid-run,
the first commit after the rotation threshold crossed took 457.6 ms
against 8.5 ms with no dump, and 457.4 ms of that was the admission
wait. The rotate and its fsync that the crossing statement pays are
about 7 ms, once per 10,000 commits.

The dump needed that exclusion for three reasons: it read the graph
without a publication guard or a snapshot pin, so a graph replacement or a
version GC could take state from under it; it read the catalog, theater,
id counters, ontology document and dirty id sets from live state across
its whole length, so a commit landing mid-dump could publish a schema
newer than its own rows — a schema mirror that disagrees with its
manifest; and it cleared the whole checkpoint-dirty tracking at its tail,
which is only sound when no commit has run since the dump began.

[ADR-0094](./0094-preflight-wal-acceptance.md) added a second reason: a writer drained a running
checkpoint before its first frame, so a checkpoint pinned before a
physical-uncertainty failure could not later publish state that
included a partial apply.

## Decision

**A checkpoint freezes a cut under the publication write guard, then
dumps from that cut. No commit waits for the dump.**

1. The cut is one section under the publication write guard — the same
   guard every commit group and every graph swap holds: rotate the open
   segment, read the WAL pin's LSN and commit timestamp
   ([ADR-0020](./0020-segmented-wal-concurrent-checkpoint.md)), clamp the dump horizon to the
   graph's commit timestamp, pin the graph's snapshot ([ADR-0038](./0038-lock-free-snapshot-pins.md)),
   and take the checkpoint image: the catalog projection, the theater,
   the id counters, the graph type and catalog DDL the schema mirror
   renders, the ontology document, and the dirty ids the dump covers.
   The guard is released before the dump starts.
2. The dump reads rows from the live graph at the cut's snapshot
   timestamp under the snapshot pin, and everything else from the image.
   The pin keeps GC from dropping the versions it reads and keeps the
   graph cell from being replaced while the dump holds a reference to it
   — [ADR-0137](./0137-wal-follow.md)'s pin contract, unchanged.
3. The dirty ids are taken at the cut instead of cleared at the tail: a
   commit that lands while the dump runs marks its ids again, so the
   next checkpoint covers them. A failed dump hands the taken ids back,
   so a retry covers them too.
4. The writer-facing drain and its checkpoint-failure fence are removed.
   A cut provably precedes a writer's record: it cannot observe a
   partial apply, and a checkpoint that fails cannot publish past a
   writer that started after it. A checkpoint's error reaches its own
   caller only.
5. What still waits: a commit for the cut's rotate barrier, a
   checkpoint for another checkpoint, and a commit group whose
   closed-segment backlog is over its budget
   ([ADR-0644](./0644-checkpoint-backpressure-budget.md)).
6. Only the final checkpoints compact: an explicit
   `Database::checkpoint()` and persist-on-drop coalesce clean
   neighbours into their rewritten units
   ([ADR-0183](./0183-coalesce-checkpoint-shards.md) Decision 2), while
   the automatic triggers — the timed walker and the WAL-rotation
   threshold — rewrite only the slots their dirty ids seed and publish
   every clean part verbatim. A mid-load checkpoint must not re-read
   every clean shard it has already written; the trigger, not the
   store, decides whether a dump coalesces.

## Alternatives considered

- **Keep the drain and shorten the dump.** Rejected: no chunking of a
  whole-graph dump removes the wait; the stall is the wait, not the
  dump.
- **Hold the publication read guard through the dump.** Rejected:
  commits take the write side, so they would wait for the dump on the
  lock instead of on the busy flag — the same stall under another name.
- **Copy or serialize the graph under the guard and dump from the
  copy.** Rejected: it moves the whole dump's cost into the exclusion
  window and adds a graph-sized copy.
- **Keep the tail clear and add a dirty generation.** Rejected: a
  second fact beside the id set, where taking the ids at the cut gives
  the same guarantee with one.
- **Let only schema-changing commits wait.** Rejected: classifying ops
  as schema-changing is a second vocabulary over the redo ops, and the
  group carrying one still stalls.
- **Drain only on physical-publication uncertainty.** Rejected: the cut
  already has the property the drain bought — its horizon precedes the
  writer's record — so the drain adds a wait and no guarantee.
- **Read live catalog mid-dump and re-check at the tail.** Rejected: a
  schema commit in the window still makes manifest and mirror disagree.

## Consequences

- A commit group's admission no longer touches checkpoint state; its
  guards are the publication write lock and the WAL mutex, and its worst
  case is one rotate barrier plus one concurrent commit's apply — unless
  the closed-segment backlog is over its budget, where it waits for the
  in-flight checkpoint ([ADR-0644](./0644-checkpoint-backpressure-budget.md)).
- The dump holds a snapshot pin for its length: a follow reload that
  overlaps it waits for the pin and can latch its documented follow
  error ([ADR-0137](./0137-wal-follow.md)) until the pin drops — the same bound a long read
  holds.
- A checkpoint's failure is visible to its own caller only; the
  writer-facing fence of [ADR-0094](./0094-preflight-wal-acceptance.md) survives only as WAL poisoning.
- The schema mirror is rendered from the image, so it always reflects
  the checkpoint it is written beside ([ADR-0481](./0481-schema-as-files.md)).
- Tests must show a commit that lands while a dump is held open neither
  waits for it nor joins its cut, that the next checkpoint dumps and
  recycles such a commit, and that a failed dump keeps its dirty ids.
