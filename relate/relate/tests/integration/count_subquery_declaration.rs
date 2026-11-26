//! End-to-end pins for a subquery expression whose body pattern declares a
//! name the enclosing projection dropped (REL8-1929, GitHub issue #13614):
//! `WITH DISTINCT p0 WHERE COUNT { MATCH (n0) … }` — the inner `MATCH (n0)`
//! is a fresh pattern declaration, never a read of the projected-away
//! outer `n0`, so the aggregating-projection leaf discipline may not
//! reject it. Live 2026.10.1 accepts the shape (probed 2026-11-21: the
//! issue's query returns 0 rows; a `WITH count(*)` variant returns its
//! row). A name the projection still exposes is a correlation the
//! discipline keeps checking, and a WHERE expression reading a dropped
//! name directly stays rejected.

use crate::common;

use common::{connection, err_text, rows};
use relate::Value;

/// The issue's example: `n0` was consumed by the outer pattern but not
/// projected by `WITH DISTINCT p0`; the COUNT body's `MATCH (n0)` binds
/// `n0` anew inside the body, so the WHERE's subclause reads nothing the
/// projection dropped.
#[test]
fn a_count_body_pattern_declares_a_name_the_projection_dropped() {
    let mut c = connection();
    c.query("CREATE (:L8 {k0: 614054904})-[:RT9]->(:L8)")
        .unwrap();
    assert_eq!(
        rows(
            &mut c,
            "MATCH p0 = ( {k0: 614054904}) -[r0:rt9]-> (n0:l8)
             WITH DISTINCT p0
             WHERE COUNT { MATCH (n0) WITH p0 AS alias9 } >= 0
             RETURN p0"
        ),
        Vec::<Vec<Value>>::new(),
        "no :l8 rows match, and nothing rejects the body's own n0"
    );
}

/// The same declaration shape beside an aggregate projection: `WITH
/// count(*) AS c` drops every input name, and the EXISTS body's `MATCH (n)`
/// is a fresh binding — live accepts and returns the count row.
#[test]
fn an_exists_body_pattern_declares_a_name_an_aggregate_projection_dropped() {
    let mut c = connection();
    c.query("CREATE (:A)-[:R]->(:B)").unwrap();
    assert_eq!(
        rows(
            &mut c,
            "MATCH (n:A) WITH count(*) AS c
             WHERE EXISTS { MATCH (n)-[:R]->() RETURN 1 }
             RETURN c"
        ),
        vec![vec![Value::Int(1)]]
    );
}

/// The discipline's own rejections stay: a WHERE expression reading a
/// dropped name through a property the projection did not key on is
/// inaccessible (the front end's undefined-name rejection; a verbatim
/// read of the DISTINCT key expression itself stays legal).
#[test]
fn direct_reads_of_dropped_names_stay_rejected() {
    let mut c = connection();
    c.query("CREATE (:L8 {k0: 1})").unwrap();
    assert!(
        err_text(c.query("MATCH (n0:l8) WITH DISTINCT n0.k0 AS k WHERE n0.other = 1 RETURN k"))
            .contains("not defined"),
        "a WHERE reading the dropped n0 is undefined, not a row read"
    );
}
