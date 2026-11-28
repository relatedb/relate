//! A point list property stores under one CRS: any differing SRID pair —
//! across CRS families (geographic vs cartesian) or 2D vs 3D within one —
//! is the typed `TypeError` live Neo4j answers (2026.10, Query API probe
//! 2026-11-27), and the statement writes nothing. The same list is fine
//! as a statement result: the rule guards storage, not evaluation.

use crate::common;

use relate::{Error, Value};

use common::connection;

/// The discriminating pair in every spelling: cartesian 2D beside
/// geographic 2D.
const MIXED: &str = "[point({x: 1, y: 1}), point({longitude: 1, latitude: 1})]";

/// The typed rejection's detail prefix, the one fact both assertion
/// shapes read.
const DETAIL_PREFIX: &str = "Collections containing point values with different CRS";

/// The one refusal assertion: the statement must fail with the
/// `TypeError` variant whose detail starts with [`DETAIL_PREFIX`] —
/// `common::refused_literal` runs the refusal, this states the policy.
fn assert_point_list_refused(conn: &mut relate::Connection, statement: &str) {
    let err = common::refused_literal(conn, statement);
    let Error::TypeError(detail) = &err else {
        panic!("{statement}: expected Error::TypeError, got {err:?}");
    };
    assert!(detail.starts_with(DETAIL_PREFIX), "{statement}: {detail}");
}

#[test]
fn a_point_list_property_with_two_crses_is_a_typed_type_error_and_writes_nothing() {
    let mut conn = connection();
    conn.query("CREATE (:Seed {k: 1})-[:R]->(:Seed {k: 2})")
        .unwrap();
    for statement in [
        format!("CREATE (:T {{p: {MIXED}}})"),
        format!("MERGE (:T {{p: {MIXED}}})"),
        format!("MATCH (n:Seed {{k: 1}}) SET n.p = {MIXED}"),
        format!("MATCH (n:Seed {{k: 1}}) SET n = {{p: {MIXED}}}"),
        format!("MATCH (n:Seed {{k: 1}}) SET n += {{p: {MIXED}}}"),
        format!("MATCH ()-[r:R]->() SET r.p = {MIXED}"),
        format!("MATCH (a:Seed {{k: 1}}), (b:Seed {{k: 2}}) CREATE (a)-[:R2 {{p: {MIXED}}}]->(b)"),
    ] {
        assert_point_list_refused(&mut conn, &statement);
    }
    let r = conn
        .query(
            "MATCH (n) WHERE n:T OR n.p IS NOT NULL RETURN count(n) AS nodes \
             UNION ALL MATCH ()-[r]->() WHERE r.p IS NOT NULL OR type(r) = 'R2' \
             RETURN count(r) AS nodes",
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(0)], vec![Value::Int(0)]]);
}

#[test]
fn a_point_list_with_one_crs_stores_and_round_trips() {
    let mut conn = connection();
    conn.query("CREATE (:T {p: [point({x: 1, y: 1}), point({x: 3, y: 4})]})")
        .unwrap();
    let r = conn
        .query("MATCH (n:T) RETURN size(n.p) AS size, n.p[0].srid AS srid")
        .unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(2), Value::Int(7203)]]);
}

#[test]
fn a_point_list_with_two_dimensions_of_one_crs_is_also_refused() {
    let mut conn = connection();
    for statement in [
        "CREATE (:T {p: [point({x: 1, y: 1}), point({x: 1, y: 1, z: 1})]})",
        "CREATE (:T {p: [point({longitude: 1, latitude: 1}), \
         point({longitude: 1, latitude: 1, height: 1})]})",
    ] {
        assert_point_list_refused(&mut conn, statement);
    }
}

#[test]
fn a_mixed_crs_point_list_stays_a_valid_statement_result() {
    let mut conn = connection();
    let r = conn
        .query(&format!("RETURN {MIXED} AS l, size({MIXED}) AS size"))
        .unwrap();
    assert_eq!(r.rows[0][1], Value::Int(2));
}
