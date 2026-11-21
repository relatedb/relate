//! Checkpoint machinery: pin the WAL, dump a snapshot, recycle segments
//! (ADR-0020), the background timer (ADR-0041), and persist-on-drop.

use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard, RwLockWriteGuard, Weak};
use std::time::Duration;

use relate_storage::{checkpoint_at, Wal};

use crate::commit::format_commit_timing;
use crate::{DurabilityMode, Error, Result, Shared, ThreadStop};

#[derive(Default)]
pub(crate) struct CheckpointState {
    busy: bool,
    blocked: bool,
    last_error: Option<Error>,
}

/// Excludes readers/writers, closes checkpoint admission, then waits for a
/// previously admitted dump AND its recycle to finish. No WAL lock is held.
pub(crate) struct Publication<'a> {
    shared: &'a Shared,
    _write: RwLockWriteGuard<'a, ()>,
}

pub(crate) fn begin_publication(shared: &Shared) -> Result<Publication<'_>> {
    let write = shared.publication.write()?;
    shared.check_healthy()?;
    admit_publication(shared, write)
}

/// Publication lock for a follow reload. The reload thread may already have
/// recorded a pin-stall failure in the follow health state; that must not
/// abort the swap that clears it, so this gate checks WAL poisoning only.
pub(crate) fn begin_reload_swap(shared: &Shared) -> Result<Publication<'_>> {
    let write = shared.publication.write()?;
    shared.check_poisoned()?;
    admit_publication(shared, write)
}

/// Close checkpoint admission and wait for an admitted dump and its recycle
/// to finish. The caller holds the publication write guard and has already
/// run its health gate.
fn admit_publication<'a>(
    shared: &'a Shared,
    write: RwLockWriteGuard<'a, ()>,
) -> Result<Publication<'a>> {
    // `Publication`'s Drop owns the admission release, so build it before
    // the last_error check and return it through both arms: on the error
    // path the state guard unlocks first, then the publication clears
    // `blocked` and notifies.
    let publication = Publication {
        shared,
        _write: write,
    };
    let mut state = shared.checkpoint_busy.lock()?;
    state.blocked = true;
    while state.busy {
        state = shared.checkpoint_cv.wait(state)?;
    }
    match state.last_error.take() {
        Some(error) => Err(error),
        None => Ok(publication),
    }
}

impl Drop for Publication<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.checkpoint_busy.lock() {
            state.blocked = false;
            self.shared.checkpoint_cv.notify_all();
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // `Database::drop` already called `stop_background`, which joined
        // the timers; this signal is only for a `Shared` that never went
        // through that path (a construction failure before the handle
        // existed). Signaling again here is harmless either way.
        self.signal_background();
        if self.timing_on.load(Ordering::Relaxed) && !self.timing_dumped.load(Ordering::Relaxed) {
            if let Ok(buf) = self.timing.lock() {
                let s = format_commit_timing(&buf);
                if !s.is_empty() {
                    eprint!("{s}");
                }
            }
        }
        let persist =
            self.persist_on_drop.load(Ordering::Acquire) && !self.poisoned.load(Ordering::Acquire);
        if persist {
            let _ = run_checkpoint(self, Trigger::Automatic);
        }
    }
}

/// Who asked for a checkpoint. Only an explicit `Database::checkpoint()`
/// dumps a non-durable store (ADR-0105) and only it never takes the
/// no-op skip (ADR-0646) — an explicit checkpoint is the user's
/// force-dump, and the codec validations it runs (e.g. the Parquet
/// manifest's part-metrics refusal) are part of its contract. The
/// automatic family — timers, WAL rotation, persist-on-drop, and the
/// exposed close shape `Database::checkpoint_if_pending()` — dumps a
/// durable store, recycles-only a `:memory:` store, and skips over a
/// clean WAL and a published snapshot.
#[derive(Clone, Copy, PartialEq, Eq)]
/// What scheduled a checkpoint: the automatic close/rotation/timer
/// shapes (ADR-0646's no-op skip is theirs), or an explicit
/// `Database::checkpoint()` — the one typed spelling of the two-way
/// mode every close-shaped entry passes.
pub(crate) enum Trigger {
    Explicit,
    Automatic,
}

/// Run one checkpoint to completion, retrying admission if a concurrent
/// checkpoint or a blocked writer currently holds it. Shared by the timed
/// walker, `Database::checkpoint()`, `Database::checkpoint_if_pending()`
/// and persist-on-drop.
fn run_checkpoint(shared: &Shared, trigger: Trigger) -> Result<()> {
    loop {
        wait_checkpoint_idle(shared)?;
        if try_begin_checkpoint(shared) {
            let result = pin_rotate_and_checkpoint(shared, trigger);
            end_checkpoint(shared, &result);
            return result;
        }
    }
}

pub(crate) fn wait_checkpoint_idle(shared: &Shared) -> Result<()> {
    let mut busy = shared.checkpoint_busy.lock()?;
    while busy.busy || busy.blocked {
        shared.check_healthy()?;
        busy = shared.checkpoint_cv.wait(busy)?;
    }
    shared.check_healthy()?;
    Ok(())
}

pub(crate) fn try_begin_checkpoint(shared: &Shared) -> bool {
    let Ok(mut busy) = shared.checkpoint_busy.lock() else {
        return false;
    };
    if busy.busy || busy.blocked || shared.poisoned.load(Ordering::Acquire) {
        return false;
    }
    busy.busy = true;
    true
}

fn end_checkpoint(shared: &Shared, result: &Result<()>) {
    if let Ok(mut busy) = shared.checkpoint_busy.lock() {
        busy.busy = false;
        // Only a checkpoint that fails while a writer is blocked on its
        // admission drain is surfaced to that writer. A standalone
        // `Database::checkpoint()` failure is returned to its own caller and
        // must not fence later writes (ADR-0094).
        busy.last_error = if busy.blocked {
            result.as_ref().err().cloned()
        } else {
            None
        };
        shared.checkpoint_cv.notify_all();
    }
}

/// One checkpoint entry, the caller's mode as the typed
/// [`Trigger`] — never two twin functions behind an erased fn
/// pointer. A failed background durability barrier fences explicit
/// checkpoints on the current handle until reopen (ADR-0077,
/// ADR-0094).
pub(crate) fn checkpoint_with(shared: &Shared, trigger: Trigger) -> Result<()> {
    check_durability_error(shared)?;
    run_checkpoint(shared, trigger)
}

/// Rotate the open segment, pin the WAL — LSN, `commit_ts` and the
/// uncheckpointed verdict, all read under the pin's own WAL guard —
/// then dump (if [`dumps`]) and recycle. Must not take the WAL mutex
/// across dump (ADR-0020 / ADR-0038); the pin carries the facts in so
/// the dump path never locks the WAL before it publishes.
fn pin_rotate_and_checkpoint(shared: &Shared, trigger: Trigger) -> Result<()> {
    shared.check_healthy()?;
    let pin = {
        let mut wal = shared.wal.lock()?;
        rotate_for_mode(&mut wal, shared.resolved.durability_mode)?;
        CheckpointPin::of(&wal)
    };
    checkpoint_pinned(shared, trigger, pin)
}

/// Automatic checkpoints of a store whose snapshot VFS is not durable
/// (`:memory:`) write no snapshot; they only recycle the WAL (ADR-0105).
fn dumps(shared: &Shared, trigger: Trigger) -> bool {
    trigger == Trigger::Explicit || shared.store.vfs().durable()
}

/// One checkpoint's WAL pin — the fact `checkpoint_pinned` consumes:
/// the cut's LSN, the commit ts the dump horizon floors to, and the
/// uncheckpointed verdict (ADR-0646's skip input). Built at the single
/// place each capture site holds the WAL guard, and never re-read:
/// the dump path must proceed while another thread holds that lock
/// (ADR-0020).
#[derive(Clone, Copy)]
pub(crate) struct CheckpointPin {
    lsn: u64,
    pin_ts: u64,
    pending: bool,
}

impl CheckpointPin {
    /// The pin's one capture: both halves read under the caller's own
    /// WAL guard — the LSN/`commit_ts` pair `Wal::checkpoint_pin`
    /// reports, and the uncheckpointed verdict (ADR-0646's skip
    /// input). Never re-read later: the dump path must proceed while
    /// another thread holds the WAL lock (ADR-0020).
    pub(crate) fn of(wal: &Wal) -> Self {
        let (lsn, pin_ts) = wal.checkpoint_pin();
        CheckpointPin {
            lsn,
            pin_ts,
            pending: wal.has_uncheckpointed(),
        }
    }
}

/// Dump (if [`dumps`]) and recycle at a pin.
///
/// This is the one graph-reading path that derefs `shared.graph` without the
/// publication read guard or a snapshot pin (ADR-0137), and it must stay
/// that way: `admit_publication` holds the publication WRITE lock while it
/// waits for `checkpoint_busy.busy` to clear, so a checkpoint that took the
/// read guard would deadlock against the very writer waiting for it. The
/// exclusion is the busy flag instead — a caller owns it through
/// `try_begin_checkpoint`, and every publication holder (commit group,
/// follow reload swap) waits it out before it can `replace` the cell.
fn checkpoint_pinned(shared: &Shared, trigger: Trigger, pin: CheckpointPin) -> Result<()> {
    // Clamp the dump horizon to the graph's commit ts — the one seam, so
    // every pin producer inherits the floor. The WAL pin is 0 when no
    // commit was appended since the last recycle (e.g. a read-only session
    // right after a snapshot load), while snapshot-loaded rows are born at
    // xmin 1 — dumping at 0 would write an empty snapshot. After a rotate
    // every committed record is in a closed segment, so `graph.commit_ts()`
    // can only exceed the pin by a snapshot-synthesized ts that has no WAL
    // records the cut at `lsn` could lose.
    let snapshot_ts = pin.pin_ts.max(shared.graph.commit_ts());
    if dumps(shared, trigger) {
        if trigger != Trigger::Explicit && nothing_to_publish(shared, pin.pending)? {
            // A WAL with nothing uncheckpointed over a published snapshot
            // (ADR-0646): no commit this session appended, no pending
            // tail replayed — either leaves `pending` true and dumps as
            // before — and the published image already describes this
            // graph. The dump — for a markdown vault a full re-walk and
            // re-render of every file — would rewrite the bytes it just
            // read, so the checkpoint ends here: nothing to write,
            // nothing to recycle (no closed segment waits), no versions
            // to GC (the loaded rows are live at xmin 1 and no write
            // ran). The stores the skip must still serve keep their
            // dump: `:memory:` and a never-checkpointed bootstrap have
            // no published image to keep, so `nothing_to_publish`
            // answers false for them and the first dump still pins the
            // codec. An explicit `checkpoint()` never takes this
            // branch: it is the user's force-dump and keeps its codec
            // validations.
            return Ok(());
        }
        shared.check_healthy()?;
        // ADR-0120 §5/§6: the checkpoint publishes the store's current
        // ontology document (v4 manifest + object, or the JSON inline
        // field at value_encoding 3); `None` keeps the previous format.
        // The commit pipeline updates the state under the publication
        // boundary, so a checkpoint always observes the last committed
        // document.
        checkpoint_at(
            &shared.store,
            &**shared.graph,
            pin.lsn,
            snapshot_ts,
            shared.ontology.get().as_deref(),
        )?;
    }
    recycle_and_gc(shared, pin.lsn)
}

/// The no-op checkpoint verdict (the skip's one home): the WAL holds
/// nothing a dump could publish — `pending`, read under the pin's WAL
/// guard, the same predicate the timed checkpoint gates on — and the
/// store has already published a checkpoint image. Both halves are
/// load-bearing: a clean WAL alone would skip the first checkpoint of
/// a bootstrap (leaving the codec unpinned and no files materialized),
/// and a published image alone would skip publishing a replayed tail.
/// Never locks the WAL itself: the dump path must proceed while
/// another thread holds that lock (ADR-0020).
fn nothing_to_publish(shared: &Shared, pending: bool) -> Result<bool> {
    Ok(!pending && relate_storage::has_published_snapshot(&shared.store)?)
}

fn recycle_and_gc(shared: &Shared, lsn: u64) -> Result<()> {
    {
        let mut wal = shared.wal.lock()?;
        wal.recycle(lsn)?;
    }
    shared.graph.gc_versions();
    Ok(())
}

pub(crate) fn spawn_checkpoint_timer(
    weak: Weak<Shared>,
    ctl: Arc<ThreadStop>,
    interval: Duration,
) -> Result<Option<std::thread::JoinHandle<()>>> {
    crate::spawn_interval_worker(
        "relate-checkpoint-timer",
        weak,
        ctl,
        interval,
        timed_checkpoint,
    )
}

pub(crate) fn spawn_durability_timer(
    weak: Weak<Shared>,
    ctl: Arc<ThreadStop>,
    interval: Duration,
) -> Result<Option<std::thread::JoinHandle<()>>> {
    crate::spawn_interval_worker("relate-wal-sync", weak, ctl, interval, durability_tick)
}

fn durability_tick(shared: &Shared) {
    let result = match shared.wal.lock() {
        Ok(mut wal) if wal.needs_sync() => wal.sync().map(|_| ()),
        Ok(_) => Ok(()),
        Err(err) => Err(Error::semantic(err.to_string())),
    };
    if let Err(err) = result {
        record_durability_error(shared, err);
    }
}

pub(crate) fn check_durability_error(shared: &Shared) -> Result<()> {
    let guard = shared.durability_error.lock()?;
    match guard.as_ref() {
        Some(e) => Err(e.clone()),
        None => Ok(()),
    }
}

/// Snapshot rotate skips the WAL barrier (checkpoint is the durable cut).
/// Full always barriers; EverySecond also barriers here so a checkpoint that
/// wins the race against the 1s worker still closes the durability window.
pub(crate) fn rotate_for_mode(wal: &mut Wal, mode: DurabilityMode) -> Result<u64> {
    match mode {
        DurabilityMode::Snapshot => wal.rotate_relaxed(),
        DurabilityMode::Full | DurabilityMode::EverySecond => wal.rotate(),
    }
}

pub(crate) fn lock_wal_checked(shared: &Shared) -> Result<MutexGuard<'_, Wal>> {
    check_durability_error(shared)?;
    let wal = shared.wal.lock()?;
    if let Err(e) = check_durability_error(shared) {
        drop(wal);
        return Err(e);
    }
    Ok(wal)
}

fn record_durability_error(shared: &Shared, error: Error) {
    if let Ok(mut guard) = shared.durability_error.lock() {
        if guard.is_none() {
            *guard = Some(error);
        }
    }
}

pub(crate) fn timed_checkpoint(shared: &Shared) {
    if !try_begin_checkpoint(shared) {
        return;
    }
    let dirty = match shared.wal.lock() {
        Ok(wal) => wal.has_uncheckpointed(),
        Err(_) => {
            end_checkpoint(shared, &Ok(()));
            return;
        }
    };
    if !dirty {
        end_checkpoint(shared, &Ok(()));
        return;
    }
    let result = pin_rotate_and_checkpoint(shared, Trigger::Automatic);
    end_checkpoint(shared, &result);
}

/// WAL-rotation checkpoint. A dump runs on a background thread where threads
/// exist. [`Database`]'s drop joins that thread before the writer lease can
/// be taken again. A recycle-only checkpoint, or a failed spawn, runs inline.
/// `pin` — captured under the caller's own WAL guard — is the fact the dump
/// decision and ADR-0646's skip read; the callers schedule only over
/// uncheckpointed work, so its `pending` is true today, but it travels as
/// derived state rather than a hidden assumption.
pub(crate) fn schedule_checkpoint(shared: &Arc<Shared>, pin: CheckpointPin) {
    // The checkpoint-and-release tail the spawned worker and the
    // inline fallback share — one spelling, so a step added here
    // cannot be missed in one of the two paths. The pin is `Copy`, so
    // both holders run on their own copy.
    let run = move |shared: &Arc<Shared>| {
        let result = checkpoint_pinned(shared, Trigger::Automatic, pin);
        end_checkpoint(shared, &result);
    };
    #[cfg(not(target_arch = "wasm32"))]
    if dumps(shared, Trigger::Automatic) {
        let shared_t = Arc::clone(shared);
        if let Ok(handle) = std::thread::Builder::new()
            .name("relate-checkpoint".into())
            .spawn(move || run(&shared_t))
        {
            shared.track_background(Some(handle));
            return;
        }
    }
    run(shared);
}

#[cfg(test)]
mod tests;
