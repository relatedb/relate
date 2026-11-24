//! The `tx_metadata` argument of `Database.execute` (ADR-0610): the
//! Python binding's embedded metadata channel. The kwarg's dict converts
//! through the parameter policy and seeds the connection's
//! per-transaction map before the statement runs — landing, serialized,
//! in the auto-commit write's transaction record, which the test reads
//! back from the store's retained records (the doc-hidden observation
//! surface `relate::Database::transaction_records`, the same read-back
//! the engine suite's `value_provenance.rs` uses).
//!
//! `RELATE_PREVIEW` is process-global and this module is part of the one
//! test binary `cargo test -p relate-python` runs, so the gate is set
//! once, before the first open, through the shared fixture — and no
//! module here asserts value-provenance's closed side (the engine suite
//! owns that, in its own binary).

use std::collections::BTreeMap;

use pyo3::types::{PyDict, PyList};

use super::*;

/// The `by: app, run: 42` metadata dict every seed test passes — the same
/// map the engine suite's `by_app()` spells, one key per converted value
/// kind (a string and an integer).
fn by_app_run(py: Python<'_>) -> Bound<'_, PyDict> {
    let metadata = PyDict::new(py);
    metadata.set_item("by", "app").unwrap();
    metadata.set_item("run", 42i64).unwrap();
    metadata
}

/// The same map as [`by_app_run`], on the engine side — the record's
/// serialized form.
fn by_app_run_value() -> BTreeMap<String, relate::Value> {
    BTreeMap::from([
        ("by".to_string(), relate::Value::String("app".into())),
        ("run".to_string(), relate::Value::Int(42)),
    ])
}

/// The commit sequence a `relate:<n>` bookmark names.
fn bookmark_sequence(bookmark: &Option<String>) -> u64 {
    bookmark
        .as_deref()
        .and_then(|b| b.strip_prefix("relate:"))
        .and_then(|n| n.parse().ok())
        .expect("the auto-commit write carries a bookmark")
}

#[test]
fn tx_metadata_seeds_the_auto_commit_record() {
    with_py(|py| {
        relate_test_support::preview::set_preview_once("value-provenance");
        let db = new_database(":memory:");
        // One statement carrying both arguments: the parameters bind, the
        // metadata seeds, the write commits — and the record the bookmark
        // names carries the map.
        let params = PyDict::new(py);
        params.set_item("name", "Ada").unwrap();
        let metadata = by_app_run(py);
        let result = db
            .execute(
                py,
                "CREATE (:Person {name: $name})",
                Some(params.as_any()),
                Some(metadata.as_any()),
            )
            .unwrap();
        let sequence = bookmark_sequence(&result.bookmark);
        let record = db
            .engine
            .as_ref()
            .unwrap()
            .db
            .transaction_records()
            .unwrap()
            .into_iter()
            .find(|record| record.commit_ts == sequence)
            .expect("the auto-commit write retained a record");
        assert_eq!(record.metadata.as_ref(), Some(&by_app_run_value()));
        // Cleared after (ADR-0610): a later write carries no metadata.
        db.execute(py, "CREATE (:Person {name: 'Bob'})", None, None)
            .unwrap();
        assert_eq!(
            db.engine
                .as_ref()
                .unwrap()
                .db
                .transaction_records()
                .unwrap()
                .iter()
                .filter(|record| record.metadata.is_some())
                .count(),
            1
        );
    });
}

#[test]
fn test_tx_metadata_shape_errors_are_dbapi_worded() {
    with_py(|py| {
        let list = PyList::empty(py).into_any();
        let err = optional_tx_metadata_to_cypher(Some(&list)).unwrap_err();
        assert!(err.is_instance_of::<PyTypeError>(py));
        let msg = err.to_string();
        assert!(
            msg.contains("transaction metadata must be a dict"),
            "unexpected message: {msg}"
        );
        assert!(msg.contains("'list'"), "message must name the type: {msg}");

        let dict = PyDict::new(py);
        dict.set_item(42, 1).unwrap();
        let err = optional_tx_metadata_to_cypher(Some(dict.as_any())).unwrap_err();
        assert!(err.is_instance_of::<PyTypeError>(py));
        let msg = err.to_string();
        assert!(
            msg.contains("transaction metadata keys must be strings"),
            "unexpected message: {msg}"
        );
        assert!(msg.contains("'int'"), "message must name the type: {msg}");
    });
}
