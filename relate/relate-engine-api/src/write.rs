//! Durable ordering and the commit clock. Apply and publication stay in
//! the facade, which hands the query engine the redo this trait made durable.

use relate_core::Result;

use crate::commit::CommitRecord;
use crate::export::GraphImport;
use crate::redo::RedoOp;

/// One group's redo, captured against the snapshot the statement read.
pub struct CommitGroup {
    pub ops: Vec<RedoOp>,
    pub snapshot_ts: u64,
    /// Timestamp admission assigned. The log rejects zero.
    pub commit_ts: u64,
}

/// The clock value the log assigned to one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Make `groups` durable. Each group already carries the commit
    /// timestamp admission assigned. Returns those timestamps with the
    /// log sequence number the append gave them.
    fn commit(&self, groups: &[CommitGroup]) -> Result<Vec<CommittedSlot>>;

    /// Publish a snapshot of `graph` at `snapshot_ts`, covering the log
    /// through `lsn`.
    fn checkpoint(&self, graph: &dyn GraphImport, lsn: u64, snapshot_ts: u64) -> Result<()>;

    /// Records with `lsn` greater than `from_lsn`.
    fn poll_follow(&self, from_lsn: u64) -> Result<FollowBatch>;

    /// The facade installs a reloaded query engine. This reopens the log
    /// against the snapshot the caller has already loaded into `graph`.
    fn reload(&self, graph: &mut dyn GraphImport) -> Result<()>;
}
