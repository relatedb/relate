//! Roundtrip fidelity for the experimental codec (measurement feature,
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
    let loaded = crate::codec::rkyv::load_with_checkpoint_lsn(&store).unwrap();
    assert!(
        matches!(loaded.marker, crate::version::SnapshotMarker::Current),
        "an ontology-bearing rkyv store must reopen as Current, got {:?}",
        loaded.marker
    );
    assert_eq!(loaded.ontology.as_deref(), Some("(:Person {name: 'Ada'})"));
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
    let error = load(&store).expect_err("a corrupt archive must not load");
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
    let error = load(&store).expect_err("a bomb declaration must not load");
    assert!(
        error.to_string().contains("beyond the"),
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
    let error = load(&store).expect_err("a null theater must not load");
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
