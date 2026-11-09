//! The public `OpenOptions::wide_columns` façade (ADR-0534): the flag
//! reaches the codec configuration, opts the store into the wide-column
//! part encoding at its next checkpoint, and the wide manifest then keeps
//! the store wide whatever the next open's flags.
//!
//! The whole file needs the Parquet codec: without it the flag is
//! refused, which is the refusal a no-Parquet build's own lane covers.

#![cfg(feature = "parquet")]

use relate::{Database, OpenOptions};

/// The store's manifest `format_version`.
fn manifest_version(dir: &std::path::Path) -> u64 {
    let bytes = std::fs::read(dir.join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    manifest["format_version"].as_u64().unwrap()
}

#[test]
fn wide_columns_opts_the_store_in_and_the_manifest_keeps_it_wide() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("db");
    let uri = dir.to_str().unwrap();

    let opts = OpenOptions {
        wide_columns: true,
        ..Default::default()
    };
    let db = Database::open_with(uri, opts).unwrap();
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
        "the opt-in publishes the wide manifest"
    );

    // Reopened without the flag: the wide manifest keeps the store wide,
    // and every property survives.
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

#[test]
fn wide_columns_with_markdown_bodies_is_a_semantic_error() {
    let temp = tempfile::tempdir().unwrap();
    let uri = temp.path().join("db");
    let opts = OpenOptions {
        wide_columns: true,
        markdown_body_properties: Some(Default::default()),
        ..Default::default()
    };
    let Err(error) = Database::open_with(uri.to_str().unwrap(), opts) else {
        panic!("the codec-exclusive pair is refused");
    };
    assert!(
        error.to_string().contains("set only one"),
        "the codec-exclusive pair is refused: {error}"
    );
}
