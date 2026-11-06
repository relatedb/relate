//! The local write engine. It owns the process-local log. The facade keeps
//! group-commit admission, conflict checks, apply, and publication, and
//! calls [`append_assigned`] while it already holds the log lock.
//!
//! No threads are spawned here. Checkpoint timers and follow loops stay in
//! the facade, which compiles them out on `wasm32`.

use std::ops::Deref;
use std::sync::Mutex;

use relate_core::{Error, Result};
use relate_engine_api::{CommitGroup, CommittedSlot, FollowBatch, GraphImport, WriteEngine};
use relate_storage::{StoreTarget, Wal, WalCatchUp, WalRecord};

/// One accepted group's timestamp and redo, borrowed for the append.
pub struct AssignedRecord<'a> {
    pub commit_ts: u64,
    pub ops: &'a [relate_engine_api::RedoOp],
}

/// What one durable append wrote, in group order.
pub struct AppendedRecord {
    pub lsn: u64,
    pub encode_write_ns: u64,
}

/// Preflight and append records the caller already timestamped. Does not
/// `sync`. The caller holds the log lock and decides the durability barrier.
pub fn append_assigned(wal: &mut Wal, records: &[AssignedRecord<'_>]) -> Result<Vec<AppendedRecord>> {
    let base = wal.lsn;
    let mut proofs = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let lsn = base
            .checked_add(index as u64 + 1)
            .ok_or_else(|| Error::storage("WAL LSN overflow"))?;
        proofs.push(wal.preflight_record(lsn, record.commit_ts, record.ops)?);
    }
    let mut appended = Vec::with_capacity(proofs.len());
    for proof in proofs {
        let written = wal.append_preflighted(proof)?;
        appended.push(AppendedRecord {
            lsn: written.lsn,
            encode_write_ns: written.encode_write_ns,
        });
    }
    Ok(appended)
}

/// The local log plus the store a checkpoint publishes into.
pub struct LocalWriteEngine {
    wal: Mutex<Wal>,
    store: StoreTarget,
}

impl LocalWriteEngine {
    pub fn new(store: StoreTarget, wal: Wal) -> Self {
        Self {
            wal: Mutex::new(wal),
            store,
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
        let mut wal = self
            .wal
            .lock()
            .map_err(|_| Error::storage("WAL lock poisoned"))?;
        let borrowed: Vec<AssignedRecord<'_>> = groups
            .iter()
            .map(|group| AssignedRecord {
                commit_ts: group.commit_ts,
                ops: group.ops.as_slice(),
            })
            .collect();
        let appended = append_assigned(&mut wal, &borrowed)?;
        wal.sync()?;
        Ok(appended
            .into_iter()
            .zip(groups)
            .map(|(written, group)| CommittedSlot {
                lsn: written.lsn,
                commit_ts: group.commit_ts,
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
        // The log is reopened against the snapshot that engine loaded.
        let _ = graph;
        let _ = relate_storage::open_follower_wal(&self.store)?;
        Ok(())
    }
}

fn commit_record(record: WalRecord) -> relate_engine_api::CommitRecord {
    relate_engine_api::CommitRecord {
        lsn: record.lsn,
        commit_ts: record.commit_ts,
        ops: record.ops,
    }
}
