//! CIP-275 behind the `graph-type-show-variants` preview gate (ADR-0371),
//! enabled side, beyond the CIP's own examples (`graph_type_show_variants.rs`):
//! independent constraints made by a plain `CREATE CONSTRAINT` (the
//! store-only ones, as opposed to the graph type body's own), states a
//! variant leaves alone, transitive inheritance, the statement shape
//! (`YIELD` / `WHERE` / `AS GRAPH`) and the display-only guarantee against
//! the store: nothing is created, nothing is persisted.
//!
//! Specifications are compared without backticks, generated constraint
//! names or line breaks, the shape the CIP prints.

mod common;

use relate::Value;

use common::show_variants::show_spec;
use common::{rows, run, scalar_one};

/// The engine's own, unprinted specification: byte-identity checks.
fn raw_spec(c: &mut relate::Connection, variant: &str) -> Value {
    scalar_one(c, &format!("SHOW CURRENT GRAPH TYPE {variant}"))
}

fn constraint_names(c: &mut relate::Connection) -> Vec<Value> {
    rows(c, "SHOW CONSTRAINTS YIELD name RETURN name ORDER BY name")
        .into_iter()
        .map(|row| row.into_iter().next().expect("one column"))
        .collect()
}

#[test]
fn store_only_independent_constraints_consolidate_too() {
    let mut c = common::show_variants::show_variants_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => :B {p1 :: STRING}), \
         (:C => :A&D {p2 :: FLOAT NOT NULL}) }",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT d_p_int FOR (n:D) REQUIRE n.p IS :: INTEGER",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT e_p FOR (n:E) REQUIRE n.p IS NOT NULL",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT s_str FOR ()-[r:S]-() REQUIRE r.p IS :: STRING",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT b_unique FOR (n:B) REQUIRE n.p3 IS UNIQUE",
    );
    let names_before = constraint_names(&mut c);
    let plain = show_spec(&mut c, "");
    assert_eq!(
        plain,
        "{  (:A => :B {p1 :: STRING}),  (:C => :A&D {p2 :: FLOAT NOT NULL}),  \
         CONSTRAINT b_unique FOR (n:B) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT d_p_int FOR (n:D) REQUIRE (n.p) IS :: INTEGER,  \
         CONSTRAINT e_p FOR (n:E) REQUIRE (n.p) IS NOT NULL,  \
         CONSTRAINT s_str FOR ()-[r:S]->() REQUIRE (r.p) IS :: STRING}"
    );

    // The independent ones become element types; the unique constraint is
    // undesignated and stays, with its (still non-identifying) label B.
    assert_eq!(
        show_spec(&mut c, "AS CONSOLIDATED"),
        "{  (:A => :B {p1 :: STRING}),  (:C => :A&D {p2 :: FLOAT NOT NULL}),  \
         (:D => {p :: INTEGER}),  (:E => {p :: ANY NOT NULL}),  \
         ()-[:S => {p :: STRING}]->(),  \
         CONSTRAINT b_unique FOR (n:B) REQUIRE (n.p3) IS UNIQUE}"
    );
    // D's independent type constraint is inherited by C as if dependent,
    // and B's unique constraint by A and C.
    assert_eq!(
        show_spec(&mut c, "AS FLATTENED"),
        "{  (:A => :B {p1 :: STRING}),  \
         (:C => :A&B&D {p1 :: STRING, p2 :: FLOAT NOT NULL, p :: INTEGER}),  \
         CONSTRAINT FOR (n:A =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT FOR (n:C =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT b_unique FOR (n:B) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT d_p_int FOR (n:D) REQUIRE (n.p) IS :: INTEGER,  \
         CONSTRAINT e_p FOR (n:E) REQUIRE (n.p) IS NOT NULL,  \
         CONSTRAINT s_str FOR ()-[r:S]->() REQUIRE (r.p) IS :: STRING}"
    );

    // Display only: no constraint was created or dropped, and plain SHOW
    // is what it was.
    assert_eq!(constraint_names(&mut c), names_before);
    assert_eq!(show_spec(&mut c, ""), plain);
}

#[test]
fn rules_of_one_property_merge_across_the_body_and_the_store() {
    let mut c = common::show_variants::show_variants_connection();
    // The body's type constraint and a store-only existence constraint on
    // the same property of the same label are one property definition.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => {p1 :: STRING}), \
         CONSTRAINT FOR (n:D) REQUIRE n.p IS :: INTEGER, \
         CONSTRAINT FOR (n:D) REQUIRE n.q IS :: STRING }",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT d_p_exists FOR (n:D) REQUIRE n.p IS NOT NULL",
    );
    assert_eq!(
        show_spec(&mut c, "AS CONSOLIDATED"),
        "{  (:A => {p1 :: STRING}),  (:D => {p :: INTEGER NOT NULL, q :: STRING})}"
    );
    // A union carries NOT NULL per member.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => {p1 :: STRING}), \
         CONSTRAINT FOR (n:D) REQUIRE n.p IS :: INTEGER | FLOAT }",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT d_p_exists FOR (n:D) REQUIRE n.p IS NOT NULL",
    );
    assert_eq!(
        show_spec(&mut c, "AS CONSOLIDATED"),
        "{  (:A => {p1 :: STRING}),  (:D => {p :: INTEGER NOT NULL | FLOAT NOT NULL})}"
    );
}

#[test]
fn states_a_variant_has_nothing_to_do_with_are_returned_as_they_are() {
    let mut c = common::show_variants::show_variants_connection();
    // The empty graph type.
    for variant in ["AS CONSOLIDATED", "AS FLATTENED"] {
        assert_eq!(raw_spec(&mut c, variant), raw_spec(&mut c, ""));
    }
    // Element types and undesignated constraints only, no inheritance, no
    // independent constraints: both variants are the plain type, byte for
    // byte, as strings and as graphs.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => {b :: STRING, a :: INTEGER NOT NULL}), \
         (:A)-[:R => {w :: FLOAT}]->(:B =>), (:B => {k :: STRING}), \
         CONSTRAINT a_key FOR (n:A) REQUIRE n.a IS KEY, \
         CONSTRAINT FOR (n:Plain) REQUIRE n.u IS UNIQUE }",
    );
    for variant in ["AS CONSOLIDATED", "AS FLATTENED"] {
        assert_eq!(raw_spec(&mut c, variant), raw_spec(&mut c, ""), "{variant}");
        assert_eq!(
            rows(
                &mut c,
                &format!("SHOW CURRENT GRAPH TYPE {variant} AS GRAPH")
            )
            .len(),
            1
        );
    }
}

#[test]
fn flattening_follows_inheritance_chains() {
    let mut c = common::show_variants::show_variants_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:N1 => {a :: STRING}), \
         (:N2 => :N1 {b :: STRING}), (:N3 => :N2 {c :: STRING}), \
         (:N3)-[:R =>]->(:N1) }",
    );
    // Each layer shows every ancestor's rules, not only its parent's;
    // relationship endpoints stay labels.
    assert_eq!(
        show_spec(&mut c, "AS FLATTENED"),
        "{  (:N1 => {a :: STRING}),  (:N2 => :N1 {a :: STRING, b :: STRING}),  \
         (:N3 => :N1&N2 {a :: STRING, b :: STRING, c :: STRING}),  \
         (:N3 =>)-[:R =>]->(:N1 =>)}"
    );
    // The stored type is unchanged: an inherited type still drops as the
    // declared one.
    assert_eq!(
        show_spec(&mut c, ""),
        "{  (:N1 => {a :: STRING}),  (:N2 => :N1 {b :: STRING}),  \
         (:N3 => :N2 {c :: STRING}),  (:N3 =>)-[:R =>]->(:N1 =>)}"
    );
}

#[test]
fn variants_take_yield_where_and_as_graph() {
    let mut c = common::show_variants::show_variants_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => {p1 :: STRING}), \
         CONSTRAINT FOR (n:D) REQUIRE n.p IS :: INTEGER }",
    );
    // Columns are the plain ones, and YIELD / WHERE work on them.
    let hit = rows(
        &mut c,
        "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED YIELD specification \
         WHERE specification CONTAINS '(:`D` => {`p` :: INTEGER})' RETURN specification",
    );
    assert_eq!(hit.len(), 1);
    let miss = rows(
        &mut c,
        "SHOW CURRENT GRAPH TYPE AS FLATTENED YIELD specification \
         WHERE specification CONTAINS '(:`D` => {`p` :: INTEGER})' RETURN specification",
    );
    assert!(miss.is_empty());
    let graph = rows(
        &mut c,
        "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED AS GRAPH SINGLE \
         YIELD nodes WHERE size(nodes) = 2 RETURN size(nodes)",
    );
    assert_eq!(graph, vec![vec![Value::Int(2)]]);
}

#[test]
fn the_variants_persist_nothing() {
    // Opt in before any store opens (the shared fixture does it once).
    drop(common::show_variants::show_variants_connection());
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().join("variants").to_str().unwrap().to_string();
    let (plain, names) = {
        let db = relate::Database::open(&uri).unwrap();
        let mut c = db.connect().unwrap();
        run(
            &mut c,
            "ALTER CURRENT GRAPH TYPE SET { (:A => :B {p1 :: STRING}), \
             (:C => :A&D {p2 :: FLOAT NOT NULL}), \
             CONSTRAINT d_p_int FOR (n:D) REQUIRE n.p IS :: INTEGER }",
        );
        let plain = raw_spec(&mut c, "");
        raw_spec(&mut c, "AS CONSOLIDATED");
        raw_spec(&mut c, "AS FLATTENED");
        assert_eq!(raw_spec(&mut c, ""), plain);
        (plain, constraint_names(&mut c))
    };
    let db = relate::Database::open(&uri).unwrap();
    let mut c = db.connect().unwrap();
    assert_eq!(raw_spec(&mut c, ""), plain);
    assert_eq!(constraint_names(&mut c), names);
    // And the reopened store shows the same variants as before.
    assert!(show_spec(&mut c, "AS CONSOLIDATED").contains("(:D => {p :: INTEGER})"));
}
