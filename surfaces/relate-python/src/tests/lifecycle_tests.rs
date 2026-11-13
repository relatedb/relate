//! Database lifecycle: creation, reset, persistence, transactions,
//! foreign-thread affinity, read-only mode.

use super::*;

#[test]
fn test_database_creation() {
    with_py(|py| {
        let db = new_database(":memory:");
        assert_eq!(db.node_count(py).unwrap(), 0);
        assert_eq!(db.relationship_count(py).unwrap(), 0);
    });
}

#[test]
fn test_database_repr() {
    with_py(|py| {
        let db = new_database(":memory:");
        assert_eq!(
            db.__repr__(py).unwrap(),
            "Database(nodes=0, relationships=0)"
        );
    });
}

#[test]
fn test_path_suffix_does_not_select_backend() {
    // A `.lance`-looking suffix is not a backend switch: the path is an
    // ordinary Relate store, so a write there round-trips.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.lance");
    let path_str = path.to_str().unwrap().to_string();
    {
        let mut db = new_database(&path_str);
        raw_query(&mut db, "CREATE (:Person {name: 'Alice'})");
    }
    with_py(|py| {
        let db = new_database(&path_str);
        assert_eq!(db.node_count(py).unwrap(), 1);
    });
}

#[test]
fn test_reset_in_memory() {
    let mut db = new_database(":memory:");
    raw_query(&mut db, "CREATE (:Person {name: 'Alice'})");
    with_py(|py| {
        assert_eq!(db.node_count(py).unwrap(), 1);
        db.reset(py).unwrap();
        assert_eq!(db.node_count(py).unwrap(), 0);
    });
}

#[test]
fn test_file_persistence_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("database");
    let path_str = path.to_str().unwrap().to_string();
    {
        let mut db = new_database(&path_str);
        raw_query(&mut db, "CREATE (:Person {name: 'Alice'})");
        with_py(|py| db.flush(py).unwrap());
    }
    with_py(|py| {
        let db2 = new_database(&path_str);
        assert_eq!(db2.node_count(py).unwrap(), 1);
    });
}

#[test]
fn reset_reopens_persistent_directory() {
    with_py(|py| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database");
        let mut db = new_database(&path.to_string_lossy());
        db.execute(py, "CREATE (:Person {name: 'Alice'})", None)
            .unwrap();
        db.reset(py).unwrap();
        assert_eq!(db.node_count(py).unwrap(), 1);
        assert!(path.is_dir());
        assert!(path.join("wal").is_dir());
    });
}

#[test]
fn rollback_discards_transaction() {
    with_py(|py| {
        let db = new_database(":memory:");
        db.begin_transaction(py).unwrap();
        db.execute(py, "CREATE (:Transient)", None).unwrap();
        assert!(db.transaction_active(py).unwrap());
        db.rollback_transaction(py).unwrap();
        assert_eq!(db.node_count(py).unwrap(), 0);
    });
}

#[test]
fn relate_error_carries_binding_metadata() {
    with_py(|py| {
        let db = new_database(":memory:");
        let err = db.execute(py, "INVALID SYNTAX", None).unwrap_err();
        let kind = err_attr(&err, py, "kind");
        let tck_class = err_attr(&err, py, "tck_class");
        assert_eq!(kind, "SyntaxError");
        assert_eq!(tck_class, "SyntaxError");
    });
}

#[test]
fn foreign_thread_engine_use_raises_typed_error_not_panic() {
    // `db` moves to the foreign thread; `db_keep` proves the typed refusal
    // leaves the engine untouched for the owning thread.
    let (db, db_keep): (Py<Database>, Py<Database>) = with_py(|py| {
        let db = new_database(":memory:");
        let db = Py::new(py, db).unwrap();
        let keep = db.clone_ref(py);
        (db, keep)
    });
    let refusal = std::thread::spawn(move || {
        Python::with_gil(|py| {
            match db.call_method(py, "execute", ("MATCH (n) RETURN n",), None) {
                // A panic here (pyo3's unsendable assertion) is exactly the
                // regression this test guards against (GH #352 / LABS-700).
                Ok(_) => panic!("foreign-thread execute unexpectedly succeeded"),
                Err(err) => err,
            }
        })
    })
    .join()
    .unwrap();
    with_py(|py| {
        assert_eq!(err_attr(&refusal, py, "kind"), "WrongThread");
        assert!(refusal.is_instance_of::<RelateNativeError>(py));
        let bound = db_keep.borrow_mut(py);
        bound.execute(py, "CREATE (:T {id: 1})", None).unwrap();
        assert_eq!(bound.node_count(py).unwrap(), 1);
    });
}

#[test]
fn foreign_thread_drop_runs_deferred_cleanup_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("thread-drop");
    let path_str = path.to_str().unwrap().to_string();
    let db: Py<Database> = with_py(|py| {
        let mut db = new_database(&path_str);
        raw_query(&mut db, "CREATE (:Person {name: 'Alice'})");
        Py::new(py, db).unwrap()
    });
    std::thread::spawn(move || {
        // Releasing the last reference here runs the pyclass `Drop` (the
        // deferred checkpoint) on this thread — the unsendable design turned
        // this into an unraisable RuntimeError (GH #352 / LABS-700).
        Python::with_gil(|_py| drop(db));
    })
    .join()
    .unwrap();
    with_py(|py| {
        let db2 = new_database(&path_str);
        assert_eq!(db2.node_count(py).unwrap(), 1);
    });
}

#[test]
fn read_only_rejects_writes_and_skips_checkpoint() {
    with_py(|py| {
        let mut db = Database::new(py, ":memory:".to_string(), "neo4j".to_string(), true).unwrap();
        db.execute(py, "RETURN 1 AS n", None).unwrap();
        let err = db.execute(py, "CREATE ()", None).unwrap_err();
        assert_eq!(err_attr(&err, py, "kind"), "WriteOnReadOnlyAccessDatabase");
        db.flush(py).unwrap();
        db.reset(py).unwrap();
        let err = db.execute(py, "CREATE ()", None).unwrap_err();
        assert_eq!(err_attr(&err, py, "kind"), "WriteOnReadOnlyAccessDatabase");
        db.close(py).unwrap();

        let writable =
            Database::new(py, ":memory:".to_string(), "neo4j".to_string(), false).unwrap();
        writable.execute(py, "CREATE ()", None).unwrap();
    });
}

// LABS-680 (ADR-0477) safe mode is an engine-level opt-in; the Python binding
// never sets it, so there is nothing binding-specific to assert here (the
// engine default is covered by `labs680_safe_mode` in the engine crate).
