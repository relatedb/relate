# 0136. Frozen read-only open beside a native writer

- **Status**: Proposed
- **Date**: 2026-09-29
- **Deciders**: CypherLite maintainers

## Context

ADR-0097 takes a process-lifetime exclusive `flock` on each `.cypherlite.lock`.
A second opener, including one that only wants to read, fails with
`Error::Locked`. There is no read-only open: a handle checkpoints on close,
migrates older stores on open (ADR-0127), and trims a torn WAL tail
(`Wal::open`). Those are writes.

Callers that must not mutate a database (backups, agents, a local reader
next to a writer) need an open that loads the snapshot and WAL once and
then leaves the files alone. The same native directory has to stay open
for the writer. `fs4` 0.8.4 only offers whole-file locks, so the identity
marker cannot be exclusive for the writer and shared for readers at once.

`.cl` bundles rewrite pages inside one host file (ADR-0074). Immutable
shard publication (ADR-0089) makes a native directory readable while a
checkpoint publishes the next generation. The bundle does not.

## Decision

1. **Identity lock is shared; the writer lease is exclusive.** Every live
   native opener takes `LOCK_SH` on the existing root `.cypherlite.lock` and,
   when the WAL directory is distinct, on its `.cypherlite.lock`, for the
   life of the process. A writable open also creates and exclusively locks
   a stable `.cypherlite.writer` beside each of those markers. The writer
   file is mode `0600` when newly created, is never unlinked on close, and
   is not a database identity: nested scans still key only on
   `.cypherlite.lock` and `.cl`. A second writable open fails that lease
   with `Error::Locked`. Reset and offline adoption keep `LOCK_EX` on
   `.cypherlite.lock`, which conflicts with every shared holder. On Windows
   a shared `LockFileEx` rejects `WriteFile` on the owning handle, so a
   first open writes the role bytes under a brief exclusive lock and then
   returns to `LOCK_SH`. Identity checks open the marker for file
   attributes only, because a second read of a locked range returns
   `ERROR_LOCK_VIOLATION`. A read-only open accepts a root marker of
   `ROOT` or an empty marker on a directory that already has root
   evidence and rejects a WAL role or any other bytes, and after the
   Windows upgrade back to `LOCK_SH` the opener checks identity again
   and returns `Error::Storage` if the directory changed.
2. **Read-only open does not create or repair.** `OpenMode::ReadOnly`
   and CLI `--read-only` open an existing native directory or `.cl` file.
   A missing store is `Error::Storage` and creates nothing: no directory,
   marker, segment, or writer lease. The native reader opens markers
   read-only and takes only the shared identity locks. A `.cl` reader
   takes `LOCK_SH` on the existing host file; the writer still holds
   `LOCK_EX` on that file, so a bundle reader and a bundle writer do not
   run together. `:memory:` with `read_only` is a write fence and takes
   no file lock.
3. **The loaded graph is a snapshot.** Read-only open loads the manifest
   and shards, replays complete WAL frames, and stops at a torn tail
   without truncating it. If the manifest bytes change during the load,
   or a read fails because a checkpoint removed a file mid-load, the open
   retries a bounded number of times and then returns `Error::Storage`.
   After a stable load the process does not read the files again.
   Migrate-on-open, orphan sweep, checkpoint timers, the durability
   timer, and persist-on-drop do not run. Legacy snapshots are served
   through the legacy reader and are not rewritten (ADR-0127).
4. **Mutations fail.** `try_wal_commit`, `checkpoint`, and
   `apply_replicated` return `Error::Semantic` (`database is read-only`).
   Reads stay available.

## Alternatives considered

- **Keep the exclusive identity lock and only refuse writes.** Rejected:
  a reader cannot open the directory while the writer is alive, so a
  frozen replica on the same files is impossible.
- **Byte-range locks on `.cypherlite.lock`.** Rejected for this change:
  `fs4` 0.8.4 is whole-file `flock` / `LockFileEx`, and a second file
  keeps the existing identity marker's role bytes untouched.
- **Let a read-only `.cl` open share the host file with a writer.**
  Rejected: bundle pages are rewritten in place, unlike immutable native
  shards.

## Consequences

- Two writable opens of one native database still conflict. A writable
  open and one or more read-only opens share the native directory. Reset
  fails with `Error::Locked` while any of them is open.
- A read-only handle does not observe commits that land after its load.
  Reopen to refresh. `--follow` is a separate decision.
- Bolt (`--read-only`), Python (`read_only=False`), the C ABI
  (`cypherlite_database_open_read_only`), JavaScript
  (`Database.open(uri, { readOnly: true })`), Java (`Database.openReadOnly`),
  Go (`OpenReadOnly`), and WASM (`readOnly` on `new` and `open`) expose the
  same open. The default on each stays writable. `--follow` stays on the CLI
  and Rust `OpenMode::Follow`.
- `.cypherlite.writer` appears in native root and distinct WAL directories
  created or opened writable after this change. Copies of a database
  include it; it holds no role bytes.

## Later references

ADR-0480 supersedes the flag *spelling* only: the CLI's read-only open is
reached as `--read-only` / `--read-only=snapshot` (bare form unchanged,
the frozen-snapshot behavior this ADR defines is exactly the default).
Nothing else in this decision changes; the internal `OpenMode::ReadOnly`
name stays.
