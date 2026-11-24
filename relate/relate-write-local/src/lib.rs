//! The local write engine. It owns the process-local log. The facade keeps
//! group-commit admission, conflict checks, apply, and publication, and
//! calls [`append_assigned`] while it already holds the log lock.
//!
//! No threads are spawned here. Checkpoint timers and follow loops stay in
//! the facade, which compiles them out on `wasm32`.

use std::ops::Deref;
use std::sync::Mutex;

use relate_core::tokens::WorkspaceCharge;
use relate_core::{Error, Result};
use relate_engine_api::{CommitGroup, CommittedSlot, FollowBatch, GraphImport, WriteEngine};
use relate_storage::{EncodedOps, StoreTarget, Wal, WalCatchUp, WalRecord};

/// One accepted group's timestamp and the redo's one encoding (made at
/// admission, which validated the redo) that the append consumes.
pub struct AssignedRecord {
    pub commit_ts: u64,
    pub encoded: EncodedOps,
}

/// What one durable append wrote, in group order. It names the record by
/// the timestamp it was assigned, so a caller reads each record's outcome
/// off the record itself rather than pairing positions.
pub struct AppendedRecord {
    pub lsn: u64,
    pub commit_ts: u64,
    pub encode_write_ns: u64,
}

/// Preflight and append records the caller already timestamped. Does not
/// `sync`. The caller holds the log lock and decides the durability barrier.
pub fn append_assigned(wal: &mut Wal, records: Vec<AssignedRecord>) -> Result<Vec<AppendedRecord>> {
    // One group preflight: each record is checked against its predecessor in
    // the group, so a batch whose timestamps do not advance is refused
    // before the first frame is written.
    let proofs = wal.preflight_group(
        records
            .into_iter()
            .map(|record| (record.commit_ts, record.encoded)),
    )?;
    let mut appended = Vec::with_capacity(proofs.len());
    for proof in proofs {
        let commit_ts = proof.commit_ts();
        let written = wal.append_preflighted(proof)?;
        appended.push(AppendedRecord {
            lsn: written.lsn,
            commit_ts,
            encode_write_ns: written.encode_write_ns,
        });
    }
    Ok(appended)
}

/// The local log plus the store a checkpoint publishes into.
pub struct LocalWriteEngine {
    wal: Mutex<Wal>,
    store: StoreTarget,
    /// The handle's per-record payload cap (ADR-0650), copied once at
    /// construction: every handle this engine can ever hold — the initial
    /// open, a promote, a follower adopt, a reload — is constructed from
    /// the same resolved `wal_payload_limit`, so the value is invariant for
    /// the engine's lifetime and the commit path reads it without taking
    /// the log lock.
    payload_cap: u64,
}

impl LocalWriteEngine {
    pub fn new(store: StoreTarget, wal: Wal) -> Self {
        let payload_cap = wal.payload_cap();
        Self {
            wal: Mutex::new(wal),
            store,
            payload_cap,
        }
    }

    pub fn store(&self) -> &StoreTarget {
        &self.store
    }
}

impl Deref for LocalWriteEngine {
    type Target = Mutex<Wal>;

    fn deref(&self) -> &Mutex<Wal> {
        &self.wal
    }
}

impl WriteEngine for LocalWriteEngine {
    fn commit(&self, groups: &[CommitGroup]) -> Result<Vec<CommittedSlot>> {
        // This trait path has no admission step: it encodes each group's
        // redo once here, before the log lock, under the same shared encoder,
        // charging the encodings to one workspace charge held until the
        // append has consumed them.
        let mut charge = WorkspaceCharge::empty();
        let payload_cap = self.payload_cap;
        let borrowed: Vec<AssignedRecord> = groups
            .iter()
            .map(|group| {
                Ok(AssignedRecord {
                    commit_ts: group.commit_ts,
                    encoded: EncodedOps::encode(&group.ops, &mut charge, payload_cap)?,
                })
            })
            .collect::<Result<_>>()?;
        let mut wal = self
            .wal
            .lock()
            .map_err(|_| Error::storage("WAL lock poisoned"))?;
        // The cached cap and the installed handle cannot disagree: every
        // handle this engine ever holds is constructed from the same
        // resolved `wal_payload_limit`. Pinned here, under the lock the
        // commit already takes, so a future install path that breaks that
        // invariant fails loudly in debug builds instead of encoding
        // against a bound the append would refuse.
        debug_assert_eq!(
            wal.payload_cap(),
            payload_cap,
            "engine cap cache diverged from the installed handle"
        );
        let appended = append_assigned(&mut wal, borrowed)?;
        wal.sync()?;
        drop(charge);
        Ok(appended
            .into_iter()
            .map(|written| CommittedSlot {
                lsn: written.lsn,
                commit_ts: written.commit_ts,
            })
            .collect())
    }

    fn checkpoint(&self, graph: &dyn GraphImport, lsn: u64, snapshot_ts: u64) -> Result<()> {
        relate_storage::checkpoint_at(&self.store, graph, lsn, snapshot_ts, None)
    }

    fn poll_follow(&self, from_lsn: u64) -> Result<FollowBatch> {
        let mut wal = self
            .wal
            .lock()
            .map_err(|_| Error::storage("WAL lock poisoned"))?;
        Ok(match wal.poll_follow(from_lsn)? {
            WalCatchUp::Idle => FollowBatch::Idle,
            WalCatchUp::Gap => FollowBatch::Gap,
            WalCatchUp::Records { records, .. } => {
                FollowBatch::Records(records.into_iter().map(commit_record).collect())
            }
        })
    }

    fn reload(&self, graph: &mut dyn GraphImport) -> Result<()> {
        // Recovery builds a new query engine and the facade installs it.
        // This is the validation half of that reload on the write side: the
        // follower log is reopened read-only against the published
        // checkpoint — its scan proves the on-disk WAL is whole — and the
        // handle itself is deliberately dropped; the facade's adopt installs
        // the live one.
        let _ = graph;
        let _ = relate_storage::open_follower_wal(&self.store, self.payload_cap)?;
        Ok(())
    }
}

fn commit_record(record: WalRecord) -> relate_engine_api::CommitRecord {
    relate_engine_api::CommitRecord {
        lsn: record.lsn,
        commit_ts: record.commit_ts,
        ops: record.ops,
        // ADR-0610: the follower's catch-up carries the record's provenance
        // half unchanged — the fields the writer's store serialized (None
        // on a store that never opted in), the record's family type
        // included.
        metadata: record.metadata,
        changed_keys: record.changed_keys,
        record_type: record.record_type,
    }
}
