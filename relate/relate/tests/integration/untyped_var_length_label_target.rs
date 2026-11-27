//! REL8-2216: an untyped `-[*1..2]->` to a labelled target answers
//! exactly what the typed fixed-length chain answers. The walk rejects a
//! stop whose target fails the label before copying its state; the
//! answers (and the label-or-property target arms it must not skip) are
//! pinned here against the typed spelling.

use crate::common;

use common::connection;
use relate::Connection;

fn fixture() -> Connection {
    let mut conn = connection();
    conn.query(
        "CREATE (co:Country {country:'US'}), (co2:Country {country:'DE'}),
                (s1:State {n:'WA'})-[:STATE_IN]->(co),
                (s2:State {n:'BY'})-[:STATE_IN]->(co2),
                (c1:City {city:'Seattle'})-[:CITY_IN]->(s1),
                (c2:City {city:'Munich'})-[:CITY_IN]->(s2),
                (c3:City {city:'Direct'})-[:CITY_IN]->(co),
                (:Person {age: 30})-[:LIVES_IN]->(c1),
                (:Person {age: 40})-[:LIVES_IN]->(c1),
                (:Person {age: 20})-[:LIVES_IN]->(c2),
                (:Person {age: 50})-[:LIVES_IN]->(c3)",
    )
    .unwrap();
    conn
}

/// The query's rows as sorted canonical strings (`common::rows` keeps the
/// query text in a failure message).
fn rows(conn: &mut Connection, q: &str) -> Vec<String> {
    let mut out: Vec<String> = common::rows(conn, q)
        .iter()
        .map(|row| format!("{row:?}"))
        .collect();
    out.sort();
    out
}

#[test]
fn untyped_star_matches_typed_chain_per_city() {
    let mut conn = fixture();
    let untyped = rows(
        &mut conn,
        "MATCH (p:Person)-[:LIVES_IN]->(c:City)-[*1..2]->(co:Country) \
         RETURN c.city AS city, co.country AS country, count(p) AS n",
    );
    // the 1-hop (Direct) and 2-hop (via State) routes both answer
    assert_eq!(untyped.len(), 3, "{untyped:?}");
    let typed_two = rows(
        &mut conn,
        "MATCH (p:Person)-[:LIVES_IN]->(c:City)-[:CITY_IN]->(:State)-[:STATE_IN]->(co:Country) \
         RETURN c.city AS city, co.country AS country, count(p) AS n",
    );
    let typed_one = rows(
        &mut conn,
        "MATCH (p:Person)-[:LIVES_IN]->(c:City)-[:CITY_IN]->(co:Country) \
         RETURN c.city AS city, co.country AS country, count(p) AS n",
    );
    let mut typed = [typed_two, typed_one].concat();
    typed.sort();
    assert_eq!(untyped, typed);
}

#[test]
fn property_target_still_sees_the_rel_variable() {
    let mut conn = fixture();
    // a property-bearing target and a rel-variable WHERE keep the
    // post-bind accept (no pre-clone gate): same rows as the plain form
    let expected = rows(
        &mut conn,
        "MATCH (c:City)-[*1..2]->(co:Country) WHERE co.country = 'US' \
         RETURN c.city, co.country",
    );
    let with_prop = rows(
        &mut conn,
        "MATCH (c:City)-[r*1..2]->(co:Country {country:'US'}) WHERE size(r) >= 1 \
         RETURN c.city, co.country",
    );
    assert_eq!(with_prop, expected);
}
