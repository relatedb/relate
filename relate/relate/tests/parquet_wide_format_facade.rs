//! The Parquet checkpoint format is the wide-column part encoding
//! (ADR-0553): a plain facade open writes wide parts with no flag, and
//! the wide manifest keeps the store wide.
//!
//! The whole file needs the Parquet codec: without it there is no
//! Parquet checkpoint to speak of.

#![cfg(feature = "parquet")]

use relate::{Database, OpenOptions};

/// The store's manifest `format_version`.
fn manifest_version(dir: &std::path::Path) -> u64 {
    let bytes = std::fs::read(dir.join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    manifest["format_version"].as_u64().unwrap()
}

#[test]
fn a_plain_open_writes_the_wide_encoding_and_the_manifest_keeps_it_wide() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("db");
    let uri = dir.to_str().unwrap();

    let db = Database::open_with(uri, OpenOptions::default()).unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:Person {name: 'ada', age: 36})")
        .unwrap();
    conn.query("CREATE (:Person {name: 'bob', age: 41})")
        .unwrap();
    db.checkpoint().unwrap();
    drop((conn, db));
    assert_eq!(
        manifest_version(&dir),
        5,
        "the Parquet codec publishes the wide manifest with no flag"
    );

    // Reopened plainly: the wide manifest keeps the store wide, and every
    // property survives.
    let db = Database::open(uri).unwrap();
    let mut conn = db.connect().unwrap();
    let rows = conn
        .query("MATCH (n:Person) RETURN n.name AS name, n.age AS age ORDER BY name")
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    db.checkpoint().unwrap();
    drop((conn, db));
    assert_eq!(manifest_version(&dir), 5, "a wide store stays wide");
}
