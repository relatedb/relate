//! POINT index contract through the public Rust API (ADR-0549): indexed
//! bbox and radius seeks answer through the R-tree postings, and a
//! file-backed reopen keeps them correct (postings are derived state,
//! rebuilt from the replayed graph at open).

use relate::{Connection, Database, Value};
use relate_test_support::uri;

fn near_point_queries(c: &mut Connection) {
    let rows = c
        .query(
            "MATCH (n:P) WHERE point.withinBBox(n.loc, point({x: 0, y: 0}), point({x: 2, y: 2})) \
             RETURN n.id AS id",
        )
        .unwrap();
    assert_eq!(rows.rows, vec![vec![Value::String("near".into())]]);
    let rows = c
        .query(
            "MATCH (n:P) WHERE point.distance(n.loc, point({x: 0, y: 0})) < 2.0 \
             RETURN n.id AS id",
        )
        .unwrap();
    assert_eq!(rows.rows, vec![vec![Value::String("near".into())]]);
}

#[test]
fn point_index_answers_bbox_and_radius() {
    let mut c = Database::open(":memory:").unwrap().connect().unwrap();
    c.query(
        "CREATE (:P {id: 'near', loc: point({x: 1.0, y: 1.0})}), \
                (:P {id: 'far', loc: point({x: 9.0, y: 9.0})})",
    )
    .unwrap();
    c.query("CREATE POINT INDEX locIndex FOR (n:P) ON (n.loc)")
        .unwrap();
    near_point_queries(&mut c);
}

#[test]
fn point_index_seek_correct_after_file_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(uri(dir.path())).unwrap();
        let mut c = db.connect().unwrap();
        c.query(
            "CREATE (:P {id: 'near', loc: point({x: 1.0, y: 1.0})}), \
                    (:P {id: 'far', loc: point({x: 50.0, y: 50.0})})",
        )
        .unwrap();
        c.query("CREATE POINT INDEX locIndex FOR (n:P) ON (n.loc)")
            .unwrap();
    }
    let db = Database::open(uri(dir.path())).unwrap();
    let mut c = db.connect().unwrap();
    near_point_queries(&mut c);
}
