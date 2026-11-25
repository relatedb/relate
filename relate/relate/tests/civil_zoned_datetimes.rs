//! ADR-0683's far-year zoned datetimes, end to end: the TCK sentinels'
//! shapes — parse into a property, order against in-range zoned literals
//! across the representations, serve the same counts through a RANGE
//! INDEX, reopen through the WAL, and the issue-#13165 shape (a far-year
//! `Z` value on a relationship property).

mod common;

use common::{connection, reopened_under, scalar};
use relate::Value;

/// The TCK sentinels setup: the two far-year values and the in-range one
/// between them.
const SETUP: &str = "CREATE (:Event {at: datetime('-999999999-01-01T05:00+18:00')}),
       (:Event {at: datetime('2020-01-01T00:00Z')}),
       (:Event {at: datetime('+999999999-12-31T23:59:59.999999999-18:00')})";

fn count_where(connection: &mut relate::Connection, predicate: &str) -> Value {
    scalar(
        connection,
        &format!("MATCH (n:Event) WHERE {predicate} RETURN count(n) AS c"),
    )
}

#[test]
fn far_year_zoned_values_parse_and_store() {
    let mut c = connection();
    c.query(SETUP).unwrap();
    assert_eq!(
        scalar(&mut c, "MATCH (n:Event) RETURN count(n) AS c"),
        Value::Int(3)
    );
}

#[test]
fn open_ranges_order_across_the_representations() {
    // The corpus predicates: the far bounds and the in-range middle value
    // order against each other however the two sides were parsed.
    let mut c = connection();
    c.query(SETUP).unwrap();
    assert_eq!(
        count_where(&mut c, "n.at > datetime('2020-01-01T00:00Z')"),
        Value::Int(1),
        "only the +999999999 value is above the in-range middle"
    );
    assert_eq!(
        count_where(&mut c, "n.at < datetime('2020-01-01T00:00Z')"),
        Value::Int(1),
        "only the -999999999 value is below the in-range middle"
    );
    // The -18:00 far value's instant is 18h later than the same wall clock
    // read at Z, so it clears the Z bound; the equal-instant bound is not
    // below itself.
    assert_eq!(
        count_where(
            &mut c,
            "n.at > datetime('+999999999-12-31T23:59:59.999999999Z')"
        ),
        Value::Int(1)
    );
    assert_eq!(
        count_where(&mut c, "n.at < datetime('-999999999-01-01T05:00+18:00')"),
        Value::Int(0),
        "the value's own instant is equal to the bound, not below it"
    );
}

#[test]
fn a_range_index_serves_the_same_counts() {
    // The sentinels' `USING INDEX` arm: an indexed property holding both
    // representations answers the same counts as the scan.
    let mut c = connection();
    c.query(SETUP).unwrap();
    c.query("CREATE RANGE INDEX Event_at FOR (n:Event) ON (n.at)")
        .unwrap();
    c.query("CALL db.awaitIndexes()").unwrap();
    assert_eq!(
        count_where(&mut c, "n.at > datetime('2020-01-01T00:00Z')"),
        Value::Int(1)
    );
    assert_eq!(
        count_where(&mut c, "n.at < datetime('2020-01-01T00:00Z')"),
        Value::Int(1)
    );
    assert_eq!(
        count_where(
            &mut c,
            "n.at > datetime('+999999999-12-31T23:59:59.999999999Z')"
        ),
        Value::Int(1)
    );
}

#[test]
fn the_wal_reopen_keeps_far_year_zoned_values() {
    let (_dir, mut c) = reopened_under(1 << 30, &[SETUP]);
    assert_eq!(
        scalar(&mut c, "MATCH (n:Event) RETURN count(n) AS c"),
        Value::Int(3)
    );
    assert_eq!(
        count_where(&mut c, "n.at > datetime('2020-01-01T00:00Z')"),
        Value::Int(1),
        "the reopened values still order across the representations"
    );
}

#[test]
fn a_relationship_carries_a_far_year_z_value() {
    // The issue-#13165 shape: `datetime('+275760-09-13T00:00Z')` as a
    // relationship property, read back and ordered.
    let mut c = connection();
    c.query(
        "CREATE (a:Channel {id: 'channel::0'}), (b:State {id: 'state::0'}) \
         CREATE (a)-[:METADATA_STATE {to: datetime('+275760-09-13T00:00Z')}]->(b)",
    )
    .unwrap();
    assert_eq!(
        scalar(
            &mut c,
            "MATCH ()-[r:METADATA_STATE]->() WHERE r.to > datetime('2020-01-01T00:00Z') \
             RETURN count(r) AS c"
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &mut c,
            "MATCH ()-[r:METADATA_STATE]->() \
             RETURN toString(r.to) AS s"
        ),
        // toString renders at full seconds (ADR-0448).
        Value::String("+275760-09-13T00:00:00Z".into())
    );
}

#[test]
fn a_far_year_zoned_value_is_zoned_datetime() {
    // The type predicates read core's kind partition, so the far-year
    // representation answers `IS :: ZONED DATETIME` like its chrono twin.
    let mut c = connection();
    c.query("CREATE (:T {at: datetime('-999999999-01-01T05:00+18:00')})")
        .unwrap();
    assert_eq!(
        scalar(
            &mut c,
            "MATCH (n:T) WHERE n.at IS :: ZONED DATETIME RETURN count(n) AS c"
        ),
        Value::Int(1)
    );
}

#[test]
fn far_year_zoned_values_bucket_by_their_instant_identity() {
    // `DISTINCT` groups the far-year pair by instant-plus-zone, the
    // `DateTime` equivalence (ADR-0446's model): same wall clock at
    // different offsets is two groups, and `=` still says unequal.
    let mut c = connection();
    c.query(
        "CREATE (:G {at: datetime('-999999999-01-01T03:00+02:00')}),
                (:G {at: datetime('-999999999-01-01T01:00Z')})",
    )
    .unwrap();
    assert_eq!(
        scalar(&mut c, "MATCH (n:G) RETURN count(DISTINCT n.at) AS c"),
        Value::Int(2)
    );
}
