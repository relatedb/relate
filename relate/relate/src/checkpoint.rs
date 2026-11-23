//! Checkpoint machinery: freeze a cut (the WAL pin, the commit ts, the
//! snapshot pin and the store image), dump from it, recycle segments
//! (ADR-0020, ADR-0643), the background timer (ADR-0041), and
//! persist-on-drop.

use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard, RwLockWriteGuard, Weak};
use std::time::Duration;

use relate_storage::{checkpoint_at_image, CheckpointCut, Rewrite, Wal};
use web_time::Instant;

use crate::commit_timing::format_commit_timing;
use crate::{DurabilityMode, Error, Result, Shared, ThreadStop};

/// The pause of the most recently completed checkpoint: its wall-clock
/// duration and what asked for it. Kept only for the checkpoint tests —
/// production reads it from the stderr report.
#[cfg(test)]
pub(crate) struct CheckpointReport {
    pub(crate) trigger: Trigger,
    pub(crate) duration: Duration,
}

#[derive(Default)]
pub(crate) struct CheckpointState {
    busy: bool,
    #[cfg(test)]
    last: Option<CheckpointReport>,
}

/// The exclusive publication boundary: one commit group, one graph swap, or
/// one checkpoint cut at a time. A dump holds it only for its cut, never for
/// the dump itself (ADR-0643).
pub(crate) struct Publication<'a> {
    _write: RwLockWriteGuard<'a, ()>,
}

pub(crate) fn begin_publication(shared: &Shared) -> Result<Publication<'_>> {
    let write = shared.publication.write()?;
    shared.check_healthy()?;
    Ok(Publication { _write: write })
}

/// Publication lock for a follow reload. The reload thread may already have
/// recorded a pin-stall failure in the follow health state; that must not
/// abort the swap that clears it, so this gate checks WAL poisoning only.
pub(crate) fn begin_reload_swap(shared: &Shared) -> Result<Publication<'_>> {
    let write = shared.publication.write()?;
    shared.check_poisoned()?;
    Ok(Publication { _write: write })
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
            // The persist-on-drop checkpoint is the only durability event a
            // non-Full store gets at drop, and this `Shared` is being
            // destroyed: recording into its own durability-error slot would
            // be a phantom fix (no fence or reader outlives the handle), so
            // the failure is reported where a human still sees it — the drop
            // path's stderr, as the timing dump above does.
            if let Err(error) = checkpoint_with(self, Trigger::PersistOnDrop) {
                report_checkpoint_failure(Trigger::PersistOnDrop, &error);
            }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// The interval worker (ADR-0041).
    Timed,
    /// The WAL rotation threshold crossed by a commit (`schedule_checkpoint`).
    Rotation,
    /// `Database::checkpoint()` / `checkpoint_if_writable`.
    Explicit,
    /// `Database`'s persist-on-drop.
    PersistOnDrop,
    /// The exposed close shape `Database::checkpoint_if_pending()`
    /// (ADR-0646): the automatic family's no-op skip with a final
    /// checkpoint's compaction.
    Close,
}

/// One trigger's row of per-trigger facts, from [`Trigger::policy`] — the one
/// exhaustive table, so a new variant states every fact instead of the
/// family being derived by inequality (where a new variant silently joins it).
struct TriggerPolicy {
    /// Whether this trigger is one of the automatic family — every trigger
    /// but an explicit call.
    automatic: bool,
    /// Whether a failed background durability barrier refuses this trigger
    /// (ADR-0077, ADR-0094). Only an explicit checkpoint is fenced: it is the
    /// user's force-dump and must fail loudly on a fenced handle, while the
    /// automatic triggers write no snapshot on a non-durable store anyway.
    fenced: bool,
    /// This trigger's rewrite choice for the storage entry.
    rewrite: Rewrite,
    /// The trigger name the checkpoint report logs.
    label: &'static str,
}

impl Trigger {
    /// This trigger's row: whether it is automatic, whether a durability
    /// fence refuses it, its rewrite choice and its report label. The one
    /// exhaustive match — every variant named, no `_` arm — so a new trigger
    /// cannot silently join the automatic family, inherit a neighbour's
    /// rewrite or fence, or leave its label stale.
    fn policy(self) -> TriggerPolicy {
        match self {
            // Automatic, mid-load, copy-on-write.
            Self::Timed => TriggerPolicy {
                automatic: true,
                fenced: false,
                rewrite: Rewrite::Incremental,
                label: "timed walker",
            },
            Self::Rotation => TriggerPolicy {
                automatic: true,
                fenced: false,
                rewrite: Rewrite::Incremental,
                label: "rotation threshold",
            },
            // The user's force-dump — the one fenced trigger — and the final
            // checkpoints that coalesce clean neighbours (ADR-0183
            // Decision 2).
            Self::Explicit => TriggerPolicy {
                automatic: false,
                fenced: true,
                rewrite: Rewrite::Coalesce,
                label: "explicit call",
            },
            Self::PersistOnDrop => TriggerPolicy {
                automatic: true,
                fenced: false,
                rewrite: Rewrite::Coalesce,
                label: "persist-on-drop",
            },
            Self::Close => TriggerPolicy {
                automatic: true,
                fenced: false,
                rewrite: Rewrite::Coalesce,
                label: "close path",
            },
        }
    }

    /// Whether this trigger is one of the automatic family — every trigger
    /// but an explicit call. Only an explicit call dumps a non-durable store
    /// and never takes the ADR-0646 no-op skip.
    fn is_automatic(self) -> bool {
        self.policy().automatic
    }

    /// This trigger's rewrite choice for the storage entry: the final
    /// checkpoints — an explicit call, persist-on-drop and the close shape —
    /// coalesce clean neighbours into their rewritten units (ADR-0183
    /// Decision 2); the automatic triggers run mid-load and stay
    /// copy-on-write, because re-reading every clean shard they have already
    /// written is exactly what they must not do (ADR-0643 Decision 6).
    fn rewrite(self) -> Rewrite {
        self.policy().rewrite
    }

    /// The trigger name the checkpoint report logs: one per [`Trigger`].
    fn label(self) -> &'static str {
        self.policy().label
    }
}

/// One checkpoint entry, the caller's mode as the typed [`Trigger`] — never
/// two twin functions behind an erased fn pointer. Run to completion,
/// retrying admission if another checkpoint already holds it. Every caller
/// uses this one entry: `Database::checkpoint()`,
/// `Database::checkpoint_if_pending()`, persist-on-drop and the close path
/// (the timed walker owns its own busy begin and never retries). The
/// durability fence is the trigger's own row fact, consulted in
/// [`checkpoint_pinned`], not a property of which entry a caller picked.
pub(crate) fn checkpoint_with(shared: &Shared, trigger: Trigger) -> Result<()> {
    loop {
        wait_checkpoint_idle(shared)?;
        if let Some(_busy) = try_begin_checkpoint(shared) {
            return checkpoint_pinned(shared, trigger);
        }
    }
}

pub(crate) fn wait_checkpoint_idle(shared: &Shared) -> Result<()> {
    wait_checkpoint_idle_within(shared, Duration::MAX)
}

/// The one checkpoint-idle wait protocol, with a deadline: the commit
/// writers' back-pressure wait (unbounded — a checkpoint either finishes
/// or poisons the store) and the harness's bounded wait (a stuck
/// checkpoint fails the test, never hangs it) are this one loop.
pub(crate) fn wait_checkpoint_idle_within(shared: &Shared, timeout: Duration) -> Result<()> {
    // The module's clock is `web_time::Instant`: std's panics on wasm.
    let start = Instant::now();
    let mut busy = shared.checkpoint_busy.lock()?;
    while busy.busy {
        shared.check_healthy()?;
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(Error::storage("checkpoint did not become idle in time"));
        }
        busy = shared.checkpoint_cv.wait_timeout(busy, remaining)?.0;
    }
    shared.check_healthy()?;
    Ok(())
}

/// How many closed WAL segments may wait for a dump before a writer is
/// back-pressured: above it, a commit group waits for the in-flight
/// checkpoint to finish and recycle. A closed segment holds at most one
/// rotation threshold's records (~64 MiB), so the budget bounds the WAL left
/// on disk while leaving a running dump room to finish under a writing load
/// (ADR-0644).
pub(crate) const CLOSED_SEGMENT_BUDGET: usize = 4;

/// The writer's back-pressure gate: over the closed-segment budget, a commit
/// group waits for the in-flight checkpoint to finish its dump and recycle;
/// under it a dump costs a commit nothing (ADR-0643, ADR-0644). Holds no lock
/// while it waits — the checkpoint's cut needs the publication write guard,
/// which this caller has not taken yet.
fn wait_for_backlog_budget(shared: &Shared) -> Result<()> {
    let backlog = lock_wal_checked(shared)?.closed_segments();
    if backlog > CLOSED_SEGMENT_BUDGET {
        wait_checkpoint_idle(shared)?;
    }
    Ok(())
}

/// The committing writer's one publication acquire: the back-pressure gate
/// first ([`wait_for_backlog_budget`] — it holds no guard while it waits),
/// then [`begin_publication`]. Both the group-commit leader and the
/// replicated path take publication through here, so the closed-segment
/// budget binds a replica applying batches exactly as it binds a local
/// group. The checkpoint and the resize take [`begin_publication`]
/// directly: they are the work the budget waits for, not writers subject
/// to it.
pub(crate) fn begin_commit_publication(shared: &Shared) -> Result<Publication<'_>> {
    wait_for_backlog_budget(shared)?;
    begin_publication(shared)
}

/// The checkpoint's exclusive busy flag, held for one checkpoint and
/// released on drop: dropping clears `busy` and wakes the writers waiting in
/// [`wait_checkpoint_idle`]. Owns its `Shared` when the rotation checkpoint
/// moves it into its own thread.
pub(crate) struct CheckpointBusy<'a> {
    shared: BusyOwner<'a>,
}

enum BusyOwner<'a> {
    Borrowed(&'a Shared),
    Owned(Arc<Shared>),
}

impl BusyOwner<'_> {
    fn get(&self) -> &Shared {
        match self {
            Self::Borrowed(shared) => shared,
            Self::Owned(shared) => shared,
        }
    }
}

impl Drop for CheckpointBusy<'_> {
    fn drop(&mut self) {
        let shared = self.shared.get();
        // The release is unconditional, poison or not: clearing the flag and
        // waking the writers in `wait_checkpoint_idle` is the guard's whole
        // contract, and skipping it on a poisoned lock would leave the slot
        // marked busy forever while `mark_busy` refuses every later
        // checkpoint on the same poison. The refusal policy belongs there,
        // and every other poison-tolerant read in `shared.rs` recovers the
        // same way.
        let mut busy = shared
            .checkpoint_busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        busy.busy = false;
        shared.checkpoint_cv.notify_all();
    }
}

fn mark_busy(shared: &Shared) -> bool {
    let Ok(mut busy) = shared.checkpoint_busy.lock() else {
        return false;
    };
    if busy.busy || shared.poisoned.load(Ordering::Acquire) {
        return false;
    }
    busy.busy = true;
    true
}

pub(crate) fn try_begin_checkpoint(shared: &Shared) -> Option<CheckpointBusy<'_>> {
    mark_busy(shared).then(|| CheckpointBusy {
        shared: BusyOwner::Borrowed(shared),
    })
}

/// [`try_begin_checkpoint`] for a caller that hands the guard to a spawned
/// thread, so it must own its `Shared`.
fn try_begin_checkpoint_owned(shared: &Arc<Shared>) -> Option<CheckpointBusy<'static>> {
    mark_busy(shared).then(|| CheckpointBusy {
        shared: BusyOwner::Owned(Arc::clone(shared)),
    })
}

/// Automatic checkpoints of a store whose snapshot VFS is not durable
/// (`:memory:`) write no snapshot; they only recycle the WAL (ADR-0105).
fn dumps(shared: &Shared, trigger: Trigger) -> bool {
    !trigger.is_automatic() || shared.store.vfs().durable()
}

/// ADR-0646's no-op skip, then the timed delegate: an automatic checkpoint
/// over a durable store with a clean WAL and an already-published snapshot
/// writes nothing at all; everything else runs
/// [`pin_dump_and_recycle`] under the checkpoint-busy flag and reports its
/// duration.
fn checkpoint_pinned(shared: &Shared, trigger: Trigger) -> Result<()> {
    // The row's fence fact, consulted at the one place every entry reaches: a
    // failed background durability barrier refuses an explicit checkpoint on
    // this handle until reopen (ADR-0077, ADR-0094).
    if trigger.policy().fenced {
        check_durability_error(shared)?;
    }
    // ADR-0646's no-op skip: an automatic checkpoint over a durable store
    // (the only automatic one that dumps) with a clean WAL and an
    // already-published snapshot writes nothing at all — no image rewrite, no
    // recycle, no GC. An explicit `checkpoint()` never takes it: it is the
    // user's force-dump and keeps its codec validations.
    if trigger.is_automatic() && shared.store.vfs().durable() && nothing_to_publish(shared)? {
        return Ok(());
    }
    let started = Instant::now();
    let result = pin_dump_and_recycle(shared, trigger);
    if result.is_ok() {
        let duration = started.elapsed();
        report_checkpoint(trigger, duration);
        // Kept for the checkpoint tests; production reads the stderr line.
        #[cfg(test)]
        if let Ok(mut state) = shared.checkpoint_busy.lock() {
            state.last = Some(CheckpointReport { trigger, duration });
        }
    }
    result
}

/// Report a completed checkpoint's duration and trigger on the stderr
/// channel the resize and torn-tail notices use. Off unless
/// `RELATE_CHECKPOINT_REPORT` is set: a library must not write to its host's
/// stderr on every checkpoint — the fs-to-graph e2e tests read a child's
/// stderr as their whole assertion — while a failed checkpoint still reports
/// unconditionally, since a failure no one can see is a silent fallback.
fn report_checkpoint(trigger: Trigger, duration: Duration) {
    if std::env::var_os("RELATE_CHECKPOINT_REPORT").is_none() {
        return;
    }
    eprintln!(
        "relate: checkpoint completed in {:.3} ms (trigger: {})",
        duration.as_secs_f64() * 1000.0,
        trigger.label()
    );
}

/// Report a background trigger's failed checkpoint where a human sees it —
/// the one home of that policy, so no trigger discards its failure. A failed
/// dump is not a durability failure (ADR-0643 §4): the WAL is intact and the
/// dirty ids go back to the next checkpoint, so nothing is fenced — but a
/// failure no one can see is a silent fallback.
fn report_checkpoint_failure(trigger: Trigger, error: &Error) {
    eprintln!(
        "relate: checkpoint failed (trigger: {}): {error}",
        trigger.label()
    );
}

/// Freeze the checkpoint's cut, dump from it (when [`dumps`]) and recycle.
///
/// The cut is taken under the publication write guard: rotate the open
/// segment (only when this checkpoint dumps), pin the WAL LSN/`commit_ts`
/// (ADR-0020), pin the graph's snapshot so the dump's MVCC view and the
/// graph cell stay stable for its length, and take the image of everything
/// else the dump publishes. The guard is released before the dump, so a
/// commit that lands during the dump neither waits for it nor changes what
/// it publishes (ADR-0643).
///
/// The snapshot pin is also what keeps this dump off every other
/// graph-reading path's exclusion: `GraphCell::replace` waits for it
/// (ADR-0137), and `gc_versions` cannot drop a version the dump still reads
/// at `snapshot_ts`.
fn pin_dump_and_recycle(shared: &Shared, trigger: Trigger) -> Result<()> {
    let publication = begin_publication(shared)?;
    // A recycling checkpoint writes no snapshot: it needs neither pin nor
    // image, and must leave the dirty ids for the next dumping checkpoint.
    let dumping = dumps(shared, trigger);
    let (lsn, snapshot_ts) = {
        // Rotate closes the open segment; the pin is its last record. Only a
        // dumping checkpoint closes it: a recycle-only one recycles the
        // segments already closed and leaves the open segment — and the
        // records it still holds — for the next dump. Clamp the dump horizon
        // to the graph's commit ts — the one seam, so every pin producer
        // inherits the floor. The WAL pin is 0 when no commit was appended
        // since the last recycle (e.g. a read-only session right after a
        // snapshot load), while snapshot-loaded rows are born at xmin 1 —
        // dumping at 0 would write an empty snapshot.
        let mut wal = shared.wal.lock()?;
        if dumping {
            rotate_for_mode(&mut wal, shared.resolved.durability_mode)?;
        }
        let (lsn, pin_ts) = wal.checkpoint_pin();
        (lsn, pin_ts.max(shared.graph.commit_ts()))
    };
    let cut = if dumping {
        // ADR-0120 §5/§6: the checkpoint publishes the store's current
        // ontology document (v4 manifest + object, or the JSON inline field
        // at value_encoding 3); `None` keeps the previous format. It is
        // frozen into the cut with the image, under the same guard, so the
        // dump serves the document of its own cut.
        let ontology = shared.ontology.get();
        Some((
            shared.graph.pin_now()?,
            CheckpointCut::take(&**shared.graph, lsn, snapshot_ts, ontology.as_deref())?,
        ))
    } else {
        None
    };
    drop(publication);
    if let Some((_pin, cut)) = cut {
        // The dump reads rows at the cut's snapshot timestamp from the
        // pinned graph and everything else from its image — the ontology
        // document included, frozen in the cut. A failure hands the
        // image's dirty ids back, so the next checkpoint still covers
        // them. The rewrite (copy-on-write vs ADR-0183's coalescing) is the
        // trigger's one classification, handed to the shard writer.
        checkpoint_at_image(&shared.store, &cut, trigger.rewrite())?;
    }
    recycle_and_gc(shared, lsn)
}

/// The no-op checkpoint verdict (ADR-0646's skip, the one home): the WAL
/// holds nothing a dump could publish — `pending`, the same predicate the
/// timed checkpoint gates on — and the store has already published a
/// checkpoint image. Both halves are load-bearing: a clean WAL alone would
/// skip the first checkpoint of a bootstrap (leaving the codec unpinned
/// and no files materialized), and a published image alone would skip
/// publishing a replayed tail.
fn nothing_to_publish(shared: &Shared) -> Result<bool> {
    let pending = lock_wal_checked(shared)?.has_uncheckpointed();
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
fn rotate_for_mode(wal: &mut Wal, mode: DurabilityMode) -> Result<u64> {
    match mode {
        DurabilityMode::Snapshot => wal.rotate_relaxed(),
        DurabilityMode::Full | DurabilityMode::EverySecond => wal.rotate(),
    }
}

/// The rotation protocol's one entry point (ADR-0643): rotate when the
/// WAL's checkpoint threshold was crossed, then answer whether a dump is
/// due — a dump is due exactly when the rotate leaves closed segments
/// behind. Every writer that finishes a commit consults this one policy;
/// `should_checkpoint` and `has_closed_segments` are its internals, not
/// facts each caller re-derives.
pub(crate) fn rotate_if_due(wal: &mut Wal, mode: DurabilityMode) -> Result<bool> {
    if wal.should_checkpoint() {
        rotate_for_mode(wal, mode)?;
    }
    Ok(wal.has_closed_segments())
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
    let Some(_busy) = try_begin_checkpoint(shared) else {
        return;
    };
    let dirty = match shared.wal.lock() {
        Ok(wal) => wal.has_uncheckpointed(),
        // A poisoned WAL lock is a real refusal, not a reason to skip
        // silently: the interval worker reports it like any other failure.
        Err(error) => {
            eprintln!("relate: timed checkpoint could not read the WAL: {error}");
            return;
        }
    };
    if !dirty {
        return;
    }
    // A dump failure is not a durability failure: the WAL is intact, and the
    // failed dump hands its dirty ids back for the next checkpoint. It is
    // reported where a human sees it, never fenced into the slot every commit
    // consults — ADR-0643 §4 keeps a checkpoint's failure visible to its own
    // caller only, and the rotation trigger fences nothing either.
    if let Err(error) = checkpoint_pinned(shared, Trigger::Timed) {
        report_checkpoint_failure(Trigger::Timed, &error);
    }
}

/// WAL-rotation checkpoint. A dump runs on a background thread where threads
/// exist. [`Database`]'s drop joins that thread before the writer lease can
/// be taken again. A recycle-only checkpoint, or a failed spawn, runs inline.
pub(crate) fn schedule_checkpoint(shared: &Arc<Shared>) {
    // A failed dump leaves its WAL tail and dirty ids for the next
    // checkpoint; nothing fences the writer that crossed the threshold. The
    // schedule owns its busy begin: losing it means another checkpoint is
    // already covering this WAL tail.
    #[cfg(not(target_arch = "wasm32"))]
    if dumps(shared, Trigger::Rotation) {
        let Some(busy) = try_begin_checkpoint_owned(shared) else {
            return;
        };
        let shared_t = Arc::clone(shared);
        if let Ok(handle) = std::thread::Builder::new()
            .name("relate-checkpoint".into())
            .spawn(move || {
                let _busy = busy;
                if let Err(error) = checkpoint_pinned(&shared_t, Trigger::Rotation) {
                    report_checkpoint_failure(Trigger::Rotation, &error);
                }
            })
        {
            shared.track_background(Some(handle));
            return;
        }
    }
    if let Some(_busy) = try_begin_checkpoint(shared) {
        if let Err(error) = checkpoint_pinned(shared, Trigger::Rotation) {
            report_checkpoint_failure(Trigger::Rotation, &error);
        }
    }
}

#[cfg(test)]
mod tests;
