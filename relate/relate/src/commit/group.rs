//! The group-commit machinery of the optimistic write pipeline (ADR-0019,
//! ADR-0094): the admission queue ([`GroupCommit`], on [`Shared`]), one
//! queued [`CommitSlot`] per admitted transaction with its
//! [`CommitWaiter`], the per-slot admission stages ([`admit_slot`]) and
//! the one leader loop that flushes a batch through
//! [`commit_preflighted`](super::commit_preflighted) and completes every
//! waiter exactly once ([`Waiters`]). Extracted from `commit.rs` (the
//! `session_5x` child-module pattern) — `commit.rs` keeps the entry
//! points that build slots and the shared commit core they flush into.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use web_time::Instant;

use relate_executor::RedoOp;
use relate_storage::EncodedOps;

use super::admission;
use super::record_provenance::ProvenanceFields;
use super::{commit_preflighted, Admitted, GroupTiming};
use crate::checkpoint::{begin_publication, lock_wal_checked};
use crate::{Error, Result, Shared};

#[derive(Default)]
pub(crate) struct GroupCommit {
    pub(super) queue: Vec<CommitSlot>,
    pub(super) has_leader: bool,
}

impl GroupCommit {
    /// No leader flushing a batch and nothing waiting to join one —
    /// the drain target [`Database::demote`] (ADR-0440) polls for once
    /// `ensure_writable` has refused every new admission, so this can
    /// only become true and stay true.
    pub(crate) fn is_quiescent(&self) -> bool {
        !self.has_leader && self.queue.is_empty()
    }
}

pub(super) struct CommitSlot {
    pub(super) snapshot_ts: u64,
    pub(super) txn_id: u64,
    pub(super) ops: Vec<RedoOp>,
    /// The ops' one JSON encoding, made at admission on the caller's thread.
    pub(super) encoded: EncodedOps,
    pub(super) parse_plan_ns: u64,
    pub(super) execute_ns: u64,
    pub(super) enqueued: Instant,
    pub(super) waiter: Arc<CommitWaiter>,
    /// The transaction's provenance half (ADR-0610) — the one declared
    /// shape (`ProvenanceFields`), carried whole from the admission
    /// derive through the flush to the retained-table insert: the
    /// metadata map, the derived changed keys and the record's family,
    /// `None` field-by-field while the feature is off (the
    /// normalization this pipeline owns, so the encoded record and the
    /// retained table never disagree).
    pub(super) provenance: ProvenanceFields,
}

#[cfg(test)]
impl CommitSlot {
    /// A slot with no timing facts whose ops are encoded here, as
    /// admission does.
    pub(super) fn for_test(
        snapshot_ts: u64,
        txn_id: u64,
        ops: Vec<RedoOp>,
        waiter: Arc<CommitWaiter>,
    ) -> Self {
        Self {
            snapshot_ts,
            txn_id,
            encoded: EncodedOps::encode_unmetered(&ops, Error::WAL_PAYLOAD_CAP_BYTES)
                .expect("test ops encode"),
            ops,
            parse_plan_ns: 0,
            execute_ns: 0,
            enqueued: Instant::now(),
            waiter,
            provenance: ProvenanceFields::default(),
        }
    }
}

pub(super) struct CommitWaiter {
    /// The slot's outcome: on success the commit timestamp the group
    /// assigned it, so the caller rebases onto *its own* commit rather
    /// than re-reading a live clock a foreign commit may have advanced.
    result: Mutex<Option<Result<u64>>>,
    cv: Condvar,
}

impl CommitWaiter {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            cv: Condvar::new(),
        })
    }

    pub(super) fn complete(&self, r: Result<u64>) {
        if let Ok(mut g) = self.result.lock() {
            *g = Some(r);
            self.cv.notify_one();
        }
    }

    pub(super) fn wait(&self) -> Result<u64> {
        let mut g = self.result.lock()?;
        while g.is_none() {
            g = self.cv.wait(g)?;
        }
        g.take()
            .expect("commit waiter result set after condvar wake")
    }

    /// Whether an outcome was recorded — the test suite's no-hang probe
    /// over the private result slot.
    #[cfg(test)]
    pub(super) fn completed(&self) -> bool {
        self.result.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

/// Owns the waiters of one group flush. `Drop` fails every waiter that has
/// not been completed yet — with the stored error when one was recorded, and
/// with an internal error otherwise — so every early return has exactly one
/// fail-all path and success only ever comes from the explicit
/// `complete_ok()`.
pub(super) struct Waiters {
    pending: Vec<(usize, Arc<CommitWaiter>)>,
    error: Option<Error>,
}

impl Waiters {
    fn new(batch: &[CommitSlot]) -> Self {
        Self {
            pending: batch
                .iter()
                .enumerate()
                .map(|(i, slot)| (i, Arc::clone(&slot.waiter)))
                .collect(),
            error: None,
        }
    }

    /// Complete a pre-acceptance rejection (conflict / capacity) immediately
    /// and drop it from the pending set so `Drop` never completes it twice.
    pub(super) fn reject(&mut self, index: usize, error: Error) {
        self.pending.retain(|(i, waiter)| {
            if *i == index {
                waiter.complete(Err(error.clone()));
                false
            } else {
                true
            }
        });
    }

    fn fail(&mut self, error: Error) {
        self.error = Some(error);
    }

    /// Complete every still-pending (i.e. accepted) waiter with the
    /// commit timestamp its slot was assigned at admission. `assigned`
    /// holds one `(batch index, commit_ts)` pair per accepted slot; a
    /// pending waiter with no pair was never accepted, which the caller's
    /// control flow excludes — fail it loudly rather than acknowledge.
    fn complete_ok(&mut self, assigned: &[(usize, u64)]) {
        for (i, waiter) in self.pending.drain(..) {
            match assigned.iter().find(|(j, _)| *j == i) {
                Some((_, ts)) => waiter.complete(Ok(*ts)),
                None => waiter.complete(Err(Error::internal(
                    "committed slot missing its assigned commit timestamp",
                ))),
            }
        }
    }
}

impl Drop for Waiters {
    fn drop(&mut self) {
        for (_, waiter) in self.pending.drain(..) {
            match &self.error {
                Some(error) => waiter.complete(Err(error.clone())),
                // Success is only ever signalled by `complete_ok`: a guard
                // dropped without a recorded outcome must not acknowledge
                // the write.
                None => waiter.complete(Err(Error::internal(
                    "commit group dropped without a result",
                ))),
            }
        }
    }
}

/// One group flush's successful outcome: the `(batch index, commit_ts)`
/// pair of every accepted slot (each waiter's success value) plus the
/// group's timing, when measured.
struct FlushOutcome {
    assigned: Vec<(usize, u64)>,
    timing: Option<GroupTiming>,
}

pub(super) fn flush_group(shared: &Arc<Shared>, batch: Vec<CommitSlot>) -> Result<()> {
    let mut waiters = Waiters::new(&batch);
    // Acquire the publication and WAL guards and run the commit core inside
    // one closure, so the guards release exactly when the group's outcome is
    // known; the single terminal `match` is the only place that completes or
    // fails every waiter.
    let result = (|| -> Result<FlushOutcome> {
        let publication = begin_publication(shared)?;
        let wal = lock_wal_checked(shared)?;
        let batch_start = Instant::now();
        let accepted = admission::admit_group(shared, batch, &mut waiters, batch_start);

        // Strip the batch indices the records traveled with — the commit
        // core is shared with the replicated path, which never sees them —
        // deriving each accepted waiter's success value from the same
        // records, just before the move.
        let (assigned, records): (Vec<(usize, u64)>, Vec<Admitted>) = accepted
            .into_iter()
            .map(|(index, a)| ((index, a.record.commit_ts), a))
            .unzip();

        if records.is_empty() {
            return Ok(FlushOutcome {
                assigned,
                timing: None,
            });
        }

        commit_preflighted(
            shared,
            wal,
            records,
            publication,
            shared.timing_on.load(Ordering::Relaxed),
        )
        .map(|committed| FlushOutcome {
            assigned,
            timing: committed.timing,
        })
    })();
    match result {
        Ok(outcome) => {
            // One flat record under a single lock: the group's samples plus
            // its one Full-durability-barrier count (ADR-0019).
            if let (Some(t), Ok(mut buf)) = (outcome.timing, shared.timing.lock()) {
                buf.sync_calls += u64::from(t.synced);
                buf.samples.extend(t.samples);
            }
            waiters.complete_ok(&outcome.assigned);
            Ok(())
        }
        Err(e) => {
            waiters.fail(e.clone());
            Err(e)
        }
    }
}
