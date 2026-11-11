# 0156. Clone a native directory without writing it

- **Status**: Proposed
- **Date**: 2026-10-02
- **Deciders**: CypherLite maintainers

## Context

A copy of a database has to be safe to take while the writable process is
committing. `Database::checkpoint` dumps the caller's own store and then
recycles that store's WAL, so it is the wrong tool: it writes the database
being copied. ADR-0136 already loads a native directory beside the writer.
The loaded graph is one consistent cut: the snapshot plus complete WAL
frames, with a torn tail left untrimmed. ADR-0137's follower keeps moving
past that cut, which is not a copy.

A `.cl` bundle rewrites pages in the one host file the writer locks
exclusively (ADR-0074, ADR-0136). There is no generation a second process
can read.

## Decision

1. **`Database::clone_directory` copies a native directory.** The CLI
   command is `cypherlite clone [--wal-dir DIR] SOURCE DEST` (ADR-0069:
   an exit-after verb is a positional command, not a flag). `:memory:`
   and `.cl` are refused before any lock is taken, for both the source
   and the destination. The source is always a frozen read-only open,
   never a follower. `CloneOptions` is `wal_dir` only, and it applies
   only to that source open. The destination is written in the codec
   that open observed. Clone does not take a codec.
2. **The cut is the LSN visible when the read-only load finishes.** The
   clone does not tail later commits. The source is not appended,
   checkpointed, migrated, trimmed, or recycled.
3. **The destination is a new store.** It must not already exist and must
   not be the source directory. The clone publishes `checkpoint_at` into
   that directory at the loaded LSN and `commit_ts`, using the source
   codec, then creates an empty WAL segment there so a later read-only
   open succeeds. The next commit on the clone is `lsn + 1`. The
   destination WAL is always `<dest>/wal`, even when the source WAL is
   external.
4. **A failed clone removes the destination it created**, while it still
   holds the destination writer lease. A failure before the destination
   exists leaves nothing behind.

## Alternatives considered

- **Call `Database::checkpoint` on the source, then copy files.** Rejected:
  the checkpoint writes and recycles the live database.
- **`--follow` until a checkpoint, then copy.** Rejected: follow has no
  single cut, and it can apply a frame the writer has not synced.
- **Clone a `.cl` bundle beside its writer.** Rejected: the bundle has no
  immutable generation to read (ADR-0136).

## Consequences

- `cypherlite clone ./mydata ./my-clone` is a consistent copy of a native
  directory and does not take the writer lease on `./mydata`.
- The clone diverges from the source at that cut. A commit that lands
  after the load is absent. Under relaxed durability, the cut can include
  a complete frame a later power loss would drop on the source, because
  no durable-LSN file is published (ADR-0137).
- `.cl` backups stay "checkpoint, then copy the file."
