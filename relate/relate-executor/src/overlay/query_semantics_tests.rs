//! End-to-end query semantics over the overlay: statements run through
//! the write harness whose behavior is not owned by a narrower overlay
//! concern module (write stats, seeks, batch commit, materialize, and
//! the rest each live in their own module).

use super::test_support::{run_write, run_write_err};
use crate::exec::test_support::{exec_write, write_scratch};
use crate::graph::Graph;
use relate_core::Value;

#[test]
fn overlay_does_not_mutate_base_until_redo() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    let result = exec_write(
        &g,
        &scratch,
        "CREATE (n:Person {name: 'Ada'}) WITH n MATCH (m:Person {name: 'Ada'}) RETURN m.name AS name",
    )
    .unwrap();
    assert_eq!(result.rows[0][0].as_string(), Some("Ada"));
    assert_eq!(g.live_node_count(), 0);
    assert_eq!(scratch.into_inner().take_redo().len(), 1);
}

#[test]
fn exists_subquery_outgoing() {
    let result = run_write(
        "CREATE (a:A {prop: 1})-[:R]->(b:B {prop: 1}) WITH * MATCH (n) WHERE exists { (n)-->() } RETURN n.prop AS p",
    );
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

#[test]
fn chained_comparison() {
    let result = run_write("CREATE ({num: 2}) WITH * MATCH (n) WHERE 1 < n.num < 3 RETURN n.num");
    assert_eq!(result.rows, vec![vec![Value::Int(2)]]);
}

#[test]
fn collect_inside_map() {
    let result = run_write("CREATE (:A) WITH * MATCH (n) RETURN {k: collect(n)} AS m");
    assert_eq!(result.rows.len(), 1);
    assert!(matches!(result.rows[0][0], Value::Map(_)));
}

#[test]
fn deleted_entity_property_access_errors() {
    let err = run_write_err("CREATE ({num: 0}) WITH * MATCH (n) DELETE n RETURN n.num");
    assert!(
        err.contains("DeletedEntityAccess"),
        "expected DeletedEntityAccess, got {err}"
    );
}

#[test]
fn deleted_entity_subscript_refuses_like_property_access() {
    // A runtime-string subscript rides the same `prop` entry as `n.num`, so
    // a deleted-but-bound node answers the same authored refusal instead of
    // a silent `null`.
    let err = run_write_err("CREATE ({num: 0}) WITH * MATCH (n) DELETE n RETURN n['num']");
    assert!(
        err.contains("DeletedEntityAccess"),
        "expected DeletedEntityAccess, got {err}"
    );
}

#[test]
fn keys_over_deleted_node_refuses_like_property_access() {
    // keys() rides the one store-backed entity_properties read, so a
    // deleted-but-bound node answers the same authored refusal as
    // properties()/n.num — not an internal no-store-entry error.
    let err = run_write_err("CREATE ({num: 0}) WITH * MATCH (n) DELETE n RETURN keys(n)");
    assert!(
        err.contains("DeletedEntityAccess"),
        "expected DeletedEntityAccess, got {err}"
    );
}

#[test]
fn properties_over_deleted_node_refuses() {
    let err = run_write_err("CREATE ({num: 0}) WITH * MATCH (n) DELETE n RETURN properties(n)");
    assert!(
        err.contains("DeletedEntityAccess"),
        "expected DeletedEntityAccess, got {err}"
    );
}

#[test]
fn keys_over_deleted_relationship_refuses_like_property_access() {
    let err = run_write_err(
        "CREATE (a)-[r:R {num: 0}]->(b) WITH * MATCH ()-[r]->() DELETE r RETURN keys(r)",
    );
    assert!(
        err.contains("DeletedEntityAccess"),
        "expected DeletedEntityAccess, got {err}"
    );
}

#[test]
fn keys_over_live_entities_list_the_backing_maps_keys() {
    let r = run_write(
        "CREATE (n:N {b: 2, a: 1})-[e:R {y: 2, x: 1}]->(m) \
         WITH * MATCH (n)-[e]->(m) RETURN keys(n) AS nk, keys(e) AS ek",
    );
    assert_eq!(r.rows.len(), 1);
    let keys = |v: &Value| -> Vec<String> {
        let Value::List(items) = v else {
            panic!("keys() did not return a list: {v:?}")
        };
        items
            .iter()
            .map(|k| k.as_string().expect("key is a string").to_string())
            .collect()
    };
    // Map order (sorted), the keys of the very map properties() returns.
    assert_eq!(keys(&r.rows[0][0]), vec!["a", "b"]);
    assert_eq!(keys(&r.rows[0][1]), vec!["x", "y"]);
}

#[test]
fn map_property_is_invalid() {
    let err = run_write_err("CREATE (a) SET a.maplist = [{num: 1}]");
    assert!(
        err.contains("InvalidPropertyType"),
        "expected InvalidPropertyType, got {err}"
    );
}

#[test]
fn undirected_var_length_two_does_not_overcount() {
    let result = run_write(
        "CREATE (a:A)-[:REL1]->(b:B), (b)-[:REL2]->(a), (a)-[:REL3]->(:C), (a)-[:REL1]->(:D) \
         WITH * MATCH (n) WHERE (n)-[:REL1*2]-() RETURN n",
    );
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn merge_binds_named_path() {
    let r = run_write("MERGE p = (a {num: 1}) RETURN p");
    assert!(matches!(r.rows[0][0], Value::Path(_)));
}

#[test]
fn with_star_forwards_create_vars() {
    let r = run_write("CREATE (a {var: 1}), (b {var: 2}) WITH * RETURN a.var, b.var");
    assert_eq!(r.rows[0][0].as_int(), Some(1));
    assert_eq!(r.rows[0][1].as_int(), Some(2));
}

#[test]
fn group_by_distinguishes_unlabeled_nodes() {
    let r = run_write(
        "CREATE (a:L), (b1), (b2) CREATE (a)-[:A]->(b1), (a)-[:A]->(b2) \
         WITH * MATCH p=(x:L)-[*]->(b) RETURN b, avg(length(p))",
    );
    assert_eq!(r.rows.len(), 2);
}

#[test]
fn bound_rel_list_is_exact_trail() {
    let r = run_write(
        "CREATE (a:A)-[:Y]->(b:B)-[:Y]->(c:C) \
         WITH * MATCH ()-[r1]->()-[r2]->() WITH [r1, r2] AS rs LIMIT 1 \
         MATCH (first)-[rs*]->(second) RETURN first, second",
    );
    assert_eq!(r.rows.len(), 1);
}
