//! A pattern comprehension is its `collect { … }` subquery (ADR-0685): the
//! front lowers one to the other, so the two spellings must answer
//! identically everywhere a user can observe them — the plain list, the
//! WHERE filter, the path variable, an aggregating consumer, a grouping
//! key import, and the no-match empty list. These tests pin that
//! equivalence against the same store, one query pair per shape.

use crate::common;

use common::{connection, list};

/// The shared store: a star of three `R` relationships from one `N` node,
/// each carrying a distinct `v`, so every projection below is observable.
fn star() -> relate::Connection {
    let mut c = connection();
    common::run(
        &mut c,
        "CREATE (n:N {id: 1}) \
         CREATE (n)-[:R {v: 1}]->(:M {v: 1}), \
                (n)-[:R {v: 2}]->(:M {v: 2}), \
                (n)-[:R {v: 3}]->(:M {v: 3})",
    );
    c
}

/// The one pair shape: both spellings must produce the same rows — every
/// column, so a pair projecting beside the list still asserts the
/// projection.
fn assert_same_rows(
    c: &mut relate::Connection,
    comp: &str,
    collect: &str,
) -> Vec<Vec<relate::Value>> {
    let a = common::rows(c, comp);
    let b = common::rows(c, collect);
    assert_eq!(
        a, b,
        "the comprehension and its collect subquery agree\n{comp}\n{collect}"
    );
    a
}

/// The plain list: one element per match, same order both spellings.
#[test]
fn the_plain_list_matches_its_collect_subquery() {
    let mut c = star();
    assert_same_rows(
        &mut c,
        "MATCH (n:N {id: 1}) WITH n RETURN [(n)-[:R]->(m) | m.v] AS l",
        "MATCH (n:N {id: 1}) WITH n RETURN collect { MATCH (n)-[:R]->(m) RETURN m.v } AS l",
    );
}

/// The WHERE filter applies inside both spellings.
#[test]
fn the_where_filter_matches_its_collect_subquery() {
    let mut c = star();
    assert_same_rows(
        &mut c,
        "MATCH (n:N {id: 1}) WITH n \
         RETURN [(n)-[:R]->(m) WHERE m.v > 1 | m.v] AS l",
        "MATCH (n:N {id: 1}) WITH n \
         RETURN collect { MATCH (n)-[:R]->(m) WHERE m.v > 1 RETURN m.v } AS l",
    );
}

/// The path variable binds the whole path per match in both spellings.
#[test]
fn the_path_variable_matches_its_collect_subquery() {
    let mut c = star();
    assert_same_rows(
        &mut c,
        "MATCH (n:N {id: 1}) WITH n \
         RETURN [p = (n)-[:R]->(m) | length(p)] AS l",
        "MATCH (n:N {id: 1}) WITH n \
         RETURN collect { MATCH p = (n)-[:R]->(m) RETURN length(p) } AS l",
    );
}

/// An aggregating consumer folds both spellings' elements the same way.
#[test]
fn an_aggregating_consumer_matches_its_collect_subquery() {
    let mut c = star();
    assert_same_rows(
        &mut c,
        "MATCH (n:N {id: 1}) WITH n \
         UNWIND [(n)-[:R]->(m) | m.v] AS x RETURN sum(x) AS l",
        "MATCH (n:N {id: 1}) WITH n \
         UNWIND collect { MATCH (n)-[:R]->(m) RETURN m.v } AS x \
         RETURN sum(x) AS l",
    );
}

/// An imported node variable is a grouping key through both spellings:
/// one group per `n`, the comprehension reading the grouped `n` exactly
/// as the subquery imports it.
#[test]
fn a_grouping_key_import_matches_its_collect_subquery() {
    let mut c = star();
    common::run(&mut c, "CREATE (:N {id: 2})");
    for (comp, collect) in [
        (
            "MATCH (n:N) RETURN n.id AS id, size([(n)-[:R]->(m) | m.v]) AS c ORDER BY id",
            "MATCH (n:N) RETURN n.id AS id, \
             size(collect { MATCH (n)-[:R]->(m) RETURN m.v }) AS c ORDER BY id",
        ),
        (
            "MATCH (n:N) RETURN n.id AS id, [(n)-[:R]->(m) | m.v] AS c ORDER BY id",
            "MATCH (n:N) RETURN n.id AS id, \
             collect { MATCH (n)-[:R]->(m) RETURN m.v } AS c ORDER BY id",
        ),
    ] {
        assert_same_rows(&mut c, comp, collect);
    }
}

/// No match is the empty list in both spellings, never null, never a row.
#[test]
fn no_match_is_the_empty_list_in_both_spellings() {
    let mut c = star();
    // A node that exists with a relationship type that never matches: the
    // pattern is satisfiable-in-shape but finds nothing. The one spelling
    // of each side, shared with the empty-list assertion below.
    let comp = "MATCH (n:N {id: 1}) WITH n RETURN [(n)-[:X]->(m) | m.v] AS l";
    let collect = "MATCH (n:N {id: 1}) WITH n \
                   RETURN collect { MATCH (n)-[:X]->(m) RETURN m.v } AS l";
    let a = assert_same_rows(&mut c, comp, collect);
    assert_eq!(
        a,
        vec![vec![list(Vec::new())]],
        "no match is the empty list, never null, never zero rows\n{comp}\n{collect}"
    );
}
