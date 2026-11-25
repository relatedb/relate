---
type: adr
area: engine
claim: 6054345655d9af7854e869206f1f449582374223
date: 2026-11-25
deciders: Relate maintainers
name: 0684-single-database-detach-is-a-full-checkpoint
status: proposed
tags:
  - checkpoint
  - storage
  - wal
title: A single-database detach is a full checkpoint, then lease release
REFERENCES:
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0137-wal-follow]]"
  - "[[adr/0440-follower-writer-toggle]]"
  - "[[adr/0646-checkpoint-nothing-to-publish-writes-nothing]]"
---

# 0684: A single-database detach is a full checkpoint, then lease release

## Context

External consumers that run on databases in a worker pool give each worker one
database. A worker is released by a detach, and the next worker attaches the
database by opening it as a follower and promoting it
([ADR-0440](./0440-follower-writer-toggle.md)).

One design for the detach is WAL-only: `fsync` the WAL, then drop the database
without a checkpoint, so the next open replays the tail. Measurement rejected it
for two reasons:

- A WAL tail adds about 35 to 40 ms per MiB to every resume.
- Under `EverySecond` durability, a commit is acknowledged once it is applied,
  and the WAL becomes durable on the one-second timer. A detach that only
  `fsync`s the WAL files does not flush what the engine still holds. Across 20
  measured detaches, the last acknowledged batch was lost in 18.

The engine already has the pieces. `Database::demote()` waits for in-flight
commits, writes a full checkpoint, swaps to a read-only WAL, and releases the
writer lease, in that order. It then clears the persist-on-drop flag, so the
drop that follows writes nothing. A plain writer drop checkpoints, then
releases the lease when the shared state is freed. A follower owns no lease and
no acknowledged commit, and `checkpoint_if_writable()` writes nothing for it
([ADR-0136](./0136-read-only-open.md)).

## Decision

1. **A single-database detach is a full checkpoint, then lease release.** For a
   writer, the detach is `demote()` followed by the drop. For a follower, it is
   the drop alone: everything the follower holds is already in the writer's
   checkpoint and WAL ([ADR-0137](./0137-wal-follow.md)). For a writer, the
   checkpoint completes before the writer lease and the flock are released.
2. **No WAL-only close is added.** `leak_without_checkpoint` stays a test hook.
   Consumers that run on pooled databases do not use it.
3. **The detach writes one full checkpoint.** The drop after `demote()` does not
   write a second one, and a follower drop writes none. Tests pin this, so a
   later change to the demote or drop path cannot add a second dump or move the
   lease release ahead of the checkpoint.
4. **No new entry point.** The demote-then-drop path already gives the order
   above. A `detach()` function would only restate it.

## Alternatives considered

- **`wal.sync()` then drop without a checkpoint.** Rejected. It keeps the WAL
  tail, and under `EverySecond` the barrier does not cover the last
  acknowledged commits. The loss is measured.
- **A checkpoint only when the tail is large.** Rejected. The consumer cannot
  tell from outside how long the replay will take, and a tail under the
  threshold still costs replay time on every resume.
- **A `detach()` entry point on `Database`.** Rejected. It would wrap
  `demote()` and the drop with no new behavior, and it would add a public
  function to keep in step with both.
- **Checkpoint on a follower.** Rejected. A follower holds no writer lease, so
  it cannot publish a snapshot, and a second publisher would race the writer.

## Consequences

- Detach time scales with the graph and the dirty data. A snapshot detach
  measured 10 to 40 times a WAL-only detach on a shared network filesystem.
  Consumers set their detach budget from a measured snapshot baseline.
- The `DurabilityMode::EverySecond` acknowledgement is safe for pooled
  databases, because the detach checkpoint covers every acknowledged commit.
- The engine change is tests only: the writer and follower detach orders are
  pinned, and the doc comment on `demote()` states the contract.
- Open-time probing for a shared network filesystem is a separate decision. It
  waits for a reproduced failure: one `ENOENT` was seen on a warm promote, with
  no reproduction yet.
