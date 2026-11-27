//! The Vortex codec roundtrip at scale: a graph large enough to span many
//! row shards (SHARD_ROWS = 4096) checkpointed and reopened, with a WAL
//! tail past the checkpoint LSN — and the reopened answers compared
//! against the default codec's, built from the identical op stream. The
//! small roundtrip test proves the format markers; this one proves the
//! codec holds up when the shard count, the column count and the replay
//! tail all exceed one.

use std::collections::BTreeMap;

use relate_core::{NodeId, RelId, Value};
use relate_engine_api::RedoOp;
use relate_executor::Graph;
use relate_storage::{
    checkpoint_with_lsn, memory_store_with_codec, open_store, parse_uri_with_options,
    SnapshotCodecKind, StoreTarget, Wal,
};

/// Three labels, two property shapes (string + int), two rel types: enough
/// variety that a per-column encoding mistake cannot cancel out.
const NODES: i64 = 20_000;
const RELS: i64 = 40_000;
/// Rows committed after the checkpoint, replayed from the WAL on reopen.
const TAIL_ROWS: i64 = 500;
/// The corpus invariant this test exists for: the rows span more than one
/// 4096-row shard, so the checkpoint publishes multiple shard objects.
const _: () = assert!(NODES + TAIL_ROWS > 4096, "the corpus must span > 1 shard");

fn node_op(id: NodeId, name: String, age: i64) -> RedoOp {
    let labels = match id % 3 {
        0 => vec!["Person".to_string()],
        1 => vec!["Person".to_string(), "Actor".to_string()],
        _ => vec!["Movie".to_string()],
    };
    RedoOp::create_node(
        id,
        labels,
        BTreeMap::from([
            ("name".to_string(), Value::String(name.into())),
            ("age".to_string(), Value::Int(age)),
            ("seq".to_string(), Value::Int(id)),
        ]),
    )
}

fn rel_op(id: RelId, start: NodeId, end: NodeId) -> RedoOp {
    let rel_type = if id % 2 == 0 { "ACTED_IN" } else { "DIRECTED" };
    RedoOp::create_rel(
        id,
        start,
        end,
        rel_type.to_string(),
        BTreeMap::from([("weight".to_string(), Value::Int(id % 97))]),
    )
}

/// The op stream: every node, every relationship, then the post-checkpoint
/// tail as a second batch. One stream, built once, applied to every store
/// so the comparison is over identical inputs.
fn ops() -> Vec<Vec<RedoOp>> {
    let nodes: Vec<RedoOp> = (0..NODES)
        .map(|id| node_op(id, format!("n{id}"), 20 + id % 80))
        .collect();
    let rels: Vec<RedoOp> = (0..RELS)
        .map(|i| rel_op(i, (i * 7) % NODES, (i * 13 + 1) % NODES))
        .collect();
    let tail: Vec<RedoOp> = (0..TAIL_ROWS)
        .map(|i| {
            let id = NODES + i;
            node_op(id, format!("tail{id}"), 60 + (i % 40))
        })
        .collect();
    vec![nodes, rels, tail]
}

/// Log `ops` and apply them to `writer`, as a database commit does.
fn commit(writer: &Graph, wal: &mut Wal, ops: Vec<RedoOp>) {
    let ts = writer.commit_ts() + 1;
    wal.append_commit(ts, &ops).unwrap();
    writer.apply_redo(ops, ts).unwrap();
}

/// Build the graph, checkpoint after the first two batches, commit the
/// tail past the checkpoint LSN, reopen, and return the reopened digest —
/// plus the pre-checkpoint digest for the identity assertion.
fn run(store: &StoreTarget, batches: &[Vec<RedoOp>]) -> (String, String) {
    let (writer, mut wal) = open_store::<Graph>(store).unwrap();
    commit(&writer, &mut wal, batches[0].clone());
    commit(&writer, &mut wal, batches[1].clone());
    let before = digest(&writer);
    checkpoint_with_lsn(store, &writer, wal.lsn, None).unwrap();
    commit(&writer, &mut wal, batches[2].clone());
    drop(wal);

    let (reopened, _wal) = open_store::<Graph>(store).unwrap();
    let after = digest(&reopened);
    (before, after)
}

/// The whole graph as one comparable string: every node's labels and
/// properties, every relationship's endpoints and properties, id-sorted —
/// a reopen (or another codec) must reproduce it byte for byte.
fn digest(graph: &Graph) -> String {
    let mut out = String::new();
    let mut node_ids = graph.all_node_ids_at(graph.commit_ts());
    node_ids.sort_unstable();
    for id in node_ids {
        let node = graph.node(id).expect("live node");
        let mut labels = node.labels.as_ref().clone();
        labels.sort();
        let props = format!("{:?}", node.properties);
        out.push_str(&format!("{id} {:?} {props}\n", labels));
    }
    let mut rel_ids = graph.all_rel_ids_at(graph.commit_ts());
    rel_ids.sort_unstable();
    for id in rel_ids {
        let rel = graph.rel(id).expect("live rel");
        out.push_str(&format!(
            "{id} {} {} {} {:?}\n",
            rel.start, rel.end, rel.rel_type, rel.properties
        ));
    }
    out
}

#[test]
fn vortex_roundtrip_at_scale_matches_the_default_codec() {
    relate_vortex_lab::engine::install_codec();
    let batches = ops();
    // The default codec's answers from the identical op stream.
    let parquet = memory_store_with_codec(SnapshotCodecKind::Parquet).unwrap();
    let (parquet_before, parquet_after) = run(&parquet, &batches);

    // The Vortex codec, memory VFS.
    let vortex = memory_store_with_codec(SnapshotCodecKind::Vortex).unwrap();
    let (vortex_before, vortex_after) = run(&vortex, &batches);
    assert_eq!(
        vortex_before, parquet_before,
        "the pre-checkpoint graphs agree"
    );
    assert_eq!(vortex_after, parquet_after, "the reopened graphs agree");

    // The WAL tail really was replayed over the snapshot, not lost.
    assert_ne!(vortex_before, vortex_after, "the tail changed the graph");
    assert!(
        vortex_after.matches("tail").count() >= TAIL_ROWS as usize,
        "every tail row is back after the reopen"
    );

    // The Vortex codec, native VFS: same answers off real files.
    let dir = tempfile::tempdir().unwrap();
    let native = parse_uri_with_options(
        dir.path().join("db").to_str().expect("tempdir is UTF-8"),
        None,
        Some(SnapshotCodecKind::Vortex),
    )
    .unwrap();
    let (native_before, native_after) = run(&native, &batches);
    assert_eq!(native_before, parquet_before);
    assert_eq!(native_after, parquet_after);

    // The checkpoint really is many row shards (SHARD_ROWS = 4096): a
    // single-shard write would mean the chunking path never ran. Shard
    // names carry the checkpoint's LSN (crash safety: no live name is
    // ever overwritten), so the count matches the prefix.
    let shard_count = native
        .vfs()
        .list_dir("")
        .unwrap()
        .into_iter()
        .filter(|name| name.starts_with("nodes-") && name.ends_with(".vortex"))
        .count();
    assert!(
        shard_count as i64 >= NODES / 4096,
        "expected at least {} node shards, found {shard_count}",
        NODES / 4096
    );
}
