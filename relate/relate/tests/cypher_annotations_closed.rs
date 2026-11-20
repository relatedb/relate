//! The `cypher-annotations` preview gate, CLOSED side (ADR-0371, CIP-281):
//! without the opt-in `@` is the parser's own unexpected-character syntax
//! error -- the gate sits at the lexer, so the disabled form is not the
//! authored preview error.
//!
//! This file is its own test binary: `RELATE_PREVIEW` is process-global and
//! read once per `Database::open`, so it must never share a process with the
//! enabled side (`cypher_annotations_gate.rs`).

mod common;

use common::connection;

#[test]
fn an_annotation_needs_the_opt_in() {
    let mut c = connection();
    for query in [
        "@profile() RETURN 1",
        "@a(name: \"x\", type: 2) MATCH (n) RETURN n",
    ] {
        let error = c.query(query).expect_err(query).to_string();
        assert!(
            error.contains("unexpected character: @"),
            "{query}: {error}"
        );
    }
}

#[test]
fn an_unannotated_query_is_unchanged() {
    let mut c = connection();
    c.query("RETURN 1 AS x").unwrap();
}
