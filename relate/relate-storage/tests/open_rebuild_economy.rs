//! LABS-1112: every materialization boundary pays the whole-graph index
//! rebuild exactly once, after the graph is final. `finish_restore` builds
//! only adjacency; the replaying open's single post-replay site and the
//! snapshot-only `load()` each own one rebuild — for a clean reopen
//! (snapshot, empty WAL), a crash reopen (snapshot plus WAL tail), a
//! WAL-only recovery and a bare snapshot load alike. The rebuild is
//! unconditional: a store can carry constraints with no live nodes, and
//! constraint keys must build. The label index the rebuild builds serves
//! the reopened graph's label counts.
//!
//! The rebuild count is the `test-hooks` seam on relate-executor (off in
//! every shipped build; enabled from this crate's dev-dependency) — the
//! policy under test is this crate's open flow, with no observable surface
//! besides the count.

use std::collections::BTreeMap;

use relate_core::{NodeId, Value};
use relate_executor::{take_index_rebuilds, Graph, RedoOp};
use relate_storage::{
    checkpoint_with_lsn, memory_store_with_codec, open_store, SnapshotCodecKind, Wal,
};

/// A labeled node create, as a database commit logs it. Every node carries a
/// unique name: the markdown vault requires a non-null unique name per
/// labeled node (ADR-0406), and it is inert for the other codecs.
fn create_person(id: NodeId, label: &str, name: &str) -> RedoOp {
    RedoOp::create_node(
        id,
        vec![label.into()],
        BTreeMap::from([("name".to_string(), Value::String(name.into()))]),
    )
}

/// Log `ops` and apply them to `writer`, as a database commit does.
fn commit(writer: &Graph, wal: &mut Wal, ops: Vec<RedoOp>) {
    let ts = writer.commit_ts() + 1;
    wal.append_commit(ts, &ops).unwrap();
    writer.apply_redo(ops, ts).unwrap();
}

/// Open the store and report this thread's whole-graph index rebuilds
/// during that open. The handle comes back with the count: the label
/// counts assert on the open they measure, never on a later one.
fn reopen(store: &relate_storage::StoreTarget) -> (Graph, u64) {
    take_index_rebuilds();
    let (graph, _) = open_store::<Graph>(store).unwrap();
    let rebuilds = take_index_rebuilds();
    (graph, rebuilds)
}

/// Every compiled root-layout codec: the rebuild policy is the open
/// flow's, codec-independent, and these tests carry only labeled nodes
/// (no relationship endpoints the markdown vault's id order would move).
fn codecs() -> Vec<SnapshotCodecKind> {
    SnapshotCodecKind::compiled().to_vec()
}

/// A clean close (snapshot, nothing past the checkpoint): one rebuild.
#[test]
fn clean_reopen_rebuilds_indexes_once() {
    for codec in codecs() {
        let store = memory_store_with_codec(codec).unwrap();
        let mut wal = Wal::open(store.vfs(), "wal", 0).unwrap();
        let writer = Graph::default();
        commit(
            &writer,
            &mut wal,
            vec![
                create_person(0, "Person", "a"),
                create_person(1, "Person", "b"),
                create_person(2, "Company", "c"),
            ],
        );
        checkpoint_with_lsn(&store, &writer, wal.lsn, None).unwrap();
        drop(wal);

        let (opened, rebuilds) = reopen(&store);
        assert_eq!(rebuilds, 1, "{codec:?}: the one post-replay rebuild");
        assert_eq!(opened.label_count("Person"), 2, "{codec:?}");
        assert_eq!(opened.label_count("Company"), 1, "{codec:?}");
    }
}

/// A crash close over a checkpointed base (snapshot plus WAL tail): still
/// one rebuild — the whole point of the split from `finish_restore`.
#[test]
fn snapshot_plus_tail_reopen_rebuilds_indexes_once() {
    for codec in codecs() {
        let store = memory_store_with_codec(codec).unwrap();
        let mut wal = Wal::open(store.vfs(), "wal", 0).unwrap();
        let writer = Graph::default();
        commit(&writer, &mut wal, vec![create_person(0, "Person", "a")]);
        checkpoint_with_lsn(&store, &writer, wal.lsn, None).unwrap();
        commit(
            &writer,
            &mut wal,
            vec![
                create_person(1, "Person", "b"),
                create_person(2, "Company", "c"),
            ],
        );
        drop(wal);

        let (opened, rebuilds) = reopen(&store);
        assert_eq!(
            rebuilds, 1,
            "{codec:?}: finish_restore builds adjacency only; the post-replay site is the one rebuild"
        );
        assert_eq!(opened.label_count("Person"), 2, "{codec:?}");
        assert_eq!(opened.label_count("Company"), 1, "{codec:?}");
    }
}

/// A WAL-only recovery (no snapshot): the replay's records are the only
/// content, and the post-replay rebuild indexes them.
#[test]
fn wal_only_reopen_rebuilds_indexes_once() {
    for codec in codecs() {
        let store = memory_store_with_codec(codec).unwrap();
        let mut wal = Wal::open(store.vfs(), "wal", 0).unwrap();
        let writer = Graph::default();
        commit(
            &writer,
            &mut wal,
            vec![
                create_person(0, "Person", "a"),
                create_person(1, "Person", "b"),
            ],
        );
        drop(wal);

        let (opened, rebuilds) = reopen(&store);
        assert_eq!(rebuilds, 1, "{codec:?}: the replayed tail is the content");
        assert_eq!(opened.label_count("Person"), 2, "{codec:?}");
    }
}

/// A checkpoint can carry rows at lsn 0 — applied redo no WAL append ever
/// logged (the sentinel suite's id-ceiling stores are this shape): the
/// graph's content, not the checkpoint lsn, is what says "index this".
#[test]
fn lsn_zero_checkpoint_with_rows_rebuilds_indexes_once() {
    for codec in codecs() {
        let store = memory_store_with_codec(codec).unwrap();
        let wal = Wal::open(store.vfs(), "wal", 0).unwrap();
        let writer = Graph::default();
        // The sentinel shape: redo applied to the writer, nothing appended.
        let ts = writer.commit_ts() + 1;
        writer
            .apply_redo(vec![create_person(0, "Person", "a")], ts)
            .unwrap();
        checkpoint_with_lsn(&store, &writer, 0, None).unwrap();
        drop(wal);

        let (opened, rebuilds) = reopen(&store);
        assert_eq!(
            rebuilds, 1,
            "{codec:?}: rows at lsn 0 still need the one rebuild"
        );
        assert_eq!(opened.label_count("Person"), 1, "{codec:?}");
    }
}

/// The snapshot-only `load()` boundary (no WAL replay) completes its own
/// indexes: the same one rebuild, and the label index serves reads.
#[test]
fn snapshot_only_load_completes_indexes_once() {
    for codec in codecs() {
        let store = memory_store_with_codec(codec).unwrap();
        let mut wal = Wal::open(store.vfs(), "wal", 0).unwrap();
        let writer = Graph::default();
        commit(&writer, &mut wal, vec![create_person(0, "Person", "a")]);
        checkpoint_with_lsn(&store, &writer, wal.lsn, None).unwrap();
        drop(wal);

        take_index_rebuilds();
        let graph = relate_storage::load::<Graph>(&store).unwrap();
        let rebuilds = take_index_rebuilds();
        assert_eq!(rebuilds, 1, "{codec:?}: load() owns its one rebuild");
        assert_eq!(graph.label_count("Person"), 1, "{codec:?}");
    }
}

/// An empty checkpointed store still rebuilds: the rebuild is
/// unconditional, so a store carrying constraints with no live nodes gets
/// its constraint keys built (the guard that skipped empty graphs left
/// them unenforced).
#[test]
fn empty_store_open_still_rebuilds_indexes_once() {
    for codec in codecs() {
        let store = memory_store_with_codec(codec).unwrap();
        let wal = Wal::open(store.vfs(), "wal", 0).unwrap();
        let writer = Graph::default();
        checkpoint_with_lsn(&store, &writer, wal.lsn, None).unwrap();
        drop(wal);

        let (opened, rebuilds) = reopen(&store);
        assert_eq!(rebuilds, 1, "{codec:?}: no skip — constraint keys build");
        assert_eq!(opened.label_count("Person"), 0, "{codec:?}");
    }
}
