//! ADR-0458 §7: the boolean predicate positions (a `MATCH`/`WITH` `WHERE`
//! and the `FILTER` clause) apply the runtime list/path conversion the
//! boolean operators use. A property read has no statically known type, so
//! the front end's literal check cannot fire and Neo4j converts at run time:
//! an empty list is false, a non-empty list is true. Live-pinned on Neo4j
//! 2026.10.1 (`MATCH (n:LABEL) WHERE n.coll AND n.bool RETURN n` returns
//! the non-empty/true node).

mod common;

use relate::Value;

use common::{connection, error_class_literal, row, rows};

#[test]
fn a_list_valued_property_coerces_to_a_predicate() {
    let mut c = connection();
    c.query(
        "CREATE (n:CoerceLabel {name: 'n', coll: [1, 2, 3], bool: true}) \
         CREATE (:CoerceLabel {coll: [], bool: true}) \
         CREATE (:CoerceLabel {coll: [1, 2, 3], bool: false}) \
         CREATE ({coll: [1, 2, 3], bool: true}) \
         CREATE (:CoerceLabel)",
    )
    .unwrap();

    // Only the first labelled node is non-empty AND true; the unlabelled
    // non-empty/true node is outside the pattern, the empty list is false,
    // and the missing properties are null (never true).
    let r = row(
        &mut c,
        "MATCH (n:CoerceLabel) WHERE n.coll AND n.bool RETURN n.name AS name",
    );
    assert_eq!(r, vec![Value::String("n".into())]);

    // The FILTER-clause spelling of the same predicate does the same: both
    // labelled nodes with a non-empty list pass (the second has bool false,
    // which FILTER does not read).
    let mut names: Vec<Option<String>> = rows(
        &mut c,
        "MATCH (n:CoerceLabel) FILTER n.coll RETURN n.name AS name",
    )
    .into_iter()
    .map(|r| r[0].as_string().map(str::to_string))
    .collect();
    names.sort();
    assert_eq!(names, vec![None, Some("n".to_string())]);
}

#[test]
fn a_non_boolean_property_predicate_is_a_type_error() {
    let mut c = connection();
    c.query("CREATE (:CoerceNumber {num: 1})").unwrap();
    // The typed class (ADR-0511), asserted through the same public
    // metadata path the class tables read — not a message substring.
    assert_eq!(
        error_class_literal(&mut c, "MATCH (n:CoerceNumber) WHERE n.num RETURN n"),
        "TypeError",
    );
}

#[test]
fn a_null_predicate_does_not_hold() {
    let mut c = connection();
    c.query("CREATE (:CoerceNull)").unwrap();
    assert!(rows(&mut c, "MATCH (n:CoerceNull) WHERE n.missing RETURN n").is_empty());
}

#[test]
fn the_aggregating_having_coerces_and_rejects_like_where() {
    let mut c = connection();
    c.query("CREATE (:CoerceAgg {ls: [1, 2], num: 5}), (:CoerceAgg {ls: []})")
        .unwrap();
    // The non-empty-list group is the only row (live: the same query on
    // 2026.10.1 returns exactly the `[1, 2]` group); the empty one drops.
    let r = row(
        &mut c,
        "MATCH (n:CoerceAgg) WITH n.ls AS ls, count(*) AS k WHERE ls RETURN ls",
    );
    assert_eq!(
        r,
        vec![Value::List(vec![Value::Int(1), Value::Int(2)].into())]
    );
    // A non-boolean HAVING is the typed rejection live raises (22N01),
    // not a silent group drop.
    assert_eq!(
        error_class_literal(
            &mut c,
            "MATCH (n:CoerceAgg) WITH n.num AS num, count(*) AS k WHERE num RETURN num",
        ),
        "TypeError",
    );
}

/// The remaining predicate positions of ADR-0458 §7 — the pattern engines'
/// leg `WHERE`s (the GPM inline form and the QPP quantified form) and the
/// list comprehension's `WHERE` — convert a runtime list and raise the
/// typed rejection on a non-boolean, live-pinned on 2026.10.1: the GPM
/// `-[r WHERE r.ls]->` single-hop and the QPP `( …-[r]->(b) WHERE r.ls )+`
/// both accept the non-empty-list edge, `r.num` raises 22N01, and
/// `[x IN [1] WHERE n.ls | x]` yields `[1]`/`[]` per node.
#[test]
fn the_pattern_leg_and_comprehension_wheres_coerce_lists_and_reject_non_booleans() {
    let mut c = connection();
    c.query(
        "CREATE (:CoercePat {id: 1})-[:RL {ls: [1], num: 5}]->(:CoercePat {id: 2}) \
         CREATE (:CoercePat {id: 2})-[:RL {ls: []}]->(:CoercePat {id: 3}) \
         CREATE (:CoerceComp {id: 1, ls: [1, 2], num: 5}) \
         CREATE (:CoerceComp {id: 2, ls: []})",
    )
    .unwrap();

    // The GPM inline leg WHERE: only the non-empty-list edge matches.
    let r = rows(
        &mut c,
        "MATCH (a:CoercePat {id: 1})-[r WHERE r.ls]->(b) RETURN b.id AS id",
    );
    assert_eq!(r, vec![vec![Value::Int(2)]]);

    // The QPP leg WHERE converts per trial: the walk takes the
    // non-empty-list edge and the empty one ends it (live: the 1-hop path).
    let r = rows(
        &mut c,
        "MATCH p = ( (a:CoercePat {id: 1})-[r:RL]->(b) WHERE r.ls )+ RETURN length(p) AS len",
    );
    assert_eq!(r, vec![vec![Value::Int(1)]]);

    // A non-boolean leg WHERE is the typed rejection (live 22N01).
    assert_eq!(
        error_class_literal(
            &mut c,
            "MATCH (a:CoercePat {id: 1})-[r WHERE r.num]->(b) RETURN b.id AS id",
        ),
        "TypeError",
    );

    // The comprehension WHERE converts per element: the non-empty node
    // keeps the element, the empty one drops it.
    let r = rows(
        &mut c,
        "MATCH (n:CoerceComp) RETURN [x IN [1] WHERE n.ls | x] AS out ORDER BY n.id",
    );
    assert_eq!(
        r,
        vec![
            vec![Value::List(vec![Value::Int(1)].into())],
            vec![Value::List(vec![].into())],
        ]
    );

    // A non-boolean comprehension WHERE is the typed rejection (live 22N01).
    assert_eq!(
        error_class_literal(
            &mut c,
            "MATCH (n:CoerceComp {id: 1}) RETURN [x IN [1] WHERE n.num | x] AS out",
        ),
        "TypeError",
    );
}
