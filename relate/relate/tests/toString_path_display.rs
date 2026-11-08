//! CIP-271 display form for paths: `toString(path)`, `toStringOrNull(path)`
//! and `toStringList([path])` render the bare traversal chain — no enclosing
//! `<…>` and no properties. Live Neo4j 2026.10.1 probe (2026-11-08):
//! `MATCH p=(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C) RETURN toString(p)` gives
//! `(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C)`; `toStringList([p])` gives the same
//! string in a list.

mod common;

use common::connection;
use relate::Value;

fn seeded() -> relate::Connection {
    let mut c = connection();
    c.query("CREATE (:A {name: 'a'})-[:FIRST]->(:B {name: 'b'})<-[:SECOND]-(:C {name: 'c'})")
        .unwrap();
    c
}

const CHAIN: &str = "(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C)";

#[test]
fn to_string_of_a_path_is_the_bare_traversal_chain() {
    let mut c = seeded();
    let r = c
        .query(
            "MATCH p=(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C) \
             RETURN toString(p) AS s, toStringOrNull(p) AS sn",
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![
            Value::String(CHAIN.into()),
            Value::String(CHAIN.into())
        ]]
    );
}

#[test]
fn to_string_list_of_a_path_list_renders_the_same_chain() {
    let mut c = seeded();
    let r = c
        .query("MATCH p=(:A)-[:FIRST]->(:B)<-[:SECOND]-(:C) RETURN toStringList([p]) AS sl")
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![Value::List(vec![Value::String(CHAIN.into())].into())]]
    );
}
