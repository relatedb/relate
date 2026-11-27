//! ADR-0703's open wiring, end to end: the opt-in attaches the store at
//! open, writes land while it is attached, and every read answers
//! unchanged. The reads here cannot prove WHICH tier answered them (the
//! compact records always can, by the seam's miss contract) — the delta
//! overlay's own behavior is pinned where it is observable, in the
//! executor-side dispatch tests (`graph/vortex_store/tests.rs`: a
//! write script vs an unattached twin). Compiled only behind the feature
//! it tests.
//!
//! A file of its own, not the shared integration binary: the negative
//! control below depends on the preview Once staying unset for this
//! process — the process-global opt-in's rule (a closed-gate test never
//! shares a binary with env-setting tests).

#![cfg(feature = "vortex-store")]

use relate::{Database, OpenOptions, Value};

#[test]
fn the_typed_field_attaches_the_store_and_answers_unchanged() {
    // The attach happens at open on the empty graph; the writes below
    // land while it is attached (ADR-0665's versioning step 1 feeds the
    // delta) — the wiring's contract in one pass, and the answers must
    // equal the compact records' own.
    let options = OpenOptions {
        vortex_store: true,
        ..Default::default()
    };
    let db = Database::open_with(":memory:", options).unwrap();
    assert!(db.vortex_store_attached());
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:Person {name: 'ann', age: 30}), (:Person {name: 'bob', age: 40})")
        .unwrap();

    // Point read through the seam, the equality seek, and the aggregate
    // folds — the answers are the compact records' own (the seam's
    // executor-side tests pin the parity; here the open wiring's contract
    // is that the shapes answer at all, unchanged).
    let point = conn
        .query("MATCH (p:Person {name: 'ann'}) RETURN p.age AS age")
        .unwrap();
    assert_eq!(point.rows[0][0], Value::Int(30));
    let folds = conn
        .query("MATCH (p:Person) RETURN min(p.age) AS lo, max(p.age) AS hi, count(p.age) AS n")
        .unwrap();
    assert_eq!(folds.rows[0][0], Value::Int(30));
    assert_eq!(folds.rows[0][1], Value::Int(40));
    assert_eq!(folds.rows[0][2], Value::Int(2));
}

#[test]
fn an_unopted_open_in_the_same_process_stays_default() {
    // The negative control: no field, no row — the compact records answer,
    // and the same process keeps an unopted open default.
    let db = Database::open(":memory:").unwrap();
    assert!(!db.vortex_store_attached());
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:Person {name: 'bob', age: 40})")
        .unwrap();
    let row = conn
        .query("MATCH (p:Person {name: 'bob'}) RETURN p.age AS age")
        .unwrap();
    assert_eq!(row.rows[0][0], Value::Int(40));
}
