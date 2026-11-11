# 0137. Follow a native WAL and reload across a checkpoint gap

- **Status**: Proposed
- **Date**: 2026-09-29
- **Deciders**: CypherLite maintainers

## Context

ADR-0136 opens a frozen snapshot. A reader that should see later commits
without taking a second copy of the files has to watch the WAL the writer
appends. ADR-0045 rejected an uncoordinated tail that pins WAL recycle:
a dead consumer would hold closed segments forever. ADR-0574 later
removes the slots; follow still does not pin recycle. Checkpoint deletes
segments whose `max_lsn` is covered by the published manifest
(ADR-0020). A follower that has not applied those records loses them.

The query engine already applies redo beside readers (ADR-0013). Files
are a recovery log plus a checkpoint; the database is the in-memory
graph. Catch-up is applying frames into that graph, and a gap is a new
load of the manifest, not a page read.

`:memory:` has no shared files. A `.cl` bundle is not safe to read while
its writer rewrites pages (ADR-0136).

## Decision

1. **`--follow` is read-only plus a tail.** `OpenMode::Follow` is the
   read-only tail. CLI `--follow` selects it. The mode is refused for
   `:memory:` and `.cl` with `Error::Semantic`. The handle still never
   appends, checkpoints, migrates, trims, or sweeps. A mid-batch apply
   failure reloads the graph, and if
   that reload fails the follower stays failed until the process reopens
   it; a snapshot pin that blocks reload past a short bound is reported
   as a follow error until the pin drops and the swap completes.
2. **Poll the WAL; do not pin recycle.** A thread on the follower lists
   native segments and reads the active segment length. It applies only
   complete CRC-valid frames with `lsn` greater than the applied LSN,
   via `Graph::apply_redo`, and it does not append. There is no inotify.
   Statement snapshots keep the pins they already hold.
3. **Reload only across a gap.** Caught up means the applied LSN is at
   least the manifest `checkpoint_lsn` before recycled segments
   disappear. The follower keeps tailing whatever WAL remains. If a
   segment the follower still needs is gone, or the active segment
   shrinks under the cursor (a crash trim), the follower stops applying
   and loads the current manifest, shards, and surviving WAL beside
   readers still using the installed graph. The exclusive publication
   lock covers only the swap: it waits out snapshot pins, which outlive
   `read_guard`, then installs the new graph. Queries that start
   afterward see the reloaded snapshot. A torn tail is not truncated.
4. **No durable-LSN file.** A follower may apply a frame the writer has
   not synced yet. A later trim of that tail is a gap and takes the same
   reload as a checkpoint the follower missed.

## Alternatives considered

- **Register a CDC slot from the follower.** Rejected: the slot write
  breaks read-only, and a dead follower would hold WAL recycle
  (ADR-0045). ADR-0574 removes the slot API entirely.
- **Reload on every checkpoint.** Rejected: a follower that already
  applied the recycled LSN has nothing to rebuild.
- **Publish a durable LSN before a frame may be applied.** Rejected for
  this milestone: it adds a barrier on the writer. Divergence after a
  crash is repaired by the trim-triggered reload.

## Consequences

- Followers share a native directory with one writer and do not share
  the in-memory graph. Each follower is a full graph.
- A paused or slow follower can lag until the next checkpoint, then
  pays a full reload instead of blocking the writer's recycle.
- Freshness is the last successful poll or reload, not the writer's
  latest acknowledged commit.

## Later references

- ADR-0441: a host may select `FollowGapPolicy::Yield` so a gap latches
  `follow-gap` instead of loading a second graph. The default remains the
  reload described here.
