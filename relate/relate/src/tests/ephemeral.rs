//! LABS-903: ephemeral opens — a read-only open that **turns** a durable
//! store into the in-memory store. The load itself is unchanged (the engine
//! is memory-first); what changes is that the source is released after it:
//! no open handle, no lock, and the running store is `:memory:`-shaped.

use crate::test_opts::writable;
use crate::{Database, OpenMode};

/// A `.rl` bundle holding one `:Person`, plus its path: seed a store
/// directory, clone it into a bundle, drop the directory.
fn person_bundle(dir: &std::path::Path) -> std::path::PathBuf {
    let store = dir.join("seed");
    {
        let db = Database::open_with(store.to_str().unwrap(), writable()).unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("CREATE (:Person {name: 'Alice'})").unwrap();
        db.checkpoint().unwrap();
    }
    let bundle = dir.join("person.rl");
    Database::clone(
        store.to_str().unwrap(),
        bundle.to_str().unwrap(),
        crate::CloneOptions::default(),
    )
    .unwrap();
    bundle
}

#[test]
fn an_ephemeral_bundle_open_loads_into_memory_and_releases_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = person_bundle(dir.path());

    let db = Database::open_ephemeral(bundle.to_str().unwrap()).unwrap();
    let mut conn = db.connect().unwrap();
    let rows = conn
        .execute("MATCH (p:Person) RETURN p.name AS name")
        .unwrap();
    assert_eq!(rows.rows[0][0], crate::Value::String("Alice".into()));

    // The source is gone underneath — the loaded store does not notice.
    std::fs::remove_file(&bundle).unwrap();
    let rows = conn
        .execute("MATCH (p:Person) RETURN count(p) AS c")
        .unwrap();
    assert_eq!(rows.rows[0][0], crate::Value::Int(1));
}

#[test]
fn an_ephemeral_open_holds_no_lock_another_writer_can_open_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = person_bundle(dir.path());

    let ephemeral = Database::open_ephemeral(bundle.to_str().unwrap()).unwrap();
    // The `.rl` bundle lock is exclusive with a writer: without the release,
    // this writable open fails. With the turn-into, it succeeds at once.
    let writer = Database::open(bundle.to_str().unwrap()).unwrap();
    let mut conn = writer.connect().unwrap();
    conn.execute("CREATE (:Writer {took: true})").unwrap();

    // The ephemeral store is unaffected by the writer's change — it is a
    // frozen in-memory copy, exactly the snapshot freshness it asked for.
    let mut conn = ephemeral.connect().unwrap();
    let rows = conn.execute("MATCH (n) RETURN count(n) AS c").unwrap();
    assert_eq!(rows.rows[0][0], crate::Value::Int(1));
}

/// The illegal combinations are unrepresentable — `OpenMode::Ephemeral` is
/// its own mode, so a writable or following turn-into cannot be spelled —
/// so this pins the shape instead: the mode is read-only and never a
/// follower.
#[test]
fn the_ephemeral_mode_is_read_only_and_never_a_follower() {
    assert!(OpenMode::Ephemeral.is_read_only());
    assert!(!OpenMode::Ephemeral.follows());
}

#[test]
fn a_plain_read_only_open_still_holds_the_source() {
    // The control for the lock test above: without `ephemeral`, the
    // read-only bundle open keeps its host — a writable open beside it
    // fails, which is ADR-0074's exclusive bundle lock.
    let dir = tempfile::tempdir().unwrap();
    let bundle = person_bundle(dir.path());

    let _reader = Database::open_read_only(bundle.to_str().unwrap()).unwrap();
    let err = Database::open(bundle.to_str().unwrap())
        .err()
        .expect("a writable open beside a plain read-only bundle fails");
    assert!(
        err.to_string().contains("lock") || err.to_string().contains("in use"),
        "{err}"
    );
}

#[test]
fn an_ephemeral_native_directory_open_also_releases_the_source() {
    // The turn-into is not bundle-specific: a native directory store loads
    // the same way, so the directory is releasable too.
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    {
        let db = Database::open_with(store.to_str().unwrap(), writable()).unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("CREATE (:Person {name: 'Bob'})").unwrap();
        db.checkpoint().unwrap();
    }

    let db = Database::open_ephemeral(store.to_str().unwrap()).unwrap();
    // A writable open beside the ephemeral one succeeds — no writer lease,
    // no lock is held.
    let writer = Database::open(store.to_str().unwrap()).unwrap();
    let mut conn = writer.connect().unwrap();
    conn.execute("CREATE (:Writer)").unwrap();

    let mut conn = db.connect().unwrap();
    let rows = conn.execute("MATCH (n) RETURN count(n) AS c").unwrap();
    assert_eq!(rows.rows[0][0], crate::Value::Int(1));
}

/// A refused open must not lease the store (the fail-before-lease contract
/// every other knob upholds): an ephemeral open of a *missing* path fails
/// before anything is created, leaving nothing behind.
#[test]
fn an_ephemeral_open_of_a_missing_store_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.rl");
    let err = Database::open_ephemeral(missing.to_str().unwrap())
        .err()
        .expect("an ephemeral open requires an existing store");
    assert!(
        err.to_string().contains("not found") || err.to_string().contains("existing"),
        "{err}"
    );
    assert!(!missing.exists(), "nothing was created");
}
