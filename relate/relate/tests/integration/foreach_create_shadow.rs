//! A `FOREACH` body's `CREATE` declares fresh: a node that spells labels or
//! properties shadows an enclosing binding of its name even beside a
//! relationship, as the planner's binder already admits (Neo4j
//! `ForeachAcceptance` "Shadow outer variable in CREATE/MERGE"). A bare
//! node beside a relationship stays a reference to the existing binding.
//! The binder and the executor read one predicate,
//! `PathPattern::node_reuses_binding`.

use crate::common;

use common::connection;

/// The label sets of every node, sorted, as one string per node.
fn label_sets(c: &mut relate::Connection) -> Vec<String> {
    let result = c
        .query("MATCH (n) RETURN labels(n) AS l")
        .expect("labels query");
    let mut sets: Vec<String> = common::first_column(&result)
        .iter()
        .map(|v| format!("{v:?}"))
        .collect();
    sets.sort();
    sets
}

#[test]
fn a_labelled_node_beside_a_relationship_shadows_the_outer_binding() {
    let mut c = connection();
    c.query("CREATE (n:A) WITH n FOREACH (x IN [1] | CREATE (n:L)-[:R]->(m))")
        .expect("shadowing CREATE in a FOREACH body");
    // The outer `(:A)` is untouched; the body made fresh `(:L)` and `()`.
    let sets = label_sets(&mut c);
    assert_eq!(sets.len(), 3, "{sets:?}");
    assert!(
        sets.iter()
            .all(|s| !(s.contains("\"A\"") && s.contains("\"L\""))),
        "the outer node must not gain the shadowing node's label: {sets:?}"
    );
}

#[test]
fn a_bare_node_beside_a_relationship_references_the_outer_binding() {
    let mut c = connection();
    c.query("CREATE (n:A) WITH n FOREACH (x IN [1] | CREATE (n)-[:R]->(m))")
        .expect("reference CREATE in a FOREACH body");
    // Only `m` is new: the bare `n` is the outer node.
    assert_eq!(label_sets(&mut c).len(), 2);
}
