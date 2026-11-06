//! A write engine that records every trait call and forwards it. Facade
//! commit, follow, and checkpoint tests can wrap the local engine with this
//! to prove they reach the log through [`relate_engine_api::WriteEngine`].

use std::sync::Mutex;

use relate_core::Result;
use relate_engine_api::{
    CommitGroup, CommittedSlot, FollowBatch, GraphImport, WriteEngine,
};

/// Records the groups, checkpoints, polls, and reloads it forwards.
pub struct RecordingWriteEngine<E> {
    inner: E,
    commits: Mutex<Vec<Vec<CommittedSlot>>>,
    checkpoints: Mutex<u64>,
    polls: Mutex<u64>,
    reloads: Mutex<u64>,
}

impl<E> RecordingWriteEngine<E> {
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            commits: Mutex::new(Vec::new()),
            checkpoints: Mutex::new(0),
            polls: Mutex::new(0),
            reloads: Mutex::new(0),
        }
    }

    pub fn commits(&self) -> Vec<Vec<CommittedSlot>> {
        self.commits.lock().expect("recording lock").clone()
    }

    pub fn checkpoints(&self) -> u64 {
        *self.checkpoints.lock().expect("recording lock")
    }

    pub fn polls(&self) -> u64 {
        *self.polls.lock().expect("recording lock")
    }

    pub fn reloads(&self) -> u64 {
        *self.reloads.lock().expect("recording lock")
    }
}

impl<E: WriteEngine> WriteEngine for RecordingWriteEngine<E> {
    fn commit(&self, groups: &[CommitGroup]) -> Result<Vec<CommittedSlot>> {
        let slots = self.inner.commit(groups)?;
        self.commits.lock().expect("recording lock").push(slots.clone());
        Ok(slots)
    }

    fn checkpoint(&self, graph: &dyn GraphImport, lsn: u64, snapshot_ts: u64) -> Result<()> {
        let result = self.inner.checkpoint(graph, lsn, snapshot_ts);
        *self.checkpoints.lock().expect("recording lock") += 1;
        result
    }

    fn poll_follow(&self, from_lsn: u64) -> Result<FollowBatch> {
        *self.polls.lock().expect("recording lock") += 1;
        self.inner.poll_follow(from_lsn)
    }

    fn reload(&self, graph: &mut dyn GraphImport) -> Result<()> {
        let result = self.inner.reload(graph);
        *self.reloads.lock().expect("recording lock") += 1;
        result
    }
}

#[cfg(test)]
mod tests {
    use relate_engine_api::{CommitGroup, RedoOp, WriteEngine};
    use relate_storage::{memory_store_with_codec, SnapshotCodecKind, Wal};
    use relate_write_local::LocalWriteEngine;

    use super::RecordingWriteEngine;

    #[test]
    fn a_commit_and_a_poll_go_through_the_trait() {
        let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
        let engine = RecordingWriteEngine::new(LocalWriteEngine::new(store, Wal::memory()));
        let slots = engine
            .commit(&[CommitGroup {
                ops: vec![RedoOp::create_node(
                    0,
                    vec!["N".into()],
                    relate_core::PropertyMap::default(),
                )],
                snapshot_ts: 0,
                commit_ts: 1,
            }])
            .unwrap();
        assert_eq!(slots, vec![relate_engine_api::CommittedSlot { lsn: 1, commit_ts: 1 }]);
        assert_eq!(engine.commits().len(), 1);
        // The writer log refuses a follow poll. The trait still observed it.
        assert!(engine.poll_follow(0).is_err());
        assert_eq!(engine.polls(), 1);
    }
}
