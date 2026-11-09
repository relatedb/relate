//! Result value structures on the wire (ADR-0272, ADR-0325, ADR-0484
//! decisions 6–7): what a PULL's RECORDs carry for graph and temporal
//! values at each spoken version. The 4.4 forms are byte-pinned by the
//! compat corpus (`baseline/value-*.bolt`, `values/temporal-structs.bolt`);
//! the tests here pin the 5.4 forms against them — the element-id fields
//! of Node/Relationship/UnboundRelationship and the fixed DateTime
//! structures — plus the 4.4 contrast that keeps the gate honest.

use std::net::TcpStream;

use super::*;
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    connected_client, connected_client_at, drain_all, read_msg, send_run, success_map,
};

/// RUN `cypher` and drain it: the one row exchange spelling these value
/// tests share.
fn run_one_row(client: &mut TcpStream, cypher: &str) -> Vec<Pack> {
    send_run(client, cypher);
    success_map(read_msg(client));
    let (mut records, _) = drain_all(client, Message::Pull);
    assert_eq!(records.len(), 1, "{cypher} returns one row");
    records.remove(0)
}

/// ADR-0484 decision 6: at 5.4 the graph structures carry the element-id
/// fields — Node `0x4E` `[id, labels, properties, element_id]`,
/// Relationship `0x52` `[id, start, end, rel_type, properties, element_id,
/// start_node_element_id, end_node_element_id]` — and the strings are the
/// engine `node_element_id`/`rel_element_id` helpers' own output: the one
/// source `elementId()` reads, pinned by returning both the structures and
/// the function's strings in one row.
#[test]
fn records_at_5_4_carry_the_element_id_structures() {
    let mut client = connected_client_at(BoltVersion::V5_4);
    let row = run_one_row(
        &mut client,
        "CREATE (a:P {k: 1})-[r:R {w: 2}]->(b:P {k: 3}) RETURN a, r, b, elementId(a), elementId(r), elementId(b)",
    );
    assert_eq!(
        row[0],
        Pack::Struct {
            tag: 0x4E,
            fields: vec![
                Pack::Int(0),
                Pack::List(vec![Pack::String("P".into())]),
                Pack::Map(vec![(Pack::String("k".into()), Pack::Int(1))]),
                Pack::String("4:relate-neo4j:n0".into()),
            ],
        },
        "Node 0x4E is the 4-field structure at 5.4"
    );
    assert_eq!(
        row[1],
        Pack::Struct {
            tag: 0x52,
            fields: vec![
                Pack::Int(0),
                Pack::Int(0),
                Pack::Int(1),
                Pack::String("R".into()),
                Pack::Map(vec![(Pack::String("w".into()), Pack::Int(2))]),
                Pack::String("5:relate-neo4j:r0".into()),
                Pack::String("4:relate-neo4j:n0".into()),
                Pack::String("4:relate-neo4j:n1".into()),
            ],
        },
        "Relationship 0x52 is the 8-field structure at 5.4"
    );
    assert_eq!(
        row[2],
        Pack::Struct {
            tag: 0x4E,
            fields: vec![
                Pack::Int(1),
                Pack::List(vec![Pack::String("P".into())]),
                Pack::Map(vec![(Pack::String("k".into()), Pack::Int(3))]),
                Pack::String("4:relate-neo4j:n1".into()),
            ],
        }
    );
    // The struct's element_id field is exactly `elementId()`'s string.
    assert_eq!(row[3], Pack::String("4:relate-neo4j:n0".into()));
    assert_eq!(row[4], Pack::String("5:relate-neo4j:r0".into()));
    assert_eq!(row[5], Pack::String("4:relate-neo4j:n1".into()));
    let Pack::Struct { fields, .. } = &row[0] else {
        panic!("a node is a structure");
    };
    assert_eq!(fields[3], row[3], "the element_id field is elementId()'s");
}

/// ADR-0484 decision 6: a Path's relationships ride the
/// UnboundRelationship `0x72` form, which at 5.4 is the 4-field
/// `[id, rel_type, properties, element_id]` structure, and its nodes the
/// 4-field `0x4E` form.
#[test]
fn records_at_5_4_carry_the_element_id_path_structures() {
    let mut client = connected_client_at(BoltVersion::V5_4);
    run_one_row(
        &mut client,
        "CREATE (a:P {k: 1})-[r:R {w: 2}]->(b:P {k: 3}) RETURN a",
    );
    let row = run_one_row(&mut client, "MATCH p = (x:P)-[r:R]->(y:P) RETURN p");
    let Pack::Struct { tag, fields } = &row[0] else {
        panic!("a path is a structure");
    };
    assert_eq!(*tag, 0x50, "Bolt Path tag");
    let Pack::List(nodes) = &fields[0] else {
        panic!("the path's nodes are a list");
    };
    assert_eq!(nodes.len(), 2);
    assert_eq!(
        nodes[0],
        Pack::Struct {
            tag: 0x4E,
            fields: vec![
                Pack::Int(0),
                Pack::List(vec![Pack::String("P".into())]),
                Pack::Map(vec![(Pack::String("k".into()), Pack::Int(1))]),
                Pack::String("4:relate-neo4j:n0".into()),
            ],
        }
    );
    let Pack::List(rels) = &fields[1] else {
        panic!("the path's relationships are a list");
    };
    assert_eq!(
        rels[0],
        Pack::Struct {
            tag: 0x72,
            fields: vec![
                Pack::Int(0),
                Pack::String("R".into()),
                Pack::Map(vec![(Pack::String("w".into()), Pack::Int(2))]),
                Pack::String("5:relate-neo4j:r0".into()),
            ],
        },
        "UnboundRelationship 0x72 is the 4-field structure at 5.4"
    );
}

/// ADR-0484 decision 7: at 5.4 an offset datetime is DateTime `0x49`
/// `[UTC epoch seconds, nanoseconds, tz_offset_seconds]` — unshifted
/// seconds beside the original offset — and a zone datetime
/// DateTimeZoneId `0x69`; the legacy `0x46`/`0x66` are never written.
#[test]
fn records_at_5_4_carry_the_fixed_datetime_structures() {
    let mut client = connected_client_at(BoltVersion::V5_4);
    let row = run_one_row(
        &mut client,
        "RETURN datetime('2024-01-02T03:04:05.123456789+01:00') AS dt, datetime('2024-01-02T03:04:05[Europe/Stockholm]') AS zdt",
    );
    assert_eq!(
        row[0],
        Pack::Struct {
            tag: 0x49,
            fields: vec![Pack::Int(1704161045), Pack::Int(123456789), Pack::Int(3600)],
        },
        "the offset form is 0x49 with unshifted UTC seconds and the original offset"
    );
    assert_eq!(
        row[1],
        Pack::Struct {
            tag: 0x69,
            fields: vec![
                Pack::Int(1704161045),
                Pack::Int(0),
                Pack::String("Europe/Stockholm".into()),
            ],
        },
        "the zone form is 0x69"
    );
}

/// The 4.4 contrast (ADR-0484: a 4.4 connection is byte-identical to
/// today): the same values keep the 3/5-field graph structures with no
/// element ids and the legacy DateTime `0x46` with its shifted local
/// seconds — the forms the compat corpus pins byte-for-byte.
#[test]
fn records_at_4_4_keep_the_legacy_value_structures() {
    let mut client = connected_client();
    let row = run_one_row(
        &mut client,
        "CREATE (a:P {k: 1})-[r:R {w: 2}]->(b:P {k: 3}) RETURN a, r, datetime('2024-01-02T03:04:05.123456789+01:00')",
    );
    assert_eq!(
        row[0],
        Pack::Struct {
            tag: 0x4E,
            fields: vec![
                Pack::Int(0),
                Pack::List(vec![Pack::String("P".into())]),
                Pack::Map(vec![(Pack::String("k".into()), Pack::Int(1))]),
            ],
        },
        "Node 0x4E keeps its 3 fields at 4.4"
    );
    assert_eq!(
        row[1],
        Pack::Struct {
            tag: 0x52,
            fields: vec![
                Pack::Int(0),
                Pack::Int(0),
                Pack::Int(1),
                Pack::String("R".into()),
                Pack::Map(vec![(Pack::String("w".into()), Pack::Int(2))]),
            ],
        },
        "Relationship 0x52 keeps its 5 fields at 4.4"
    );
    assert_eq!(
        row[2],
        Pack::Struct {
            tag: 0x46,
            fields: vec![Pack::Int(1704164645), Pack::Int(123456789), Pack::Int(3600)],
        },
        "the offset datetime stays legacy 0x46 with its shifted local seconds"
    );
}
