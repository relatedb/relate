//! The cut a checkpoint dumps from ([`CheckpointCut`]) and the rewrite
//! policy a shard writer follows ([`Rewrite`]).
//!
//! It lives in the codec tree, not in `checkpoint.rs`, because the codec
//! vocabulary is what names it: [`CheckpointDump`](super::CheckpointDump) is
//! `fn(&StoreTarget, &CheckpointCut<'_>, …)`, so the cut is the codec's one
//! graph argument, and `checkpoint.rs` reaches every codec through
//! `codec::codec_ops`. Keeping the type on the publication side would make
//! the two modules depend on each other; here the dependency runs one way,
//! `checkpoint.rs` → `codec`.

use relate_catalog::{ConstraintDef, IndexDef};
use relate_core::{Node, NodeId, RelId, Relationship, Result};
use relate_engine_api::{CheckpointDirty, CheckpointDirtyMarks, CheckpointTheater, GraphExport};

/// How far a checkpoint rewrites beyond the slots a dirty id seeds. Named
/// here with the cut's dump, not in the sharded writer: the image tail that
/// picks the codec's writer is compiled without that writer, and the sharded
/// writer is reached solely through `codec_ops`'s Parquet row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Rewrite {
    /// Absorb adjacent clean slots into each unit up to the byte target
    /// (ADR-0183 Decision 2) — compaction, for an explicit checkpoint.
    Coalesce,
    /// Rewrite the seeding slots alone and publish every clean part
    /// verbatim (ADR-0183 Decision 4) — no clean shard is read.
    Incremental,
}

/// What a dump freezes at its cut: the id counters, the catalog
/// projection (named indexes and constraints, the committed theater, the
/// graph type, the schema-mirror DDL) and the dirty ids the dump covers.
/// A checkpoint freezes this, so a commit that lands while the dump runs
/// cannot change what that dump publishes. It does not freeze rows, nor
/// `commit_ts` and `live_node_count`: those are live-graph reads served by
/// the cut at its pinned snapshot. Private to the cut — the cut is its
/// only consumer, so the engine-api surface does not carry it.
struct CheckpointImage {
    next_node_id: i64,
    next_rel_id: i64,
    named_indexes: Vec<IndexDef>,
    named_constraints: Vec<ConstraintDef>,
    theater: CheckpointTheater,
    graph_type: Option<String>,
    catalog_ddl: Vec<String>,
    dirty: CheckpointDirty,
}

impl CheckpointImage {
    /// Freeze the image from a graph the caller has already made stable: a
    /// checkpoint's cut holds the publication boundary the commits also
    /// take, and a single-threaded entry point needs no such guard. Every
    /// fallible read runs before the dirty-mark drain, so a failed freeze
    /// provably leaves the marks for the next checkpoint.
    ///
    /// The reads take the narrower [`GraphExport`]: the drain is the only
    /// use of the dirty-mark capability here.
    fn take(graph: &dyn CheckpointDirtyMarks) -> Result<Self> {
        let export: &dyn GraphExport = graph;
        let next_node_id = export.next_node_id();
        let next_rel_id = export.next_rel_id();
        let named_indexes = export.named_indexes();
        let named_constraints = export.named_constraints();
        let theater = export.checkpoint_theater_for_write()?;
        let graph_type = export.graph_type_spec()?;
        let catalog_ddl = export.catalog_ddl();
        let dirty = graph.take_checkpoint_dirty();
        Ok(Self {
            next_node_id,
            next_rel_id,
            named_indexes,
            named_constraints,
            theater,
            graph_type,
            catalog_ddl,
            dirty,
        })
    }
}

/// Everything a checkpoint's cut fixes together (ADR-0643): the graph the
/// dump reads rows from, the image of everything it publishes but those
/// rows, the ontology document (ADR-0120) the codecs publish, and the WAL
/// LSN and pinned timestamp the snapshot records. One type with one
/// constructor, so a caller cannot pair an image with another cut's LSN.
///
/// It is also the codec's one graph argument. Its surface splits one way:
/// the rows and `live_node_count` are live reads served at `snapshot_ts` (the
/// dump's MVCC view is what pins them), everything else comes from the frozen
/// image — the id counters, the catalog projection, the theater and the dirty
/// ids — so a commit that lands while the dump runs cannot change what the
/// dump publishes. The ontology document is frozen here beside the image
/// rather than traveling as a loose argument read outside the cut.
pub struct CheckpointCut<'a> {
    graph: &'a dyn CheckpointDirtyMarks,
    image: CheckpointImage,
    ontology: Option<String>,
    lsn: u64,
    snapshot_ts: u64,
}

impl<'a> CheckpointCut<'a> {
    /// Freeze `graph` into a cut at `snapshot_ts`, then drain its dirty
    /// marks. Every fallible read runs before the drain, so a failed freeze
    /// leaves the marks for the next checkpoint. The ontology document is
    /// frozen with the image: `take` runs under the checkpoint's exclusive
    /// pin, so the dump serves the document of its own cut.
    pub fn take(
        graph: &'a dyn CheckpointDirtyMarks,
        lsn: u64,
        snapshot_ts: u64,
        ontology: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            graph,
            image: CheckpointImage::take(graph)?,
            ontology: ontology.map(str::to_owned),
            lsn,
            snapshot_ts,
        })
    }

    pub(crate) fn lsn(&self) -> u64 {
        self.lsn
    }

    pub(crate) fn snapshot_ts(&self) -> u64 {
        self.snapshot_ts
    }

    /// The ontology document this cut's dump publishes (ADR-0120), frozen
    /// at [`take`] — `None` keeps the pre-ontology formats.
    pub(crate) fn ontology_document(&self) -> Option<&str> {
        self.ontology.as_deref()
    }

    /// Hand the cut's dirty ids back to the graph it was taken from after a
    /// failed dump, so the next checkpoint still covers them.
    pub(crate) fn restore_dirty_marks(&self) {
        self.graph.merge_checkpoint_dirty(&self.image.dirty);
    }

    /// Run one dump and, when it fails, hand the image's dirty ids back —
    /// the one marks-rollback protocol every dump entry runs (the facade's
    /// `checkpoint_at_image` and the Parquet writer's byte-targeted test
    /// entry alike), so no entry has to remember the hand-back itself. The
    /// hand-back is a guard, not an `inspect_err`: a dump that unwinds (a
    /// poisoned lock, an encoder expect) must restore the marks too, or the
    /// next checkpoint republishes the stale parts for those ids and its
    /// recycle deletes the WAL records that could have replayed the rows.
    pub(crate) fn dump_with_marks(&self, dump: impl FnOnce() -> Result<()>) -> Result<()> {
        let mut restore = DirtyRestore {
            cut: self,
            armed: true,
        };
        let result = dump();
        if result.is_ok() {
            restore.armed = false;
        }
        result
    }
}

/// The marks-rollback guard [`CheckpointCut::dump_with_marks`] arms around a
/// dump: while armed, its drop hands the cut's dirty ids back to the graph, so
/// a dump that returns an error and one that unwinds alike leave them for the
/// next checkpoint.
struct DirtyRestore<'c, 'g> {
    cut: &'c CheckpointCut<'g>,
    armed: bool,
}

impl Drop for DirtyRestore<'_, '_> {
    fn drop(&mut self) {
        if self.armed {
            self.cut.restore_dirty_marks();
        }
    }
}

impl GraphExport for CheckpointCut<'_> {
    /// The pinned timestamp, never a re-read of the live graph: every
    /// codec publishes `snapshot_ts` beside this, so the two cannot
    /// disagree about which commit the dump records.
    fn commit_ts(&self) -> u64 {
        self.snapshot_ts
    }
    fn next_node_id(&self) -> i64 {
        self.image.next_node_id
    }
    fn next_rel_id(&self) -> i64 {
        self.image.next_rel_id
    }
    fn live_node_count(&self) -> usize {
        self.graph.live_node_count()
    }
    fn checkpoint_dirty(&self) -> CheckpointDirty {
        self.image.dirty.clone()
    }
    // Every row read serves the cut's own pinned timestamp: a caller
    // passing another one gets the snapshot the cut froze, because that
    // is the only view the cut exists to publish. The `ts` parameter
    // stays because [`GraphExport`] is the shared export surface — the
    // concrete graph keeps the parameterized form.
    fn all_node_ids_at(&self, _ts: u64) -> Vec<NodeId> {
        self.graph.all_node_ids_at(self.snapshot_ts)
    }
    fn all_rel_ids_at(&self, _ts: u64) -> Vec<RelId> {
        self.graph.all_rel_ids_at(self.snapshot_ts)
    }
    fn node_at(&self, id: NodeId, _ts: u64) -> Option<Node> {
        self.graph.node_at(id, self.snapshot_ts)
    }
    fn rel_at(&self, id: RelId, _ts: u64) -> Option<Relationship> {
        self.graph.rel_at(id, self.snapshot_ts)
    }
    fn node_checkpoint_bytes(&self, id: NodeId, _ts: u64) -> Result<Option<u64>> {
        self.graph.node_checkpoint_bytes(id, self.snapshot_ts)
    }
    fn rel_checkpoint_bytes(&self, id: RelId, _ts: u64) -> Result<Option<u64>> {
        self.graph.rel_checkpoint_bytes(id, self.snapshot_ts)
    }
    fn named_indexes(&self) -> Vec<relate_catalog::IndexDef> {
        self.image.named_indexes.clone()
    }
    fn named_constraints(&self) -> Vec<relate_catalog::ConstraintDef> {
        self.image.named_constraints.clone()
    }
    fn checkpoint_theater_for_write(&self) -> Result<CheckpointTheater> {
        Ok(self.image.theater.clone())
    }
    fn graph_type_spec(&self) -> Result<Option<String>> {
        Ok(self.image.graph_type.clone())
    }
    fn catalog_ddl(&self) -> Vec<String> {
        self.image.catalog_ddl.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use relate_catalog::{ConstraintDef, IndexDef};
    use relate_core::{Node, NodeId, RelId, Relationship, Result};
    use relate_engine_api::{
        CheckpointDirty, CheckpointDirtyMarks, CheckpointTheater, GraphExport, TheaterData,
    };

    use super::{CheckpointCut, CheckpointImage};

    /// A graph whose theater read can fail, counting the dirty drains and
    /// the mark hand-backs.
    struct TestGraph {
        fail_theater: bool,
        takes: AtomicUsize,
        merges: AtomicUsize,
    }

    impl TestGraph {
        fn new(fail_theater: bool) -> Self {
            Self {
                fail_theater,
                takes: AtomicUsize::new(0),
                merges: AtomicUsize::new(0),
            }
        }
    }

    impl GraphExport for TestGraph {
        fn commit_ts(&self) -> u64 {
            1
        }
        fn next_node_id(&self) -> i64 {
            0
        }
        fn next_rel_id(&self) -> i64 {
            0
        }
        fn live_node_count(&self) -> usize {
            0
        }
        fn checkpoint_dirty(&self) -> CheckpointDirty {
            CheckpointDirty::default()
        }
        fn all_node_ids_at(&self, _ts: u64) -> Vec<NodeId> {
            Vec::new()
        }
        fn all_rel_ids_at(&self, _ts: u64) -> Vec<RelId> {
            Vec::new()
        }
        fn node_at(&self, _id: NodeId, _ts: u64) -> Option<Node> {
            None
        }
        fn rel_at(&self, _id: RelId, _ts: u64) -> Option<Relationship> {
            None
        }
        fn node_checkpoint_bytes(&self, _id: NodeId, _ts: u64) -> Result<Option<u64>> {
            Ok(None)
        }
        fn rel_checkpoint_bytes(&self, _id: RelId, _ts: u64) -> Result<Option<u64>> {
            Ok(None)
        }
        fn named_indexes(&self) -> Vec<IndexDef> {
            Vec::new()
        }
        fn named_constraints(&self) -> Vec<ConstraintDef> {
            Vec::new()
        }
        fn checkpoint_theater_for_write(&self) -> Result<CheckpointTheater> {
            if self.fail_theater {
                return Err(relate_core::Error::storage("theater refused"));
            }
            Ok(CheckpointTheater {
                databases: Default::default(),
                data: TheaterData::default(),
                legacy_unknown_constraints: Default::default(),
            })
        }
        fn graph_type_spec(&self) -> Result<Option<String>> {
            Ok(None)
        }
        fn catalog_ddl(&self) -> Vec<String> {
            Vec::new()
        }
    }

    impl CheckpointDirtyMarks for TestGraph {
        fn take_checkpoint_dirty(&self) -> CheckpointDirty {
            self.takes.fetch_add(1, Ordering::Relaxed);
            CheckpointDirty::default()
        }
        fn merge_checkpoint_dirty(&self, _dirty: &CheckpointDirty) {
            self.merges.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_failed_freeze_leaves_the_dirty_marks() {
        let failing = TestGraph::new(true);
        assert!(CheckpointImage::take(&failing).is_err());
        assert_eq!(
            failing.takes.load(Ordering::Relaxed),
            0,
            "the drain must not run before the freeze succeeded"
        );

        let frozen = TestGraph::new(false);
        assert!(CheckpointImage::take(&frozen).is_ok());
        assert_eq!(frozen.takes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_unwinding_dump_still_hands_the_dirty_marks_back() {
        let graph = TestGraph::new(false);
        let cut = CheckpointCut::take(&graph, 1, 1, None).expect("the cut freezes");
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cut.dump_with_marks(|| panic!("the dump blew up"));
        }));
        assert!(unwound.is_err(), "the dump's panic propagates");
        assert_eq!(
            graph.merges.load(Ordering::Relaxed),
            1,
            "an unwinding dump must restore the dirty marks, not drop them"
        );
    }
}
