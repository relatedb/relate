//! `db.labels` and `db.relationshipTypes` answer from the maintained
//! per-label and per-type counts when the view is exact, and from a graph
//! walk otherwise. Both routes must answer the same names: after deletes,
//! label removal, and from inside a transaction that holds uncommitted
//! writes (the walk route).

use crate::common;

fn names(c: &mut relate::Connection, q: &str) -> Vec<String> {
    common::first_column_strings(&common::run(c, q))
}

const LABELS: &str = "CALL db.labels() YIELD label RETURN label";
const TYPES: &str = "CALL db.relationshipTypes() YIELD relationshipType RETURN relationshipType";

#[test]
fn names_track_deletes_and_label_removal() {
    let mut c = common::connection();
    common::run(&mut c, "CREATE (:A)-[:R]->(:B:C), (:A)-[:S]->(:A)");
    assert_eq!(names(&mut c, LABELS), ["A", "B", "C"]);
    assert_eq!(names(&mut c, TYPES), ["R", "S"]);

    common::run(&mut c, "MATCH (n:B) REMOVE n:C");
    common::run(&mut c, "MATCH (n:B) DETACH DELETE n");
    assert_eq!(names(&mut c, LABELS), ["A"]);
    assert_eq!(names(&mut c, TYPES), ["S"]);

    common::run(&mut c, "MATCH (n) DETACH DELETE n");
    assert!(names(&mut c, LABELS).is_empty());
    assert!(names(&mut c, TYPES).is_empty());
}

#[test]
fn an_open_transaction_sees_its_own_uncommitted_names() {
    let mut c = common::connection();
    common::run(&mut c, "CREATE (:A)");
    c.begin().unwrap();
    common::run(&mut c, "CREATE (:Z)-[:T]->(:A)");
    assert_eq!(names(&mut c, LABELS), ["A", "Z"]);
    assert_eq!(names(&mut c, TYPES), ["T"]);
    c.rollback().unwrap();
    assert_eq!(names(&mut c, LABELS), ["A"]);
    assert!(names(&mut c, TYPES).is_empty());
}
