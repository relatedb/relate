//! Roundtrip fidelity for the experimental codecs (measurement features,
//! default off). Each test compares a reload through the experimental
//! codec against the same graph reloaded through the JSON codec, so the
//! reference is the codec that shares their property conversion.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Node, Relationship, Value};
use relate_executor::Graph;
use relate_test_support::graph::{props, signature};

use crate::{checkpoint, load, memory_store_with_codec, SnapshotCodecKind};

/// A graph exercising the value vocabulary the cypher-json bridge carries:
/// labels, a relationship type, and null/bool/int/float/string/list/map
/// properties.
fn rich() -> Graph {
    let graph = Graph::default();
    graph
        .load_checkpoint_node(Node {
            id: 1,
            labels: Arc::new(vec!["Person".into(), "Employee".into()]),
            properties: props(vec![
                ("name", Value::String("Ada".into())),
                ("age", Value::Int(36)),
                ("score", Value::Float(1.5)),
                ("active", Value::Bool(true)),
                ("missing", Value::Null),
                (
                    "tags",
                    Value::List(Arc::new(vec![
                        Value::String("a".into()),
                        Value::String("b".into()),
                    ])),
                ),
                (
                    "address",
                    Value::Map(Arc::new(BTreeMap::from([(
                        "city".to_owned(),
                        Value::String("London".into()),
                    )]))),
                ),
            ]),
        })
        .unwrap();
    graph
        .load_checkpoint_node(Node {
            id: 2,
            labels: Arc::new(vec!["Person".into()]),
            properties: props(vec![("name", Value::String("Grace".into()))]),
        })
        .unwrap();
    graph
        .load_checkpoint_rel(Relationship {
            id: 1,
            start: 1,
            end: 2,
            rel_type: "KNOWS".into(),
            properties: props(vec![("since", Value::Int(2020))]),
        })
        .unwrap();
    graph.set_next_node_id(3);
    graph.set_next_rel_id(2);
    graph.set_commit_ts(1);
    graph
}

/// The fjall tests' one store fixture: a native tempdir target with the
/// fjall codec. The TempDir is returned so it outlives the target.
#[cfg(fjall_codec)]
fn fjall_target() -> (tempfile::TempDir, crate::StoreTarget) {
    let dir = tempfile::tempdir().unwrap();
    let target = crate::parse_uri_with_options(
        dir.path().join("db").to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Fjall),
    )
    .unwrap();
    (dir, target)
}

fn json_reference() -> String {
    let graph = rich();
    let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    checkpoint(&store, &graph, None).unwrap();
    signature(&load(&store).unwrap())
}

#[cfg(rkyv_codec)]
#[test]
fn rkyv_roundtrips_every_row_and_property_type() {
    let graph = rich();
    let store = memory_store_with_codec(SnapshotCodecKind::Rkyv).unwrap();
    checkpoint(&store, &graph, None).unwrap();
    let reloaded = load(&store).unwrap();
    assert_eq!(signature(&reloaded), signature(&graph));
    assert_eq!(signature(&reloaded), json_reference());
}

#[cfg(fjall_codec)]
#[test]
fn fjall_roundtrips_every_row_and_property_type() {
    let graph = rich();
    let (_dir, target) = fjall_target();
    checkpoint(&target, &graph, None).unwrap();
    let reloaded = load(&target).unwrap();
    assert_eq!(signature(&reloaded), signature(&graph));
    assert_eq!(signature(&reloaded), json_reference());
}

#[cfg(fjall_codec)]
#[test]
fn fjall_refuses_a_store_without_a_local_directory() {
    // A `:memory:` store has no native root: the codec fails closed
    // rather than writing nothing.
    let graph = rich();
    let store = memory_store_with_codec(SnapshotCodecKind::Fjall).unwrap();
    let error = checkpoint(&store, &graph, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires a local filesystem store"),
        "unexpected error: {error}"
    );
}

#[cfg(rkyv_codec)]
#[test]
fn rkyv_evidence_refuses_a_mixed_store() {
    // A directory carrying both an rkyv archive and a JSON snapshot is
    // incoherent evidence and must fail at resolution, like every other
    // mixed pair.
    let graph = rich();
    let dir = tempfile::tempdir().unwrap();
    let json_target = crate::parse_uri_with_options(
        dir.path().join("db").to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Json),
    )
    .unwrap();
    checkpoint(&json_target, &graph, None).unwrap();
    // The archive written beside the JSON store: two codecs' evidence in
    // one directory. (Opening an rkyv target *on* a JSON store is refused
    // earlier, by the codec-mismatch guard — that is the other half of
    // this policy, and it needs no directory.)
    crate::codec::rkyv::dump(&json_target, &graph, 1, graph.commit_ts(), None).unwrap();
    // Release the writer lock before resolving the directory afresh.
    drop(json_target);
    let error = crate::parse_uri(dir.path().join("db").to_str().unwrap())
        .err()
        .expect("a mixed store must not open");
    assert!(
        error.to_string().contains("mixed snapshot codecs"),
        "unexpected error: {error}"
    );
}

/// The format-marker regression: an ontology-bearing checkpoint must
/// reopen as `Current`, not `Legacy` — a Legacy marker triggers a
/// migration checkpoint on every open. The marker classifies against the
/// manifest's own ontology flag, exactly as the writer derived the
/// version from it.
#[cfg(rkyv_codec)]
#[test]
fn rkyv_ontology_checkpoint_reopens_current() {
    let graph = rich();
    let store = memory_store_with_codec(SnapshotCodecKind::Rkyv).unwrap();
    checkpoint(&store, &graph, Some("(:Person {name: 'Ada'})")).unwrap();
    let loaded = crate::codec::rkyv::load_with_checkpoint_lsn::<Graph>(&store).unwrap();
    assert!(
        matches!(loaded.marker, crate::version::SnapshotMarker::Current),
        "an ontology-bearing rkyv store must reopen as Current, got {:?}",
        loaded.marker
    );
    assert_eq!(loaded.ontology.as_deref(), Some("(:Person {name: 'Ada'})"));
}

/// The fjall twin of the marker regression above.
#[cfg(fjall_codec)]
#[test]
fn fjall_ontology_checkpoint_reopens_current() {
    let graph = rich();
    let (_dir, target) = fjall_target();
    checkpoint(&target, &graph, Some("(:Person {name: 'Ada'})")).unwrap();
    let loaded = crate::codec::fjall::load_with_checkpoint_lsn::<Graph>(&target).unwrap();
    assert!(
        matches!(loaded.marker, crate::version::SnapshotMarker::Current),
        "an ontology-bearing fjall store must reopen as Current, got {:?}",
        loaded.marker
    );
    assert_eq!(loaded.ontology.as_deref(), Some("(:Person {name: 'Ada'})"));
}

/// The replacement-semantics regression: a checkpoint of a smaller graph
/// must not leave the vanished rows in the keyspace — an insert-only
/// dump resurrects them on the next open.
#[cfg(fjall_codec)]
#[test]
fn fjall_shrinking_checkpoint_does_not_resurrect_rows() {
    let full = rich();
    let (_dir, target) = fjall_target();
    checkpoint(&target, &full, None).unwrap();
    // The shrunk graph: node 2 and the relationship gone, counters wound
    // back to what the remaining rows imply.
    let shrunk = Graph::default();
    shrunk
        .load_checkpoint_node(Node {
            id: 1,
            labels: Arc::new(vec!["Person".into(), "Employee".into()]),
            properties: props(vec![("name", Value::String("Ada".into()))]),
        })
        .unwrap();
    shrunk.set_next_node_id(2);
    shrunk.set_next_rel_id(1);
    shrunk.set_commit_ts(2);
    checkpoint(&target, &shrunk, None).unwrap();
    let reloaded = load(&target).unwrap();
    assert_eq!(signature(&reloaded), signature(&shrunk));
}

/// The rkyv load validates its archive: a corrupted store file is a typed
/// error, never undefined behaviour.
#[cfg(rkyv_codec)]
#[test]
fn rkyv_corrupt_archive_fails_closed() {
    let graph = rich();
    let store = memory_store_with_codec(SnapshotCodecKind::Rkyv).unwrap();
    checkpoint(&store, &graph, None).unwrap();
    // Flip the archive's bytes: still lz4-decodable, no longer a valid
    // archive.
    let mut bytes = crate::snapshot::get_object(&store, crate::codec::GRAPH_RKYV).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    crate::snapshot::put_object(&store, crate::codec::GRAPH_RKYV, &bytes).unwrap();
    let error = load::<Graph>(&store).expect_err("a corrupt archive must not load");
    assert!(
        error
            .to_string()
            .contains("rkyv snapshot validation failed")
            || error
                .to_string()
                .contains("rkyv snapshot decompression failed"),
        "unexpected error: {error}"
    );
}

/// A declared decompressed length beyond what the compressed block can
/// expand to is refused before allocation (the decompression-bomb bound).
#[cfg(rkyv_codec)]
#[test]
fn rkyv_declared_length_beyond_the_block_ceiling_fails_closed() {
    let graph = rich();
    let store = memory_store_with_codec(SnapshotCodecKind::Rkyv).unwrap();
    checkpoint(&store, &graph, None).unwrap();
    // A tiny block claiming four gigabytes of output.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 16]);
    crate::snapshot::put_object(&store, crate::codec::GRAPH_RKYV, &bytes).unwrap();
    let error = load::<Graph>(&store).expect_err("a bomb declaration must not load");
    assert!(
        error.to_string().contains("beyond the"),
        "unexpected error: {error}"
    );
}

/// A fjall manifest whose keyspace directory is gone is a damaged store,
/// not a fresh one: serving an empty graph would let the next checkpoint
/// publish that emptiness over the real data.
#[cfg(fjall_codec)]
#[test]
fn fjall_manifest_without_keyspace_fails_closed() {
    let graph = rich();
    let (_dir, target) = fjall_target();
    checkpoint(&target, &graph, None).unwrap();
    std::fs::remove_dir_all(
        target
            .native_root
            .as_ref()
            .unwrap()
            .join(crate::codec::fjall::FJALL_DIR),
    )
    .unwrap();
    let error = load::<Graph>(&target).expect_err("a damaged store must not load");
    assert!(
        error.to_string().contains("damaged"),
        "unexpected error: {error}"
    );
}

/// An explicit `null` theater is a corrupt image: absent means "no
/// theater", null means the field was written wrong.
#[cfg(rkyv_codec)]
#[test]
fn rkyv_null_theater_fails_closed() {
    let graph = rich();
    let store = memory_store_with_codec(SnapshotCodecKind::Rkyv).unwrap();
    checkpoint(&store, &graph, None).unwrap();
    let text = String::from_utf8(
        crate::snapshot::get_object(&store, crate::snapshot::MANIFEST_JSON).unwrap(),
    )
    .unwrap();
    let forged = text.replacen('{', "{\"theater\": null,", 1);
    crate::snapshot::put_object(&store, crate::snapshot::MANIFEST_JSON, forged.as_bytes()).unwrap();
    let error = load::<Graph>(&store).expect_err("a null theater must not load");
    // The refusal is the canonical theater decoder's (the one the Parquet
    // and markdown manifests share): an explicit null is not a
    // CheckpointTheater, so the manifest fails to parse.
    assert!(
        error
            .to_string()
            .contains("expected struct CheckpointTheater"),
        "unexpected error: {error}"
    );
}

/// A symlinked keyspace directory is refused: fjall writes through
/// `std::fs`, so the VFS's below-root symlink policy does not cover it and
/// a crafted store could otherwise redirect keyspace writes outside the
/// store root.
#[cfg(all(unix, fjall_codec))]
#[test]
fn fjall_refuses_a_symlinked_keyspace() {
    let graph = rich();
    let (_dir, target) = fjall_target();
    // A real fjall store first, so the store resolves as one; then its
    // keyspace is replaced by a symlink pointing outside the store.
    checkpoint(&target, &graph, None).unwrap();
    let root = target.native_root.clone().unwrap();
    let keyspace = root.join(crate::codec::fjall::FJALL_DIR);
    let outside = tempfile::tempdir().unwrap();
    std::fs::remove_dir_all(&keyspace).unwrap();
    std::os::unix::fs::symlink(outside.path(), &keyspace).unwrap();
    let error =
        checkpoint(&target, &graph, None).expect_err("a symlinked keyspace must not be written");
    assert!(
        error.to_string().contains("is a symlink"),
        "unexpected error: {error}"
    );
    // The redirect target is untouched.
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

/// A read-only open (ADR-0136) refuses the fjall codec at resolution:
/// fjall has no read-only keyspace open, so serving one would take an
/// exclusive lock and create or recover files — a write, on an open that
/// promised none.
#[cfg(fjall_codec)]
#[test]
fn fjall_refuses_a_read_only_open() {
    let graph = rich();
    let (dir, target) = fjall_target();
    checkpoint(&target, &graph, None).unwrap();
    let error = crate::parse_uri_opening(dir.path().join("db").to_str().unwrap(), None, None, true)
        .err()
        .expect("a read-only open must refuse the fjall codec");
    assert!(
        error.to_string().contains("read-only"),
        "unexpected error: {error}"
    );
}

/// A symlink *inside* the keyspace is refused too: the top-level guard
/// covers the `fjall/` entry itself, but fjall creates and recovers
/// partition files throughout the directory it opens, so a symlinked
/// partition would redirect those writes outside the store root.
#[cfg(all(unix, fjall_codec))]
#[test]
fn fjall_refuses_a_symlinked_partition_inside_the_keyspace() {
    let graph = rich();
    let (_dir, target) = fjall_target();
    checkpoint(&target, &graph, None).unwrap();
    let root = target.native_root.clone().unwrap();
    let keyspace = root.join(crate::codec::fjall::FJALL_DIR);
    let outside = tempfile::tempdir().unwrap();
    // A symlinked partition directory inside an otherwise real keyspace
    // (fjall keeps partitions under a `partitions/` subdirectory).
    let nodes = keyspace.join("partitions").join("nodes");
    std::fs::remove_dir_all(&nodes).unwrap();
    std::os::unix::fs::symlink(outside.path(), &nodes).unwrap();
    let error = load::<Graph>(&target).expect_err("a symlinked partition must not be opened");
    assert!(
        error.to_string().contains("refuses to follow"),
        "unexpected error: {error}"
    );
    // The redirect target is untouched.
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}
