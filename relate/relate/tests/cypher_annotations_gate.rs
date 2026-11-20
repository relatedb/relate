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
