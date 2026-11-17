use super::*;
use crate::packstream::{decode, encode, MAX_DEPTH};
use crate::testutil::wrapped_in_lists;
use relate::Value;

/// ADR-0328: a byte array result is PackStream Bytes, with the shortest
/// length marker, at any depth.
#[test]
fn bytes_results_are_packstream_bytes() {
    let small = Value::Bytes(vec![0x00, 0x01, 0xFF].into());
    let pack = Pack::from_mdb(&small, ValueCodec::plain_44()).unwrap();
    assert_eq!(pack, Pack::Bytes(vec![0x00, 0x01, 0xFF]));
    assert_eq!(encode(&pack).unwrap(), vec![0xCC, 0x03, 0x00, 0x01, 0xFF]);
    let medium = Value::Bytes(vec![7; 256].into());
    assert_eq!(
        encode(&Pack::from_mdb(&medium, ValueCodec::plain_44()).unwrap()).unwrap()[..3],
        [0xCD, 0x01, 0x00]
    );
    let large = Value::Bytes(vec![7; 65_536].into());
    assert_eq!(
        encode(&Pack::from_mdb(&large, ValueCodec::plain_44()).unwrap()).unwrap()[..5],
        [0xCE, 0x00, 0x01, 0x00, 0x00]
    );
    let nested = Value::List(vec![small].into());
    assert_eq!(
        Pack::from_mdb(&nested, ValueCodec::plain_44()).unwrap(),
        Pack::List(vec![Pack::Bytes(vec![0x00, 0x01, 0xFF])])
    );
}

#[test]
fn encode_depth_limit_matches_decode() {
    fn nested_pack(depth: usize) -> Pack {
        let mut p = Pack::Null;
        for _ in 0..depth {
            p = Pack::List(vec![p]);
        }
        p
    }

    // The root container is depth 1: 128 nested containers are accepted on
    // both the value-conversion and the wire-encoding paths.
    let at_limit = nested_value(MAX_DEPTH);
    let pack =
        Pack::from_mdb(&at_limit, ValueCodec::plain_44()).expect("128 nested containers convert");
    assert_eq!(pack, nested_pack(MAX_DEPTH));
    let bytes = encode(&pack).expect("128 nested containers encode");
    assert_eq!(decode(&bytes).unwrap(), pack);

    // The 129th nested container is rejected before any stack overflow.
    let over_limit = nested_value(MAX_DEPTH + 1);
    assert_eq!(
        Pack::from_mdb(&over_limit, ValueCodec::plain_44()),
        Err(MdbPackError::DepthExceeded)
    );
    assert_eq!(
        encode(&nested_pack(MAX_DEPTH + 1)),
        Err("packstream nesting limit exceeded".into())
    );
}

/// ADR-0462 vs Bolt's ADR-0110 packstream limit: the engine accepts values
/// up to `MAX_VALUE_DEPTH` (500) container levels, far past Bolt's 128-level
/// wire limit. Encoding such a result is a typed `MdbPackError::DepthExceeded`
/// refusal — not an abort — before any recursion overruns the stack.
#[test]
fn engine_depth_cap_value_is_a_typed_bolt_refusal() {
    let value = wrapped_in_lists(relate::Value::Null, relate::MAX_VALUE_DEPTH as usize);
    assert!(value.depth_within(relate::MAX_VALUE_DEPTH));
    assert_eq!(
        Pack::from_mdb(&value, ValueCodec::plain_44()),
        Err(MdbPackError::DepthExceeded)
    );
}

/// `Null` wrapped in `depth` nested lists — the depth-limit test's base
/// over [`wrapped_in_lists`].
fn nested_value(depth: usize) -> relate::Value {
    wrapped_in_lists(relate::Value::Null, depth)
}

fn test_node(id: i64) -> relate::Node {
    relate::Node {
        id,
        labels: vec![format!("L{id}")].into(),
        properties: relate_core::PropertyMap::share(std::collections::BTreeMap::new()),
    }
}

fn test_rel(id: i64, start: i64, end: i64) -> relate::Relationship {
    relate::Relationship {
        id,
        start,
        end,
        rel_type: format!("T{id}"),
        properties: relate_core::PropertyMap::share(std::collections::BTreeMap::new()),
    }
}

fn test_path(nodes: Vec<relate::Node>, rels: Vec<relate::Relationship>) -> relate::Value {
    relate::Value::Path(relate::Path { nodes, rels }.into())
}

/// The (nodes, relationships, sequence) lists of the encoded `0x50` Path.
fn path_fields(path: &relate::Value) -> (Vec<Pack>, Vec<Pack>, Vec<Pack>) {
    let Pack::Struct { tag, fields } = Pack::from_mdb(path, ValueCodec::plain_44()).unwrap() else {
        panic!("path must encode as a struct");
    };
    assert_eq!(tag, 0x50, "Bolt Path tag");
    assert_eq!(fields.len(), 3, "Path has nodes, relationships, sequence");
    let Pack::List(nodes) = fields[0].clone() else {
        panic!("nodes field must be a list");
    };
    let Pack::List(rels) = fields[1].clone() else {
        panic!("relationships field must be a list");
    };
    let Pack::List(seq) = fields[2].clone() else {
        panic!("sequence field must be a list");
    };
    (nodes, rels, seq)
}

#[test]
fn path_single_forward_hop_is_struct_0x50_with_unbound_rel_0x72() {
    let path = test_path(vec![test_node(1), test_node(2)], vec![test_rel(10, 1, 2)]);
    let (nodes, rels, seq) = path_fields(&path);
    assert_eq!(nodes.len(), 2);
    assert!(
        matches!(nodes[0], Pack::Struct { tag: 0x4E, .. }),
        "nodes reuse the 0x4E Node struct"
    );
    // UnboundRelationship drops start/end: [id, rel_type, properties].
    assert_eq!(rels.len(), 1);
    assert_eq!(
        rels[0],
        Pack::Struct {
            tag: 0x72,
            fields: vec![Pack::Int(10), Pack::String("T10".into()), Pack::Map(vec![])],
        }
    );
    assert_eq!(seq, vec![Pack::Int(1), Pack::Int(1)]);

    // The wire form opens with a 3-field struct and the 0x50 tag.
    let pack = Pack::from_mdb(&path, ValueCodec::plain_44()).unwrap();
    let bytes = encode(&pack).unwrap();
    assert_eq!(&bytes[..2], &[0xB3, 0x50]);
    assert_eq!(decode(&bytes).unwrap(), pack);
}

#[test]
fn path_reverse_hop_negates_relationship_index() {
    // Traversal node 1 -> node 2, but the relationship runs 2 -> 1.
    let path = test_path(vec![test_node(1), test_node(2)], vec![test_rel(10, 2, 1)]);
    let (_, _, seq) = path_fields(&path);
    assert_eq!(seq, vec![Pack::Int(-1), Pack::Int(1)]);
}

#[test]
fn path_multi_hop_mixed_direction_sequence() {
    // nodes 1 -> 2 -> 3; hop 0 uses rel 1->2 (forward), hop 1 uses rel
    // 3->2, so it is traversed against its direction.
    let path = test_path(
        vec![test_node(1), test_node(2), test_node(3)],
        vec![test_rel(10, 1, 2), test_rel(11, 3, 2)],
    );
    let (_, rels, seq) = path_fields(&path);
    assert_eq!(rels.len(), 2);
    assert_eq!(
        seq,
        vec![Pack::Int(1), Pack::Int(1), Pack::Int(-2), Pack::Int(2)]
    );
}

#[test]
fn path_broken_invariant_errors_instead_of_silently_encoding() {
    // The relationship connects neither nodes[0] nor nodes[1]: no sign is
    // correct, so encoding must fail loudly and name the invariant.
    let path = test_path(vec![test_node(1), test_node(2)], vec![test_rel(10, 3, 4)]);
    let err = Pack::from_mdb(&path, ValueCodec::plain_44()).unwrap_err();
    assert!(matches!(err, MdbPackError::PathInvariant(_)), "{err}");
    let err = err.to_string();
    assert!(err.contains("path invariant broken"), "{err}");
    assert!(err.contains("rel 10"), "{err}");
    assert!(err.contains("nodes 1 and 2"), "{err}");

    // A half-connected relationship (start matches, end does not) is just
    // as broken; the old code silently emitted a forward hop here.
    let path = test_path(vec![test_node(1), test_node(2)], vec![test_rel(10, 1, 9)]);
    let err = Pack::from_mdb(&path, ValueCodec::plain_44()).unwrap_err();
    assert!(matches!(err, MdbPackError::PathInvariant(_)), "{err}");
    assert!(err.to_string().contains("path invariant broken"), "{err}");

    // Fewer nodes than rels + 1 cannot form the hops at all.
    let path = test_path(vec![test_node(1)], vec![test_rel(10, 1, 2)]);
    let err = Pack::from_mdb(&path, ValueCodec::plain_44()).unwrap_err();
    assert!(matches!(err, MdbPackError::PathInvariant(_)), "{err}");
    let err = err.to_string();
    assert!(err.contains("path invariant broken"), "{err}");
    assert!(err.contains("1 nodes for 1 relationships"), "{err}");
}

#[test]
fn path_zero_length_has_empty_sequence() {
    let path = test_path(vec![test_node(7)], vec![]);
    let (nodes, rels, seq) = path_fields(&path);
    assert_eq!(nodes.len(), 1);
    assert!(rels.is_empty());
    assert!(seq.is_empty());
}

#[test]
fn path_nested_in_list_in_map_encodes_identically_and_decodes() {
    let path = test_path(vec![test_node(1), test_node(2)], vec![test_rel(10, 1, 2)]);
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "p".to_string(),
        relate::Value::List(vec![path.clone()].into()),
    );
    let value = relate::Value::Map(map.into());
    let pack = Pack::from_mdb(&value, ValueCodec::plain_44()).unwrap();
    let Pack::Map(entries) = &pack else {
        panic!("root must be a map");
    };
    let Pack::List(list) = &entries[0].1 else {
        panic!("map value must be a list");
    };
    let Pack::Struct { tag, .. } = &list[0] else {
        panic!("nested path must still be a struct");
    };
    assert_eq!(*tag, 0x50);
    assert_eq!(decode(&encode(&pack).unwrap()).unwrap(), pack);

    let (_, rels, seq) = path_fields(&path);
    assert_eq!(rels.len(), 1);
    assert_eq!(seq, vec![Pack::Int(1), Pack::Int(1)]);
}

#[test]
fn path_nesting_respects_the_shared_depth_limit() {
    // A Path is itself a container and its three field lists are real
    // containers on the wire too, so a Path needs two free levels below
    // it: 126 enclosing lists put it exactly at the accepted depth (128
    // containers counted from the root); the 127th enclosing list is
    // rejected with the shared nesting-limit error.
    let value = wrapped_in_lists(test_path(vec![], vec![]), MAX_DEPTH - 2);
    assert!(Pack::from_mdb(&value, ValueCodec::plain_44()).is_ok());
    let over = relate::Value::List(vec![value].into());
    assert_eq!(
        Pack::from_mdb(&over, ValueCodec::plain_44()),
        Err(MdbPackError::DepthExceeded)
    );
}

/// ADR-0484 decision 6: a 5.x graph structure names its element through
/// the engine helpers, which return nothing for the `system` namespace —
/// so a 5.4 encode of its elements fails with the engine `elementId()`
/// precedent's typed error, never a fabricated id, while the 4.4 forms
/// (which carry no element ids at all) encode the same value fine.
#[test]
fn values_54_elements_without_element_ids_fail_typed() {
    let node = relate::Value::Node(test_node(0).into());
    let err = Pack::from_mdb(&node, ValueCodec::at_5_4(relate::Namespace::System)).unwrap_err();
    assert_eq!(err, MdbPackError::ElementIdUnavailable);
    assert_eq!(
        err.to_string(),
        "elements of this namespace have no element ids"
    );
    let rel = relate::Value::Relationship(test_rel(10, 0, 1).into());
    assert_eq!(
        Pack::from_mdb(&rel, ValueCodec::at_5_4(relate::Namespace::System)).unwrap_err(),
        MdbPackError::ElementIdUnavailable
    );
    // The 4.4 structures never name elements, so the namespace axis is
    // unread and the same values encode.
    assert!(Pack::from_mdb(&node, ValueCodec::plain_44()).is_ok());
    assert!(Pack::from_mdb(&rel, ValueCodec::plain_44()).is_ok());
}
