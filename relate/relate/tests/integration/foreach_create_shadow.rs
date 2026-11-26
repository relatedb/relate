//! A `FOREACH` body's `CREATE` declares fresh: a node that spells labels or
//! properties shadows an enclosing binding of its name even beside a
//! relationship, as the planner's binder already admits (Neo4j
//! `ForeachAcceptance` "Shadow outer variable in CREATE/MERGE"). A bare
//! node beside a relationship stays a reference to the existing binding.
//! The binder and the executor read one predicate,
//! `PathPattern::node_reuses_binding`.

use crate::common;

use common::{connection, run};

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

/// The other half of the FOREACH shadowing policy: the body's **MERGE**
/// never shadows an outer *entity*. The scope hands over every enclosing
/// name (the CREATE policy's input), so `effectively_bound`'s kind gate is
/// what keeps an entity binding a join — pinned because dropping that gate
/// would silently re-bind `a` to a fresh node, and the second `A` node
/// would grow the same relationship.
#[test]
fn a_merge_in_a_foreach_body_joins_an_outer_entity_instead_of_shadowing() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE (a1:A {id: 1})-[:R]->(:B {id: 1}), (a2:A {id: 2})-[:R]->(:B {id: 2})",
    );
    run(
        &mut c,
        "MATCH (a:A {id: 1}) FOREACH (x IN [1] | MERGE (a)-[:S]->(:C))",
    );
    // Only the outer `a` (id 1) grew the relationship.
    let owners = common::first_column(&run(&mut c, "MATCH (a:A)-[:S]->() RETURN a.id"));
    assert_eq!(owners, vec![relate::Value::Int(1)], "the outer a joined");
}
