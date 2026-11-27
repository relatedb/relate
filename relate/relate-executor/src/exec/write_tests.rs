//! The write path's runtime admission rules — the SET/REMOVE/DELETE
//! seam's executor-level pins. The dynamic-access rules reject a
//! runtime-resolved empty or NUL-containing property key and a
//! wrong-typed dynamic label; non-entity write targets fall into the
//! static and runtime error classes by the target's shape; DELETE
//! admits lists, paths and path sets through the one DeleteConnectedNode
//! policy, and a deleted-but-bound relationship tombstones like a node.

use relate_core::{Error, Value};

use crate::exec::test_support::{exec_write, exec_write_uncommitted, write_scratch, Builder};
use crate::graph::Graph;

/// LABS-709: the runtime half of the shared dynamic-access rule rejects
/// an empty or NUL-containing property key resolved from a *runtime*
/// value (the compile-time fold cannot see `n.k`).
#[test]
fn runtime_dynamic_property_key_rejects_empty_and_nul_names() {
    for bad_key in ["", "a\0b"] {
        let builder = Builder::new();
        builder.prop_node(
            "holder",
            &["Holder"],
            &[("k", Value::String(bad_key.into()))],
        );
        let graph = builder.done();
        let scratch = write_scratch(&graph);
        let error = match exec_write(
            &graph,
            &scratch,
            "MATCH (n:Holder) WITH n, n.k AS prop_key SET n[prop_key] = 1",
        ) {
            Err(error) => error,
            Ok(_) => panic!("an empty/NUL property key must be rejected at runtime"),
        };
        assert!(
            error.to_string().contains(
                "invalid name. A property key name cannot be empty or contain any null-bytes"
            ),
            "unexpected error: {error}"
        );
    }
}

/// A legitimately runtime-computed key keeps working: only the checks'
/// *phase* moved for constants, never the dynamic feature itself.
#[test]
fn runtime_computed_property_key_still_writes_the_property() {
    let builder = Builder::new();
    builder.prop_node("holder", &["Holder"], &[("k", Value::String("x".into()))]);
    let graph = builder.done();
    let result = exec_write_uncommitted(
        &graph,
        "MATCH (n:Holder) WITH n, n.k AS prop_key SET n[prop_key] = 1 RETURN n.x AS v",
    );
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

/// A property list holding null is not storable (Neo4j 22N39), while a
/// plain null assignment is the removal policy's business.
#[test]
fn set_property_rejects_a_list_holding_null() {
    let builder = Builder::new();
    builder.anon(&["A"]);
    let graph = builder.done();
    let error = match exec_write(
        &graph,
        &write_scratch(&graph),
        "MATCH (n:A) SET n.p = [1, null]",
    ) {
        Err(error) => error,
        Ok(_) => panic!("a list holding null must not be stored as a property"),
    };
    assert!(error.to_string().contains("InvalidPropertyType"), "{error}");
}

/// A write target whose value is not an entity is rejected by the one
/// shared helper, classed by the target's *shape* (REL8-1813): a
/// general target expression is the pinned compile-time
/// SyntaxError-class type mismatch (`Set.feature` "An assignment
/// target expression that is an integer is a semantic error"), while a
/// variable-rooted target — including the label arms, which stay
/// static — carries the runtime type error. Asserted on the typed
/// variant, never on the rendered prefix.
#[test]
fn non_entity_write_targets_are_classed_by_the_target_shape() {
    let graph = Graph::default();
    let cases: [(&str, bool); 7] = [
        // A general target expression: the pinned static class.
        ("SET (CASE WHEN true THEN 42 END).name = 'neo4j'", true),
        // Variable-rooted property targets: the runtime class
        // (`n[x][y]` is CIP-131 [15]/[16]).
        ("UNWIND [42] AS x SET x.p = 1", false),
        ("UNWIND [42] AS x REMOVE x.p", false),
        (
            "UNWIND [{a: 1}] AS n UNWIND ['a'] AS x UNWIND ['b'] AS y SET n[x][y] = 1",
            false,
        ),
        // The label arms keep the static class: the fault is the
        // target's shape, not a runtime index value.
        ("UNWIND [42] AS x SET x:Label", true),
        ("UNWIND [42] AS x REMOVE x:Label", true),
        // A literal target is not variable-rooted either.
        ("SET (1).p = 2", true),
    ];
    for (query, is_static) in cases {
        let error = match exec_write(&graph, &write_scratch(&graph), query) {
            Err(error) => error,
            Ok(_) => panic!("{query}: a non-entity write target must be rejected"),
        };
        let expected = if is_static {
            "the static SyntaxError class"
        } else {
            "the runtime type-error class"
        };
        assert_eq!(
            matches!(error, Error::Syntax(_)),
            is_static,
            "{query}: expected {expected}, got: {error}"
        );
    }
}

/// The runtime label rule (STRING or LIST<STRING>) names the offending
/// input type, and list elements are checked individually.
#[test]
fn runtime_dynamic_labels_reject_wrong_types_and_bad_names() {
    let cases: [(Value, &str); 4] = [
        (Value::Int(1), "Expected to be STRING or LIST<STRING>."),
        (
            Value::List(vec![Value::String("A".into()), Value::Int(1)].into()),
            "Expected to be STRING or LIST<STRING>.",
        ),
        (
            Value::String("".into()),
            "invalid name. A label name cannot be empty or contain any null-bytes",
        ),
        (
            Value::String("a\0b".into()),
            "invalid name. A label name cannot be empty or contain any null-bytes",
        ),
    ];
    for (value, expected) in cases {
        let builder = Builder::new();
        builder.prop_node("holder", &["Holder"], &[("label", value)]);
        let graph = builder.done();
        let scratch = write_scratch(&graph);
        let error = match exec_write(
            &graph,
            &scratch,
            "MATCH (n:Holder) WITH n, n.label AS lbl SET n:$(lbl)",
        ) {
            Err(error) => error,
            Ok(_) => panic!("a bad runtime dynamic label must be rejected"),
        };
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error}"
        );
    }
}

/// DELETE of a list bound in the row reads nested lists and paths in
/// place: every entity is deleted once and the row keeps its list.
#[test]
fn delete_list_of_nodes_and_paths_keeps_the_row_list() {
    let builder = Builder::new();
    let a = builder.anon(&["A"]);
    let b = builder.anon(&["B"]);
    builder.rel(a, b, "R", &[]);
    builder.anon(&["C"]);
    let g = builder.done();
    let r = exec_write_uncommitted(
        &g,
        "MATCH p = (:A)-->(:B), (c:C) WITH [c] + [[p], null] AS xs \
         DELETE xs RETURN size(xs) AS n",
    );
    assert_eq!(r.rows, vec![vec![Value::Int(3)]]);
    assert_eq!(r.stats.nodes_deleted, 3);
    assert_eq!(r.stats.relationships_deleted, 1);
}

/// LABS-819: `DELETE p` admits its path nodes through the same
/// `DeleteConnectedNode` policy as `DELETE n` — a path node whose
/// relationships reach outside the clause's delete set fails the
/// statement with the *same* error instead of being silently skipped.
#[test]
fn delete_path_fails_like_delete_n_on_outside_relationships() {
    fn graph_a_r_b_outside_c() -> Graph {
        let builder = Builder::new();
        let a = builder.anon(&["A"]);
        let b = builder.anon(&["B"]);
        let c = builder.anon(&["C"]);
        builder.rel(a, b, "R", &[]);
        builder.rel(b, c, "OUTSIDE", &[]);
        builder.done()
    }
    let g = graph_a_r_b_outside_c();
    let path_error = match exec_write(&g, &write_scratch(&g), "MATCH p = (:A)-[:R]->(:B) DELETE p")
    {
        Err(error) => error,
        Ok(_) => panic!("DELETE p must fail on a node with outside relationships"),
    };
    // The single-node spelling of the same graph state fails with the
    // identical message: one admission policy, no path-specific skip.
    let g = graph_a_r_b_outside_c();
    let node_error = match exec_write(&g, &write_scratch(&g), "MATCH (b:B) DELETE b") {
        Err(error) => error,
        Ok(_) => panic!("DELETE b on the same graph state must fail"),
    };
    assert_eq!(path_error.to_string(), node_error.to_string());
    assert!(
        path_error.to_string().contains(
            "ConstraintVerificationFailed: DeleteConnectedNode: Cannot delete node with \
             relationships unless DETACH"
        ),
        "unexpected error: {path_error}"
    );
}

/// The Delete5::[7] graph: two `:User` nodes joined by a
/// two-relationship cycle, shared by the two delete-set tests (one
/// deletes through a collected list of paths, the other through the
/// multi-row path match).
fn two_user_cycle() -> Graph {
    let builder = Builder::new();
    let a = builder.anon(&["User"]);
    let b = builder.anon(&["User"]);
    builder.rel(a, b, "R", &[]);
    builder.rel(b, a, "R", &[]);
    builder.done()
}

/// The Delete5::[7] shape: two paths whose cross-relationships belong
/// to the *same clause's* delete set — the clause's relationships all
/// leave the graph before any node is admitted, so every node is
/// deleted (nothing silently skipped, nothing wrongly failed).
#[test]
fn delete_paths_covering_all_relationships_delete_every_node() {
    let r = exec_write_uncommitted(
        &two_user_cycle(),
        "MATCH p = (:User)-[r]->(:User) \
         WITH {key: collect(p)} AS pathColls \
         DELETE pathColls.key[0], pathColls.key[1]",
    );
    assert_eq!(r.stats.nodes_deleted, 2);
    assert_eq!(r.stats.relationships_deleted, 2);
}

/// The delete set spans the whole clause, not one row: a multi-row
/// path match whose rows cover each other's relationships deletes
/// everything — the row-space set model the bulk delete path already
/// uses for its own preflight.
#[test]
fn delete_path_set_spans_all_rows_of_the_clause() {
    let r = exec_write_uncommitted(
        &two_user_cycle(),
        "MATCH p = (:User)-[:R]->(:User) DELETE p",
    );
    assert_eq!(r.stats.nodes_deleted, 2);
    assert_eq!(r.stats.relationships_deleted, 2);
}

/// A deleted-but-bound relationship is tombstoned like a node: the
/// `r {.*}` map projection raises the same `DeletedEntityAccess` the
/// sibling property arm (`r.since`) raises, not a silent empty map —
/// one projection, one answer about one deleted value.
#[test]
fn deleted_relationship_all_properties_projection_raises_deleted_access() {
    let b = Builder::new();
    let a = b.anon(&["A"]);
    let b_node = b.anon(&["B"]);
    b.rel(a, b_node, "R", &[("since", Value::Int(2020))]);
    let g = b.done();
    let error = match exec_write(
        &g,
        &write_scratch(&g),
        "MATCH (:A)-[r:R {since: 2020}]->(:B) DELETE r RETURN r {.*} AS p",
    ) {
        Err(error) => error,
        Ok(_) => panic!("a deleted relationship projection must not project an empty map"),
    };
    assert!(error.to_string().contains("DeletedEntityAccess"), "{error}");
}
