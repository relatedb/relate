//! The `cypher-annotations` preview gate, ENABLED side (ADR-0371, CIP-281):
//! with `RELATE_PREVIEW=cypher-annotations` a clause-level `@name(params)`
//! annotation parses, is checked, and is discarded: the query answers exactly
//! as it does without it, and an invalid annotation is rejected.
//!
//! `RELATE_PREVIEW` is process-global and read once per `Database::open`, so
//! the enabled and closed sides are separate test binaries:
//! `cypher_annotations_closed.rs` must never share a process with this file.

mod common;

use relate::Value;

use common::{connection, set_preview_once};

#[test]
fn an_annotated_query_answers_as_the_plain_one_does() {
    set_preview_once("cypher-annotations");
    let mut c = connection();
    let plain = c.query("UNWIND [1, 2, 3] AS x RETURN x").unwrap();
    for query in [
        "@profile() UNWIND [1, 2, 3] AS x RETURN x",
        "@a(\"v\") @b(name: \"x\", type: 2) UNWIND [1, 2, 3] AS x @c(k: [1, 2]) RETURN x",
    ] {
        let annotated = c.query(query).unwrap();
        assert_eq!(annotated.rows, plain.rows, "{query}");
    }
    assert_eq!(plain.rows[0].as_slice(), [Value::Int(1)]);
}

#[test]
fn tx_metadata_is_discarded_while_the_channel_gate_is_off() {
    // ADR-0610's layering: with `cypher-annotations` on and
    // `value-provenance` off, the reserved name is discarded exactly like
    // any unrecognized annotation — its CIP-281 checks ran, the query
    // answers as its plain form does, and no record is retained (the
    // transaction-record machinery is the other gate's).
    set_preview_once("cypher-annotations");
    let db = relate::Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("@txMetadata({agent: 'a', n: 2}) CREATE (n:P {k: 1})")
        .unwrap();
    c.query("@txMetadata(agent: 'a') CREATE (m:P {k: 1})")
        .unwrap();
    // Two nodes, both spellings of the map parsed fine — and no record
    // retained: the transaction-record machinery is the other gate's.
    let count = c.query("MATCH (n) RETURN count(n) AS n").unwrap();
    assert_eq!(count.rows[0][0], Value::Int(2));
    assert!(db.transaction_records().unwrap().is_empty());
}

#[test]
fn an_invalid_annotation_is_rejected() {
    set_preview_once("cypher-annotations");
    let mut c = connection();
    for query in [
        "@a($p) RETURN 1",
        "@a(1, 2) RETURN 1",
        "@a(k: 1, k: 2) RETURN 1",
        "@a(k: {b: {c: 1}}) RETURN 1",
        "RETURN 1 @a()",
    ] {
        c.query(query)
            .expect_err(&format!("{query} must be rejected"));
    }
}
