# ADR-0440: In-process follower/writer toggle

Status: Proposed
Claim: 59ea09d54dce44274d6404e2b94327e4db5e09e2

## Context

A client implementing embedded Relate wants to raise a database's
`--memory-limit` and `--concurrent-peak-token-limit` without
customer-visible query loss, including writes. Each increase needs more
process memory, which needs a new process at the new size. The client
creates that process as a WAL follower (ADR-0137), then swaps which
process is the writer. The client needs each process to change role in
place: CypherLite today only decides writable-or-not once, at open.

`OpenMode` (`cypherlite/cypherlite/src/lib.rs`) is `Writable`, `ReadOnly`,
or `Follow`, fixed for a handle's life. `Shared::new`
(`cypherlite/cypherlite/src/shared.rs`) resolves whether the checkpoint
timer, the durability timer, and the follow thread run from that mode
once, at construction. `ensure_writable` is the single admission gate
every commit path calls (`commit.rs::try_wal_commit` before it
enqueues into `Shared::group`, and `commit_replicated`) before a write
is admitted. `Shared::group` is a
`Mutex<GroupCommit>` with a plain `queue: Vec<CommitSlot>` and
`has_leader: bool` that only ever shrinks once the gate refuses new
admissions.

The writer lease is the exclusive `.cypherlite.writer` flock
(ADR-0136 §1), separate from the shared `.cypherlite.lock` identity marker
every opener holds for its process life. A `Writable` open's lease lives
inside the opaque `NativeLease` the storage layer's `lease()` returns and
is released only by closing the store. A `Follow` open never takes it
(`lease_readonly`).

A follower's in-memory state cannot simply be flipped writable in place.
`cypherlite-storage::Wal::open_readonly` opens the active segment's file
handle read-only; writing through it fails at the OS level, not just at a
software gate. `Wal::poll_follow` never updates `Wal::bytes` /
`Wal::commits` — only `lsn` and the follow cursor — so a follower's
checkpoint-size counters are stale relative to whatever the writer being
replaced has already appended. Both are symptoms of the same fact: a
follower's `Wal` was scanned once, read-only, at open or at the last
reload, and has not been rescanned since.

## Decision

1. **`Database::promote`** turns a handle opened `OpenMode::Follow` into
   the writer. Refused (`Error::Semantic`) for a `Writable` open (already
   the writer) and a `ReadOnly` open (can never become one). No-op if
   already the writer.

   Stops and joins the follow thread (`follow::spawn` now returns its
   `JoinHandle` instead of detaching it, so this can join it; the
   detach-on-drop behavior a paused test gate needs is unchanged —
   `Database::drop` still only signals, never joins, the follow thread).
   Takes the writer lease on the store's native root and WAL directory
   through a new, independent primitive,
   `cypherlite_storage::take_writer_lease`, so a second writer — another
   process holding a `Writable` open, or another handle this process
   already promoted — fails with `Error::Locked`, exactly as a second
   `Writable` open does. The lease comes first, so the catch-up below
   cannot race another promoter. With the lease held, `promote` applies
   any surviving WAL tail onto the loaded graph (`follow::catch_up`
   until the poll is idle). A checkpoint gap or a torn apply uses the
   follower's `FollowGapPolicy` (ADR-0441): `Yield` latches retryable
   `follow-gap` and does not load a snapshot; `Reload` keeps the
   in-place reload for embedded callers. On a clean tip, `Wal::open`
   rescans `bytes` and `commits` (migrate-on-open, orphan sweep, and
   torn-tail trim) and that writable handle replaces the read-only one
   only when its LSN equals the applied LSN. The loaded graph stays.
   Only then does `promote` flip `Shared::writable` and start the
   checkpoint and durability timers. A failure drops the lease and
   restarts the follow thread; nothing durable has changed. A handle
   that already needs reopen returns that error without taking the lease.

2. **`Database::demote`** hands the writer role back to a follower. No-op
   if already a follower. Refused for a `Writable` open that was never
   promoted — it has no lease this call could release, and ADR-0440
   deliberately never touches that path, to keep the bootstrap `Writable`
   open exactly as tested today.

   Sets a new `Shared::draining: AtomicBool`. `ensure_writable` checks it
   first: while set, every new write refuses with a new retryable
   `Error::ResourceLimit` named `Error::WRITER_DRAINING`
   (`cypherlite-core/src/error.rs`) — CypherLite HTTP already maps any
   retryable `ResourceLimit` to 429 (`surfaces/cypherlite-http/src/reply.rs`)
   and Bolt already maps it to the retryable
   `Neo.TransientError.General.MemoryPoolOutOfMemoryError`
   (`surfaces/cypherlite-bolt/src/failures.rs`); neither surface changes.
   A write already past that gate and enqueued in `Shared::group` finishes
   and is checkpointed, never cancelled. `demote` polls the queue
   (`GroupCommit::is_quiescent`, a new accessor beside the existing
   private fields) until it is empty and leaderless, bounded by a 30s
   timeout so a caller that never finishes a commit fails loudly instead
   of wedging the handoff forever. Checkpoints explicitly
   (`checkpoint::checkpoint_now`, called directly — not through
   `Database::checkpoint`, which itself calls `ensure_writable` and would
   now refuse on `draining`). Stops the checkpoint and durability timers,
   then reopens the WAL read-only at the published checkpoint
   (`open_follower_wal`) and keeps the loaded graph: that graph is already
   the snapshot just written, and a second load is what pushed a demoting
   writer past its process memory limit. `poll_follow` still needs the
   read-only handle, so the append WAL is swapped out before the lease is
   released. Then demote releases the writer lease, flips `writable`
   false, and restarts the follow thread. `draining` clears whether this
   succeeds or fails. On failure after the checkpoint, the timers restart
   and the lease is never released: the handle stays the writer, as if
   `demote` had not been called.

3. **`Shared`'s per-worker fields become `Mutex<Option<(Arc<ThreadStop>,
   JoinHandle<()>)>>`** (a new `shared::BgWorker` alias) for the
   checkpoint timer, the durability timer, and the follow thread (renamed
   `follow_ctl`), instead of `Option<Arc<ThreadStop>>` fields resolved
   once at construction. Three `start_checkpoint_timer` /
   `start_durability_timer` / `start_follow_thread` helpers (new module
   `resize.rs`) each install a worker only if the live state calls for
   one and none is already installed; the open path, `promote`, and
   `demote` all call the same three instead of each repeating the
   decision. `checkpoint_interval` moves from a separate
   `SharedOpenState` field into `OpenResolved` so `promote` can start the
   same timer a bootstrap writable open would have. `Shared::read_only` is
   renamed in spirit, not in place: it stays the fixed open-time mode
   (plus a new `opened_as_follow` fact `promote`/`demote` check);
   `Database::is_read_only` now reads the live `writable` flag instead.

4. **The error envelope's `metrics` object gains `retryable`** when the
   underlying error is a `ResourceLimit` that already attaches a receipt —
   today, solely `concurrent-peak-token-limit` refusals
   (`cypherlite-storage`/`cypherlite/src/token_budget.rs`'s
   `resource_limit_with_receipt`). `surfaces/cypherlite-http/src/reply.rs`'s
   `run_error` folds both `concurrent-peak-token-limit` and any retryable
   `ResourceLimit` into the same HTTP 429 with the same message text
   (`e.to_string()` never includes `retryable`); a host counting retryable
   and non-retryable peak-token refusals separately has no other way to
   see the bit. Additive JSON field; an older reader ignores it. Bolt is
   unaffected.
5. **One writer lease.** `.cypherlite.writer` is held as a `WriterLease`
   on the open handle. A writable native open moves that lease out of
   the VFS lease onto `Shared` before the open returns. `promote` takes
   the same value. `demote` drops it, on a handle that opened writable
   and on one that opened `Follow`. A writable open's `promote` is a
   no-op while it still holds the lease, and works again after `demote`.
   A frozen `ReadOnly` open still cannot become the writer. A client
   implementing embedded Relate opens writable when the store files are
   absent and `Follow` plus `promote` when they exist; `Error::Locked`
   leaves that process a follower.

## Alternatives considered

- **Flip `Shared::resolved.read_only` and reuse the follower's existing
  `Wal`/graph in place.** Rejected. The follower's `Wal` file handle was
  opened read-only at the OS level (`VfsOpen::read_only()`); no software
  flag makes a read-only file descriptor writable. Its `bytes`/`commits`
  checkpoint-size counters are also stale, because `poll_follow` never
  updates them.
- **A bespoke storage-layer method that reopens the active segment for
  append in place, patching the counters by hand.** Rejected. `Wal::open`
  already rescans those counters. Promote calls that scan and keeps the
  loaded graph; it does not invent a second set of counters.
- **Leave the writable open's lease inside `NativeLease`.** Rejected.
  `demote` could not release it without closing the store, so the process
  that created the database could never hand the writer role off. The lease
  is the same `WriterLease` either way; a writable open moves it onto
  `Shared` instead of keeping a second copy in the VFS.
- **A second writer-lease flavor that blocks until free, instead of
  failing with `Error::Locked`.** Rejected. The two-handoff sequence
  already orders the lease release (old writer's `demote`) before the new
  acquisition (new writer's `promote`) in the client; a blocking lease
  would hide a sequencing bug as a hang instead of the typed error a
  second `Writable` open already produces.

## Consequences

- `cypherlite serve`'s `--follow` flag gains no new CLI surface in this
  change; `promote`/`demote` are a library API for a client implementing
  embedded Relate to call, not a `cypherlite` CLI verb.
- A direct `Writable` open stays the way to create a store, and that
  handle can now `demote` and `promote` with the same lease a follower
  uses. `cypherlite serve`/`query` and other embedders are unchanged
  until they call those methods.
- A promoted follower's `Database::is_read_only()` now reflects the live
  writable fact, not the open-time mode; `Database::is_follower()` is new
  and reports the fixed open-time fact the old method used to.
- `follow::spawn` returns its `JoinHandle` instead of detaching it;
  `Database::drop`'s existing signal-only (never join) handling of the
  follow thread is unchanged — only `promote` joins it, and only because
  it is about to replace the `Wal` that thread was tailing.
- A client that already reads `metrics.peakTokens`/`metrics.tokenSchedule`
  on a 429/507 now also sees `metrics.retryable` on a peak-token refusal;
  `docs/api/http/spec.md`'s metrics-object description gains the field.

## Later references

ADR-0501 removes `metrics.tokenSchedule`. `metrics.retryable` stays. The
peak this consequence called `metrics.peakTokens` is `metrics.peakLogicalBytes`.
