//! The per-key value statistics through `db.stats.retrieve('GRAPH COUNTS')`
//! (ADR-0696), over the real `Connection`: a consult requests a build, a
//! later statement's completion serves it, and the counts read lists what is
//! built without ever building or requesting anything.
//!
//! No production planner rung consults the statistics yet, so the consult is
//! driven through the store's test seam (`Shared::test_plan_consult`), the
//! way a planner rung will consult.

use std::collections::BTreeMap;

use crate::{Connection, Database, Value};

/// 300 cities over four countries (a quarter of the rows each), and ten
/// `:Rare` nodes that nothing consults.
fn store() -> (Database, Connection) {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("UNWIND range(1, 300) AS i CREATE (:City {country: 'C' + toString(i % 4), pop: i})")
        .unwrap();
    c.query("UNWIND range(1, 10) AS i CREATE (:Rare {k: i})")
        .unwrap();
    (db, c)
}

fn consult(db: &Database, label: &str, property: &str) {
    *db.inner.test_plan_consult.lock().unwrap() = Some((label.to_string(), property.to_string()));
}

/// `(statistics epoch, a build is requested)`, read under the publication
/// guard the graph cell requires.
fn state(db: &Database) -> (u64, bool) {
    let _read = db.inner.read_guard().unwrap();
    let graph: &relate_executor::Graph = &db.inner.graph;
    (graph.key_stats_epoch(), graph.has_pending_key_stats())
}

/// The entries of `data.keyStatistics`, as maps.
fn key_statistics(c: &mut Connection) -> Vec<BTreeMap<String, Value>> {
    let result = c
        .query("CALL db.stats.retrieve('GRAPH COUNTS') YIELD data RETURN data.keyStatistics AS k")
        .unwrap();
    let Value::List(list) = &result.rows[0][0] else {
        panic!("keyStatistics is not a list: {:?}", result.rows[0][0]);
    };
    list.iter()
        .map(|v| match v {
            Value::Map(m) => (**m).clone(),
            other => panic!("entry is not a map: {other:?}"),
        })
        .collect()
}

/// Empty before any consult; a consult plus a later statement publishes the
/// label's keys; a label nothing consulted never appears.
#[test]
fn a_consult_and_a_later_statement_list_the_key_in_the_counts() {
    let (db, mut c) = store();
    assert!(key_statistics(&mut c).is_empty(), "nothing consulted yet");

    consult(&db, "City", "country");
    // The planning of this statement consults (and requests the build); its
    // own completion serves the request.
    c.query("MATCH (n:City) RETURN count(n) AS c").unwrap();
    c.query("RETURN 1 AS one").unwrap();

    let listed = key_statistics(&mut c);
    let names: Vec<(Value, Value)> = listed
        .iter()
        .map(|e| (e["label"].clone(), e["property"].clone()))
        .collect();
    assert_eq!(
        names,
        [
            (
                Value::String("City".into()),
                Value::String("country".into())
            ),
            (Value::String("City".into()), Value::String("pop".into())),
        ],
        "every key of the built label, and not the unconsulted :Rare"
    );
    let country = &listed[0];
    assert_eq!(country["count"], Value::Int(300));
    assert_eq!(country["distinct"], Value::Int(4));
    assert_eq!(country["provenance"], Value::String("sketched".into()));
    let pop = &listed[1];
    assert_eq!(pop["min"], Value::Int(1));
    assert_eq!(pop["max"], Value::Int(300));
    assert_eq!(pop["sum"], Value::Int(45_150));
}

/// The read lists what is built and nothing more. With nothing requested
/// it moves no epoch and requests nothing; with a build requested, the
/// listing the statement returns is what was built BEFORE it (the read never
/// builds), and only the statement's completion serves the request, so the
/// next listing shows it.
#[test]
fn the_listing_shows_what_was_built_before_the_statement_ran() {
    let (db, mut c) = store();
    let (epoch, pending) = state(&db);
    assert!(!pending);
    assert!(key_statistics(&mut c).is_empty());
    assert_eq!(
        state(&db),
        (epoch, false),
        "a read with nothing asked changes nothing"
    );

    // A consult during planning requests the build; EXPLAIN completes
    // without a pump in between that this test could observe, so assert on
    // the request itself, not on how it was served.
    consult(&db, "Rare", "k");
    consult_only_plan(&mut c);
    consult_off(&db);

    // Whatever has been served by now, one listing is a pure read of the
    // built set: listing twice in a row returns the same entries and, once
    // nothing is pending, moves nothing.
    let first = key_statistics(&mut c);
    let settled = state(&db);
    assert!(!settled.1, "completions have served the request");
    let second = key_statistics(&mut c);
    assert_eq!(first, second, "a listing is a pure read");
    assert_eq!(state(&db), settled, "and changes no epoch");
    for entry in &second {
        assert_eq!(entry["label"], Value::String("Rare".into()));
    }
    assert_eq!(second.len(), 1, "the one key of :Rare");
}

/// Plan one statement over `:Rare` so the configured consult fires.
fn consult_only_plan(c: &mut Connection) {
    c.query("MATCH (n:Rare) RETURN count(n) AS c").unwrap();
}

fn consult_off(db: &Database) {
    *db.inner.test_plan_consult.lock().unwrap() = None;
}
