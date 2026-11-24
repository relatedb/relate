//! The group-commit admission phase: every slot of a flushed group runs
//! the element-id conflict, commit-time uniqueness, capacity and timestamp
//! stages in order, against the state the earlier accepted slots
//! established. [`admit_group`] is the one entry; `flush_group` keeps the
//! publication / WAL / commit / waiter orchestration.

use std::sync::Arc;

use relate_executor::{Graph, RedoOp, SlotPosition};
use web_time::Instant;

use super::{Accepted, Admitted, CommitSlot, Waiters};
use crate::shared::Reserved;
use crate::{Error, Result, Shared};
use relate_executor::ConstraintAdmission;

/// Admit every slot of `batch` in order, returning the accepted ones with
/// their batch index (the index travels with the accepted record, so the
/// completion list derives from this one source instead of a parallel
/// vector). A refused slot is rejected through `waiters` and leaves no
/// reservation behind; the rest of the group still commits.
pub(super) fn admit_group(
    shared: &Arc<Shared>,
    batch: Vec<CommitSlot>,
    waiters: &mut Waiters,
    batch_start: Instant,
) -> Vec<(usize, Admitted)> {
    let mut walk = AdmissionWalk {
        accepted: Vec::new(),
        reserved: Reserved {
            bytes: 0,
            spans: shared.graph.dense_spans(),
        },
        constraint_admission: shared.graph.begin_constraint_admission(),
    };
    let batch_len = batch.len();
    for (index, slot) in batch.into_iter().enumerate() {
        let position = SlotPosition::of(index, batch_len);
        match admit_slot(shared, &mut walk, slot, position, batch_start) {
            Ok(admitted) => walk.accepted.push((index, admitted)),
            Err(e) => waiters.reject(index, e),
        }
    }
    walk.accepted
}

/// The group admission walk's mutable state: the accepted slots (their ops
/// standing in for the old `unapplied` shadow list, ADR-0063), the
/// reservation the accepted slots established, and the ADR-0439 constraint
/// overlay — one carrier threaded through [`admit_slot`] instead of lockstep
/// out-params. `reserved` moves only for an accepted slot: a slot refused at
/// any stage leaves no reservation, no phantom growth rung, and no overlay
/// hold behind.
struct AdmissionWalk<'a> {
    accepted: Vec<(usize, Admitted)>,
    /// The bytes and dense spans the accepted slots reserved. Each slot's
    /// estimate seeds from these spans instead of the committed spans, which
    /// cannot advance mid-group, so a growth rung is charged to the first slot
    /// that crosses it and costs no span growth for every later mate inside
    /// it — never re-charged per group-mate.
    reserved: Reserved,
    /// ADR-0439: the group's constraint-key admission state — the committed
    /// map plus the delta earlier accepted slots establish.
    constraint_admission: ConstraintAdmission<'a>,
}

/// Run one slot through every admission stage — element-id conflict,
/// commit-time uniqueness, capacity, timestamp — and merge its constraint
/// delta on acceptance. The one per-slot policy: the caller owns the single
/// rejection sink, and the checked-but-unmerged delta never escapes this
/// call (it is merged only after the capacity and timestamp decisions both
/// pass, so a slot rejected there leaves no holds or constraint-set changes
/// behind).
///
/// ADR-0439: the uniqueness probe runs beside the element-id conflict check
/// and before the capacity reservation; a slot that would introduce a
/// constrained tuple already held by another live entity is rejected with
/// the same retryable `Error::Conflict`, and the rest of the group still
/// commits.
fn admit_slot(
    shared: &Shared,
    walk: &mut AdmissionWalk<'_>,
    slot: CommitSlot,
    position: SlotPosition,
    batch_start: Instant,
) -> Result<Admitted> {
    // Disjoint field borrows: the earlier-ops view of `accepted` coexists
    // with the reserved/overlay mutations below.
    let AdmissionWalk {
        accepted,
        reserved,
        constraint_admission,
    } = walk;
    let wal_lock_ns = batch_start
        .saturating_duration_since(slot.enqueued)
        .as_nanos() as u64;
    let conflict = shared
        .graph
        .write_set_conflicts(slot.snapshot_ts, &slot.ops)
        || accepted
            .iter()
            .any(|(_, a)| Graph::redo_conflicts_with_later(&slot.ops, &a.record.ops));
    if conflict {
        return Err(Error::conflict());
    }
    let earlier: Vec<&[RedoOp]> = accepted
        .iter()
        .map(|(_, a)| a.record.ops.as_slice())
        .collect();
    let mut earlier_ops = earlier.iter().copied();
    let (pending, constraint_growth) =
        constraint_admission.check_with_earlier(&slot.ops, &mut earlier_ops, position)?;
    // Timestamp overflow rejects the slot like the conflict and capacity
    // stages above — never deferred to a sentinel the group-wide preflight
    // must remember to catch — and runs before the capacity reservation, so
    // a slot rejected here leaves no reserved bytes behind.
    let commit_ts = accepted
        .last()
        .map_or_else(|| shared.graph.commit_ts(), |(_, a)| a.record.commit_ts)
        .checked_add(1)
        .map(|next| next.max(slot.txn_id))
        .ok_or_else(|| Error::storage("commit timestamp overflow"))?;
    *reserved =
        shared.admit_redo_capacity_with(&slot.ops, *reserved, constraint_growth, &earlier)?;
    constraint_admission.accept(pending);
    Ok(Admitted {
        record: Accepted {
            ops: slot.ops,
            commit_ts,
            lsn: 0,
            encode_write_ns: 0,
            wal_lock_ns,
            parse_plan_ns: slot.parse_plan_ns,
            execute_ns: slot.execute_ns,
            provenance: slot.provenance,
        },
        encoded: slot.encoded,
    })
}
