//! The open's peak transient memory (ADR-0664): `Database::open_peak_transient_bytes`
//! after an open and after a Follow gap reload, and the `load_threads` option.

use crate::test_opts::{following, wait_count, writable};
use crate::{Database, OpenOptions};

fn populate(db: &Database, label: &str) {
    db.connect()
        .unwrap()
        .execute(&format!(
            "UNWIND range(1, 3000) AS i CREATE (:{label} {{id: i}})-[:R]->(:Leaf)"
        ))
        .unwrap();
    db.checkpoint().unwrap();
}

#[test]
fn open_peak_is_zero_on_an_empty_memory_store() {
    let db = Database::open(":memory:").unwrap();
    assert_eq!(db.open_peak_transient_bytes(), 0);
}

#[test]
fn open_peak_is_positive_after_opening_a_checkpointed_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let uri = path.to_str().unwrap();
    {
        let db = Database::open_with(uri, writable()).unwrap();
        populate(&db, "Seed");
    }
    for threads in [1, 4] {
        let db = Database::open_with(
            uri,
            OpenOptions {
                load_threads: Some(threads),
                ..writable()
            },
        )
        .unwrap();
        assert!(db.open_peak_transient_bytes() > 0, "threads={threads}");
    }
}

#[test]
fn open_peak_is_replaced_by_a_follow_gap_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let uri = path.to_str().unwrap();
    let writer = Database::open_with(uri, writable()).unwrap();
    writer.connect().unwrap().execute("CREATE (:Seed)").unwrap();
    writer.checkpoint().unwrap();
    let follower = Database::open_with(uri, following()).unwrap();
    wait_count(&follower, "Seed", 1);
    let opened = follower.open_peak_transient_bytes();

    follower.park_follow();
    populate(&writer, "Missed");
    follower.resume_follow();
    wait_count(&follower, "Missed", 3000);

    assert!(follower.follow_reloads() >= 1);
    assert!(
        follower.open_peak_transient_bytes() > opened,
        "reload peak {} vs open peak {opened}",
        follower.open_peak_transient_bytes()
    );
}
