//! REL8-1732: a user alias spelled with a `__` prefix is a real binding.
//!
//! Neo4j admits the alias and star-projects it. The engine used to test
//! internality as a raw name prefix, so every star-shaped consumer dropped
//! the column. Internality is now "synthesized by the executor and not
//! declared by the statement", read off the declared scope.

mod common;

use relate::Value;

fn star(query: &str) -> (Vec<String>, Vec<Vec<Value>>) {
    let mut c = common::connection();
    let r = common::run(&mut c, query);
    (
        r.header.columns.iter().map(|c| c.to_string()).collect(),
        r.rows,
    )
}

#[test]
fn star_return_keeps_a_double_underscore_alias() {
    let (cols, rows) = star("UNWIND [1,1,2] AS i WITH i AS `__x` RETURN *");
    assert_eq!(cols, vec!["__x"]);
    assert_eq!(rows.len(), 3);
}

#[test]
fn star_with_and_explicit_alias_keep_it() {
    let (cols, rows) = star("UNWIND [1,2] AS i WITH *, i AS `__x` RETURN *");
    assert_eq!(cols, vec!["__x", "i"]);
    assert_eq!(rows.len(), 2);
}

#[test]
fn star_distinct_keys_on_the_alias() {
    let (cols, rows) = star("UNWIND [1,1,2] AS i WITH i AS `__x` RETURN DISTINCT *");
    assert_eq!(cols, vec!["__x"]);
    assert_eq!(rows.len(), 2);
}

#[test]
fn alias_survives_beside_parked_order_keys() {
    let (cols, rows) =
        star("UNWIND [3,1,2] AS i WITH i AS `__ord_x`, i * 10 AS j ORDER BY j DESC RETURN *");
    assert_eq!(cols, vec!["__ord_x", "j"]);
    assert_eq!(rows[0], vec![Value::Int(3), Value::Int(30)]);
}

#[test]
fn alias_survives_beside_aggregation_seeds() {
    let (cols, rows) = star(
        "UNWIND [1,1,2] AS i WITH i AS `__agg_x` WITH *, count(*) AS n ORDER BY `__agg_x` RETURN *",
    );
    assert_eq!(cols, vec!["__agg_x", "n"]);
    assert_eq!(rows[0], vec![Value::Int(1), Value::Int(2)]);
}

#[test]
fn call_subquery_imports_the_alias_under_star_scope() {
    let (cols, rows) =
        star("UNWIND [1,2] AS i WITH i AS `__x` CALL (*) { RETURN `__x` + 1 AS y } RETURN *");
    assert_eq!(cols, vec!["__x", "y"]);
    assert_eq!(rows.len(), 2);
}

#[test]
fn pattern_variable_with_the_prefix_is_visible() {
    let mut c = common::connection();
    common::run(&mut c, "CREATE (:T {v: 1})");
    let r = common::run(&mut c, "MATCH (`__n`:T) RETURN *");
    assert_eq!(
        r.header
            .columns
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>(),
        vec!["__n"]
    );
    assert_eq!(r.rows.len(), 1);
}

#[test]
fn match_predicates_read_the_alias() {
    let mut c = common::connection();
    common::run(&mut c, "UNWIND [1,2,3] AS v CREATE (:T {v: v})");
    let r = common::run(
        &mut c,
        "UNWIND [2,3] AS i WITH i AS `__x` MATCH (n:T) WHERE n.v = `__x` RETURN n.v ORDER BY n.v",
    );
    assert_eq!(r.rows, vec![vec![Value::Int(2)], vec![Value::Int(3)]]);
    let r = common::run(
        &mut c,
        "UNWIND [2,3] AS i WITH i AS `__x` MATCH (n:T)-[r*0..1]-(m:T) WHERE n.v = `__x` RETURN count(*) AS c",
    );
    assert_eq!(r.rows.len(), 1);
}

/// A carried alias spelled like the first parked sort key survives a later
/// `ORDER BY`: the parked prefix is lengthened past every name the
/// statement declares, not only the projection's own items.
#[test]
fn carried_alias_spelled_like_a_parked_key_survives_order_by() {
    let (cols, rows) = star(
        "UNWIND [3,1,2] AS i WITH i AS `__ord0`, i * 10 AS j \
         WITH *, j AS k ORDER BY k DESC RETURN *",
    );
    assert_eq!(cols, vec!["__ord0", "j", "k"]);
    assert_eq!(rows[0], vec![Value::Int(3), Value::Int(30), Value::Int(30)]);
    assert_eq!(rows.len(), 3);
}

/// The same carried alias beside an aggregating `ORDER BY` (the fold and
/// the generic aggregation both park keys).
#[test]
fn carried_alias_spelled_like_a_parked_key_survives_aggregate_order_by() {
    let (cols, rows) = star(
        "UNWIND [1,1,2] AS i WITH i AS `__ord0` \
         WITH *, count(*) AS n ORDER BY n DESC RETURN *",
    );
    assert_eq!(cols, vec!["__ord0", "n"]);
    assert_eq!(rows[0], vec![Value::Int(1), Value::Int(2)]);
}

/// A name declared only inside a nested body is declared statement-wide
/// (the set is computed once at entry over the nested bodies too), and
/// the outer star projection still shows exactly the outer scope plus the
/// body's exports: neither the nested `__ord0` nor the outer parked key
/// is misread as the other.
#[test]
fn nested_body_spelling_does_not_flip_outer_internality() {
    let (cols, rows) = star(
        "UNWIND [3,1,2] AS i WITH i ORDER BY i DESC \
         CALL { WITH i WITH i AS `__ord0` RETURN `__ord0` + 1 AS y } \
         RETURN *",
    );
    assert_eq!(cols, vec!["i", "y"]);
    assert_eq!(rows[0], vec![Value::Int(3), Value::Int(4)]);
    assert_eq!(rows.len(), 3);
}

/// An anonymous CREATE element never lands on a user binding spelled in
/// the `__a` namespace.
#[test]
fn anonymous_element_does_not_overwrite_a_user_binding() {
    let mut c = common::connection();
    let r = common::run(
        &mut c,
        "UNWIND [1,2] AS v WITH v AS `__a1` CREATE (:T) RETURN *",
    );
    let cols: Vec<String> = r.header.columns.iter().map(|c| c.to_string()).collect();
    assert_eq!(cols, vec!["__a1"]);
    assert_eq!(r.rows, vec![vec![Value::Int(1)], vec![Value::Int(2)]]);
}

/// A named path's anonymous seats are freshened past the declared set, so
/// a user binding spelled exactly like a seat (`__node0` / `__trail0`) is
/// neither clobbered by the walk nor read back as the seat's stub value.
#[test]
fn pattern_seats_do_not_clobber_user_bindings_of_the_same_spelling() {
    let mut c = common::connection();
    c.query("CREATE (:A)-[:R]->(:B)").unwrap();
    for seat in ["__node0", "__trail0"] {
        let r = common::run(
            &mut c,
            &format!("WITH 1 AS `{seat}` MATCH p = ()-[]->() RETURN *"),
        );
        let cols: Vec<String> = r.header.columns.iter().map(|c| c.to_string()).collect();
        let at = cols.iter().position(|col| col == seat).unwrap_or_else(|| {
            panic!("the user binding `{seat}` survives the named-path walk, got columns {cols:?}")
        });
        assert_eq!(
            r.rows[0][at],
            Value::Int(1),
            "columns {cols:?}, rows {:?}",
            r.rows
        );
    }
}

/// A `SEARCH` subclause's `SCORE AS` column spelled with the prefix is a
/// declared binding: `RETURN *` star-projects it instead of dropping it as
/// a synthetic column.
#[test]
fn a_search_score_column_spelled_with_the_prefix_survives_star() {
    let mut c = common::connection();
    c.query("CREATE VECTOR INDEX filmVectors FOR (f:Film) ON f.embedding")
        .unwrap();
    c.query("CREATE (:Film {embedding: [1.0, 0.0]})").unwrap();
    let r = common::run(
        &mut c,
        "MATCH (f:Film) SEARCH f IN (VECTOR INDEX filmVectors FOR [1.0, 0.0] LIMIT 1) \
         SCORE AS `__score` RETURN *",
    );
    let cols: Vec<String> = r.header.columns.iter().map(|c| c.to_string()).collect();
    assert!(
        cols.contains(&"__score".to_string()),
        "the score column survives the star projection, got columns {cols:?}"
    );
}
