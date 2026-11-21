//! `PATH [ ... ]` construction and `p1 || p2` path concatenation through
//! `relate::Database`, under every `RELATE_PREVIEW` combination of the
//! `path-construction` and `path-concatenation` gates (ADR-0371, ADR-0637).
//!
//! `RELATE_PREVIEW` is read once per `Database::open`, so each leg opens its
//! own store under a lock that keeps the `set_var` + open pair atomic against
//! sibling tests.

mod common;

use relate::{query_result_to_wire, Value};
use relate_core::{preview_feature_disabled_text, PreviewFeature};
use relate_test_support::EnvRestore;
use serde_json::json;

use common::{err_text, run};

const SEED: &str = "CREATE (a:A {n: 1})-[:R {w: 5}]->(b:B {n: 2})-[:S]->(c:C {n: 3}), (:D {n: 4})";
const CONSTRUCT: &str = "MATCH (a:A)-[r]->(b:B) RETURN PATH [a, r, b] AS p";
const CONCAT: &str = "MATCH p1 = (:A)-[]->(:B), p2 = (:B)-[]->(:C) RETURN p1 || p2 AS p";

/// A fresh seeded store opened with `RELATE_PREVIEW` set to `setting`
/// (`None` = unset).
fn open_with(setting: Option<&str>) -> relate::Connection {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore = EnvRestore::new("RELATE_PREVIEW");
    match setting {
        Some(value) => std::env::set_var("RELATE_PREVIEW", value),
        None => std::env::remove_var("RELATE_PREVIEW"),
    }
    let mut conn = relate::Database::open(":memory:")
        .unwrap()
        .connect()
        .unwrap();
    run(&mut conn, SEED);
    conn
}

fn both() -> relate::Connection {
    open_with(Some("path-construction,path-concatenation"))
}

fn closed_construction(conn: &mut relate::Connection) {
    let text = err_text(conn.query(CONSTRUCT));
    let expected = preview_feature_disabled_text(PreviewFeature::PathConstruction);
    assert!(text.contains(&expected), "{text}");
}

/// `||` on paths keeps the pre-existing operator error while its gate is
/// off (the gate admits path operands; nothing authored replaces the error).
fn closed_concatenation(conn: &mut relate::Connection) {
    let text = err_text(conn.query(CONCAT));
    assert!(
        text.contains("|| requires two strings or two lists"),
        "{text}"
    );
}

fn path_value(conn: &mut relate::Connection, query: &str) -> Value {
    run(conn, query).rows[0][0].clone()
}

fn path_ids(p: &Value) -> (Vec<i64>, Vec<i64>) {
    let Value::Path(path) = p else {
        panic!("expected a path value")
    };
    (
        path.nodes.iter().map(|n| n.id).collect(),
        path.rels.iter().map(|r| r.id).collect(),
    )
}

#[test]
fn unset_fails_closed_for_both_forms() {
    let mut c = open_with(None);
    closed_construction(&mut c);
    closed_concatenation(&mut c);
}

#[test]
fn each_flag_opens_only_its_own_form() {
    let mut c = open_with(Some("path-construction"));
    path_value(&mut c, CONSTRUCT);
    closed_concatenation(&mut c);

    let mut c = open_with(Some("path-concatenation"));
    path_value(&mut c, CONCAT);
    closed_construction(&mut c);
}

#[test]
fn both_flags_open_both_forms() {
    let mut c = both();
    let built = path_value(&mut c, CONSTRUCT);
    assert_eq!(path_ids(&built).0.len(), 2);
    let joined = path_value(&mut c, CONCAT);
    let (nodes, rels) = path_ids(&joined);
    assert_eq!((nodes.len(), rels.len()), (3, 2));
}

#[test]
fn string_and_list_concatenation_are_never_gated() {
    let mut c = open_with(None);
    let r = run(&mut c, "RETURN 'a' || 'b' AS s, [1] || [2] AS l");
    assert_eq!(r.rows[0][0], Value::String("ab".into()));
}

#[test]
fn a_constructed_path_equals_the_matched_path() {
    let mut c = both();
    let built = path_value(&mut c, CONSTRUCT);
    let matched = path_value(&mut c, "MATCH p = (:A)-[]->(:B) RETURN p");
    assert_eq!(built, matched);
}

#[test]
fn relationship_direction_is_not_checked() {
    let mut c = both();
    // The relationship is stored A -> B; listing B first still forms a path.
    let reversed = path_value(&mut c, "MATCH (a:A)-[r]->(b:B) RETURN PATH [b, r, a] AS p");
    assert_eq!(path_ids(&reversed).1.len(), 1);
}

#[test]
fn invalid_lists_and_boundaries_are_errors() {
    let mut c = both();
    for query in [
        // not alternating node, relationship, node
        "MATCH (a:A)-[r]->(b:B) RETURN PATH [a, b] AS p",
        "MATCH (a:A)-[r]->(b:B) RETURN PATH [r, a] AS p",
        "MATCH (a:A)-[r]->(b:B) RETURN PATH [a, r] AS p",
        // non-element values
        "RETURN PATH [1, 2, 3] AS p",
        // relationship not connecting its neighbours
        "MATCH (a:A)-[r]->(:B), (d:D) RETURN PATH [a, r, d] AS p",
        // boundary mismatch: p1 ends at B, p2 starts at C
        "MATCH p1 = (:A)-[]->(:B), p2 = (:B)-[]->(:C) RETURN p2 || p1 AS p",
        // non-path operand
        "MATCH p = (:A)-[]->(:B) RETURN p || 1 AS x",
    ] {
        let text = err_text(c.query(query));
        assert!(!text.is_empty(), "{query}");
        assert!(!text.contains("preview feature"), "{query}: {text}");
    }
}

#[test]
fn null_elements_and_operands_yield_null() {
    let mut c = both();
    for query in [
        "MATCH (a:A)-[r]->(b:B) RETURN PATH [a, null, b] AS p",
        "MATCH (a:A)-[r]->(b:B) RETURN PATH [null, r, b] AS p",
        "MATCH p = (:A)-[]->(:B) RETURN p || null AS p",
        "MATCH p = (:A)-[]->(:B) RETURN null || p AS p",
    ] {
        assert_eq!(path_value(&mut c, query), Value::Null, "{query}");
    }
}

#[test]
fn the_wire_encoding_of_a_built_path_matches_a_matched_one() {
    let mut c = both();
    let built = query_result_to_wire(&run(&mut c, CONSTRUCT)).unwrap();
    let matched =
        query_result_to_wire(&run(&mut c, "MATCH p = (:A)-[]->(:B) RETURN p AS p")).unwrap();
    assert_eq!(built["rows"], matched["rows"]);
    let cell = &built["rows"][0]["p"];
    let items = cell.as_array().expect("a path encodes as an element array");
    let kinds: Vec<_> = items.iter().map(|e| e["kind"].clone()).collect();
    assert_eq!(kinds, [json!("node"), json!("relationship"), json!("node")]);
    assert_eq!(items[1]["type"], json!("R"));
    assert_eq!(items[1]["properties"], json!({"w": 5}));
}

#[test]
fn a_concatenated_path_shares_the_boundary_node_once() {
    let mut c = both();
    let wire = query_result_to_wire(&run(&mut c, CONCAT)).unwrap();
    let kinds: Vec<_> = wire["rows"][0]["p"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        ["node", "relationship", "node", "relationship", "node"]
    );
}
