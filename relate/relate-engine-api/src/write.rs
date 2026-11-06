//! Durable ordering and the commit clock. Apply and publication stay in
//! the facade, which hands the query engine the redo this trait made durable.

use relate_core::Result;

use crate::commit::CommitRecord;
use crate::export::{GraphExport, GraphImport};
use crate::redo::RedoOp;

/// One group's redo, captured against the snapshot the statement read.
pub struct CommitGroup {
    pub ops: Vec<RedoOp>,
    pub snapshot_ts: u64,
}

/// The clock value the log assigned to one group.
pub struct CommittedSlot {
    pub lsn: u64,
    pub commit_ts: u64,
}

/// What a follower poll returns.
pub enum FollowBatch {
    Idle,
    Records(Vec<CommitRecord>),
    Gap,
}

pub trait WriteEngine: Send {
    /// Make `groups` durable and assign each a commit timestamp, in order.
    fn commit(&mut self, groups: &[CommitGroup]) -> Result<Vec<CommittedSlot>>;

    /// Publish a snapshot of `graph` at `snapshot_ts`, covering the log
    /// through `lsn`.
    fn checkpoint(&mut self, graph: &dyn GraphExport, lsn: u64, snapshot_ts: u64) -> Result<()>;

    /// Records with `lsn` greater than `from_lsn`.
    fn poll_follow(&mut self, from_lsn: u64) -> Result<FollowBatch>;

    /// Replace `graph` with the latest published snapshot plus the log
    /// after it.
    fn reload(&mut self, graph: &mut dyn GraphImport) -> Result<()>;
}
