//! The temporal identity model (ADR-0446): a zoned datetime has **three**
//! relations, and each is the one its consumer needs.
//!
//! * `=` (`Temporal::equals`) compares the local wall clock plus the zone
//!   identity, so the two halves of a DST fall-back overlap are equal.
//! * `<`/ordering (`Temporal::compare`) compares instants, so the same two
//!   halves are ordered — equality does not imply "neither is less".
//! * `DISTINCT`/grouping (`GroupKey`) compares the UTC instant plus the zone,
//!   so the pair stays two groups — the spec's documented equality/
//!   equivalence divergence.
//!
//! Everything that *buckets* values by identity — an indexed seek, `MERGE`,
//! a uniqueness constraint — keys the same identity as grouping, never the
//! coarser `=` relation. Neo4j's range index and constraints keep the two
//! halves apart: an indexed equality seek finds only the exact half, a
//! uniqueness constraint accepts both, and `MERGE` through the constraint's
//! backing index creates the second node. These tests pin those verdicts on
//! the same pair (verified against Neo4j 2026.09).

mod common;

use relate::Value;

use common::{
    assert_comparison_cases, connection, err_text, scalar, unique_constrained, uniqueness,
};

const FIRST: &str = "datetime('2025-10-26T02:30:00+02:00[Europe/Stockholm]')";
const SECOND: &str = "datetime('2025-10-26T02:30:00+01:00[Europe/Stockholm]')";

const COUNT_X: &str = "MATCH (n:X) RETURN count(n) AS c";

/// The pair is equal and ordered, yet two equivalence classes.
#[test]
fn dst_overlap_halves_are_equal_ordered_and_distinct() {
    let mut c = connection();
    assert_comparison_cases(&mut c, &[(FIRST, SECOND, true, true, false, 2)]);
}

/// Uniqueness keys the value identity: both halves are accepted, whichever
/// order they are created in.
#[test]
fn unique_constraint_accepts_both_dst_overlap_halves() {
    for (first, second) in [(FIRST, SECOND), (SECOND, FIRST)] {
        let mut c = unique_constrained("x_dt", "X", "dt");
        c.query(&format!("CREATE (:X {{dt: {first}}})")).unwrap();
        c.query(&format!("CREATE (:X {{dt: {second}}})")).unwrap();
        assert_eq!(
            scalar(&mut c, COUNT_X),
            Value::Int(2),
            "{first} then {second}"
        );
    }
}

/// The full re-check behind `CREATE CONSTRAINT` buckets by the same key as
/// the incremental probe, so a store already holding the pair accepts the
/// constraint.
#[test]
fn creating_the_constraint_accepts_an_existing_overlap_pair() {
    let mut c = connection();
    c.query(&format!("CREATE (:X {{dt: {FIRST}}})")).unwrap();
    c.query(&format!("CREATE (:X {{dt: {SECOND}}})")).unwrap();
    c.query("CREATE CONSTRAINT x_dt FOR (n:X) REQUIRE n.dt IS UNIQUE")
        .unwrap();
    assert_eq!(scalar(&mut c, COUNT_X), Value::Int(2));
}

/// `MERGE` keys the constraint's backing index, so the other half is a
/// different value and creates a second node (Neo4j 2026.09 does the same).
#[test]
fn merge_sees_the_overlap_halves_as_distinct_under_a_unique_constraint() {
    let mut c = unique_constrained("x_dt", "X", "dt");
    c.query(&format!("MERGE (:X {{dt: {FIRST}}})")).unwrap();
    c.query(&format!("MERGE (:X {{dt: {SECOND}}})")).unwrap();
    assert_eq!(scalar(&mut c, COUNT_X), Value::Int(2));
}

/// `Temporal::equals` (which `Value::eq_non_null` uses for temporals) and
/// `GroupKey` both compare a zoned datetime by its local date-time and zone
/// identity, so the same instant in a different offset is a different value.
#[test]
fn datetimes_at_the_same_instant_with_different_offsets_do_not_conflict() {
    let mut c = unique_constrained("x_dt", "X", "dt");
    c.query("CREATE (:X {dt: datetime('2024-01-01T12:00:00+01:00')})")
        .unwrap();
    // 12:00 at +01:00 is 11:00 UTC, but the local date-times differ.
    c.query("CREATE (:X {dt: datetime('2024-01-01T11:00:00Z')})")
        .unwrap();
    assert_eq!(scalar(&mut c, COUNT_X), Value::Int(2));
    // A distinct local date-time is accepted; its exact repeat conflicts.
    c.query("CREATE (:X {dt: datetime('2024-01-01T12:00:00+02:00')})")
        .unwrap();
    let err = err_text(c.query("CREATE (:X {dt: datetime('2024-01-01T12:00:00+02:00')})"));
    assert_eq!(err, uniqueness("x_dt"));
    assert_eq!(scalar(&mut c, COUNT_X), Value::Int(3));
}

/// `Temporal::equals` and the equivalence key both compare a time by its
/// local time **and** offset: 12:00+01:00 and 13:00+02:00 share an instant
/// but are different times.
#[test]
fn times_at_the_same_instant_with_different_offsets_do_not_conflict() {
    let mut c = unique_constrained("x_dt", "X", "dt");
    c.query("CREATE (:X {dt: time('12:00+01:00')})").unwrap();
    // 12:00 at +01:00 and 13:00 at +02:00 are both 11:00 UTC.
    c.query("CREATE (:X {dt: time('13:00+02:00')})").unwrap();
    assert_eq!(scalar(&mut c, COUNT_X), Value::Int(2));
    // `Z` and `+00:00` spell one time, so the second spelling conflicts.
    c.query("CREATE (:X {dt: time('12:00+00:00')})").unwrap();
    let err = err_text(c.query("CREATE (:X {dt: time('12:00Z')})"));
    assert_eq!(err, uniqueness("x_dt"));
    assert_eq!(scalar(&mut c, COUNT_X), Value::Int(3));
}

/// An equality seek served by a range index keys the value identity, so the
/// `=`-equal partner is a miss (Neo4j 2026.09 agrees); the exact value is
/// found, and an unindexed scan still applies `=` and finds the partner.
#[test]
fn indexed_seek_does_not_find_the_equal_half() {
    let mut c = connection();
    c.query("CREATE RANGE INDEX x_dt_range FOR (n:X) ON (n.dt)")
        .unwrap();
    c.query(&format!("CREATE (:X {{dt: {SECOND}}})")).unwrap();
    // Pattern-map equality, `WHERE` equality and `IN` all probe the index.
    for q in [
        format!("MATCH (n:X {{dt: {FIRST}}}) RETURN count(n) AS c"),
        format!("MATCH (n:X) WHERE n.dt = {FIRST} RETURN count(n) AS c"),
        format!("MATCH (n:X) WHERE n.dt IN [{FIRST}] RETURN count(n) AS c"),
    ] {
        assert_eq!(scalar(&mut c, &q), Value::Int(0), "{q}");
    }
    // The exact value is found.
    assert_eq!(
        scalar(
            &mut c,
            &format!("MATCH (n:X) WHERE n.dt = {SECOND} RETURN count(n) AS c")
        ),
        Value::Int(1)
    );
    // Without the index the scan applies `=`, which equates the halves.
    let mut scan = connection();
    scan.query(&format!("CREATE (:X {{dt: {SECOND}}})"))
        .unwrap();
    assert_eq!(
        scalar(
            &mut scan,
            &format!("MATCH (n:X) WHERE n.dt = {FIRST} RETURN count(n) AS c")
        ),
        Value::Int(1)
    );
}

/// `USING SCAN` is honoured even beside a range index: the scan
/// applies `=` and finds the equal partner, while the same predicate forced
/// through the index does not (Neo4j 2026.09 answers 2 and 1 respectively).
#[test]
fn using_scan_hint_selects_the_filter_scan_not_the_index_seek() {
    let mut c = connection();
    c.query("CREATE RANGE INDEX x_dt_range FOR (n:X) ON (n.dt)")
        .unwrap();
    c.query(&format!("CREATE (:X {{dt: {SECOND}}})")).unwrap();
    assert_eq!(
        scalar(
            &mut c,
            &format!("MATCH (n:X) USING SCAN n:X WHERE n.dt = {FIRST} RETURN count(n) AS c")
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &mut c,
            &format!("MATCH (n:X) USING INDEX n:X(dt) WHERE n.dt = {FIRST} RETURN count(n) AS c")
        ),
        Value::Int(0)
    );
}
