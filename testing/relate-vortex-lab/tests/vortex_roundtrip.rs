//! The Vortex codec roundtrip (ADR-0665): create, checkpoint, write more so
//! the WAL carries records past the checkpoint LSN, then reopen — the whole
//! live graph must come back identically from a Native-vfs and a Memory-vfs
//! store. The codec's shard serialization lives in this crate, so the test
//! installs it first.

use std::collections::BTreeMap;

use relate_core::{NodeId, Value};
use relate_executor::{Graph, RedoOp};
use relate_storage::{
    checkpoint_with_lsn, memory_store_with_codec, open_store, parse_uri_with_options,
    shard_object_name, SnapshotCodecKind, StoreTarget, Wal,
};

fn create_person(id: NodeId, name: &str, age: i64) -> RedoOp {
    RedoOp::create_node(
        id,
        vec!["Person".into()],
        BTreeMap::from([
            ("name".to_string(), Value::String(name.into())),
            ("age".to_string(), Value::Int(age)),
        ]),
    )
}

/// Log `ops` and apply them to `writer`, as a database commit does.
fn commit(writer: &Graph, wal: &mut Wal, ops: Vec<RedoOp>) {
    let ts = writer.commit_ts() + 1;
    wal.append_commit(ts, &ops).unwrap();
    writer.apply_redo(ops, ts).unwrap();
}

/// Every live node's `(id, age)`, id-sorted — the answer a reopen must
/// reproduce.
fn answers(graph: &Graph) -> Vec<(i64, i64)> {
    let mut ids = graph.all_node_ids_at(graph.commit_ts());
    ids.sort_unstable();
    ids.into_iter()
        .map(|id| {
            let node = graph.node(id).expect("live node");
            let age = match node.properties.get("age") {
                Some(Value::Int(age)) => *age,
                other => panic!("node {id} has no integer age: {other:?}"),
            };
            (id, age)
        })
        .collect()
}

/// Create two rows, checkpoint, then commit a third past the checkpoint LSN,
/// reopen, and return the recovered answer with the checkpoint's LSN.
fn roundtrip(store: &StoreTarget) -> (Vec<(i64, i64)>, u64) {
    let (writer, mut wal) = open_store::<Graph>(store).unwrap();
    commit(
        &writer,
        &mut wal,
        vec![create_person(0, "Ada", 36), create_person(1, "Grace", 45)],
    );
    let checkpoint_lsn = wal.lsn;
    checkpoint_with_lsn(store, &writer, checkpoint_lsn, None).unwrap();
    // The WAL tail: a record past the checkpoint LSN the reopen must replay.
    commit(&writer, &mut wal, vec![create_person(2, "Alan", 41)]);
    drop(wal);

    let (reopened, _wal) = open_store::<Graph>(store).unwrap();
    assert_eq!(reopened.label_count("Person"), 3);
    (answers(&reopened), checkpoint_lsn)
}

#[test]
fn roundtrip_matches_on_native_and_memory_vfs() {
    relate_vortex_lab::engine::install_codec();

    let memory = memory_store_with_codec(SnapshotCodecKind::Vortex).unwrap();
    let (memory_answers, checkpoint_lsn) = roundtrip(&memory);
    // The checkpoint really is Vortex: the payload manifest and the node
    // shard the codec's own naming scheme predicts for this checkpoint.
    assert!(memory.vfs().exists("graph.vortex.json").unwrap());
    let objects = memory.vfs().list_dir("").unwrap();
    let node_shard = shard_object_name("nodes-", 0, checkpoint_lsn, None);
    assert!(
        objects.contains(&node_shard),
        "no node shard {node_shard} among {objects:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db");
    let native = parse_uri_with_options(
        db_path.to_str().expect("tempdir path is valid UTF-8"),
        None,
        Some(SnapshotCodecKind::Vortex),
    )
    .unwrap();
    let (native_answers, _) = roundtrip(&native);
    assert_eq!(native_answers, memory_answers);

    // The codec resolves from its evidence, with no requested codec: a
    // plain open of the directory infers Vortex and reads the same answers.
    drop(native);
    let inferred =
        relate_storage::parse_uri(db_path.to_str().expect("tempdir path is valid UTF-8")).unwrap();
    assert_eq!(inferred.snapshot_codec(), SnapshotCodecKind::Vortex);
    let (reopened, _wal) = open_store::<Graph>(&inferred).unwrap();
    assert_eq!(answers(&reopened), memory_answers);

    assert_eq!(
        memory_answers,
        vec![(0, 36), (1, 45), (2, 41)],
        "the WAL tail past the checkpoint LSN is replayed over the snapshot"
    );
}

#[test]
fn a_new_checkpoint_never_writes_a_live_shard_name() {
    // Two properties of the shard-naming scheme, both checked below: the
    // generations never share a name, and the retirement pass after the
    // second manifest removes the first generation's shards. Why that makes
    // a crash between the shard writes and the manifest swap harmless is
    // argued where it is enforced (the codec's `write_shards` /
    // `retire_orphaned_shards` docs), not observable from here.
    relate_vortex_lab::engine::install_codec();

    let memory = memory_store_with_codec(SnapshotCodecKind::Vortex).unwrap();
    let objects = || memory.vfs().list_dir("").unwrap();
    let (writer, mut wal) = open_store::<Graph>(&memory).unwrap();
    commit(&writer, &mut wal, vec![create_person(0, "Ada", 36)]);
    let first_lsn = wal.lsn;
    checkpoint_with_lsn(&memory, &writer, first_lsn, None).unwrap();
    let first = objects();
    let first_shard = shard_object_name("nodes-", 0, first_lsn, None);
    assert!(
        first.contains(&first_shard),
        "the first checkpoint published {first_shard}"
    );

    commit(&writer, &mut wal, vec![create_person(1, "Grace", 45)]);
    let second_lsn = wal.lsn;
    checkpoint_with_lsn(&memory, &writer, second_lsn, None).unwrap();
    let second = objects();
    let second_shard = shard_object_name("nodes-", 0, second_lsn, None);
    assert_ne!(
        first_shard, second_shard,
        "the generations share a name: {second_shard}"
    );
    assert!(
        second.contains(&second_shard),
        "the second checkpoint published {second_shard}"
    );
    assert!(
        !second.contains(&first_shard),
        "the first generation was retired: {first_shard} lives beside {second_shard}"
    );
}
