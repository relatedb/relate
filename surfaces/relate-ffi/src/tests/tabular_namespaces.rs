use crate::abi::relate_status;
use crate::result::*;
use crate::support::{cell, open_memory_db, run, str_of, Session};
use crate::tabular::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

/// `relate_connect_namespace` opens a connection in the named fixed
/// namespace (ADR-0117 §3); an unknown name fails with CE's
/// `DatabaseNotFound` classification on the error channel. A
/// `system_ontologies` connection is visible under the `ontology`
/// opt-in (ADR-0593) and not queryable
/// (ADR-0229 §1/§2): selecting it succeeds, a data query on it fails with
/// the 42N17 text, and the ADR-0116 §2 administration commands still run
/// from there.
#[test]
fn ffi_connect_namespace_selects_fixed_namespace() {
    // The `system_ontologies` connect needs the `ontology` opt-in
    // (ADR-0593): without it the namespace is invisible and the connect
    // fails as an unknown name (the closed pins live in the engine's
    // preview-gate suite).
    relate_test_support::preview::preview_ontology();
    let sys = CString::new("system_ontologies").unwrap();
    let unknown = CString::new("movies").unwrap();
    let mut conn: *mut relate_connection = ptr::null_mut();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let db = open_memory_db();

        assert!(matches!(
            relate_connect_namespace(db, sys.as_ptr(), &mut conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(!conn.is_null());
        // The one statement kind that still runs there (ADR-0116 §2): the
        // SHOW DATABASES rows, and the third row is system_ontologies.
        let result = run(conn, "SHOW DATABASES");
        assert_eq!(str_of(cell(result, 2, 0)), "system_ontologies");
        relate_destroy_result(result);
        // A data query routed there fails with ADR-0229 §2's 42N17 text —
        // a statement failure, so it rides the result (ADR-0502 §2).
        let q = CString::new("MATCH (n) RETURN count(n) AS n").unwrap();
        let mut failed: *mut relate_query_result = ptr::null_mut();
        assert!(matches!(
            relate_query_params(conn, q.as_ptr(), ptr::null(), &mut failed, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(!failed.is_null());
        assert_eq!(relate_result_is_error(failed), 1);
        let shape = CStr::from_ptr(relate_result_error_json(failed))
            .to_str()
            .unwrap();
        let shape: serde_json::Value = serde_json::from_str(shape).unwrap();
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.ends_with(
                    "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead."
                )),
            "error shape: {shape}"
        );
        relate_destroy_result(failed);
        relate_disconnect(conn);

        // Unknown namespace: RELATE_ERROR, NULL out-param, the CE
        // classification on the error channel.
        conn = ptr::null_mut();
        let mut error_json: *mut c_char = ptr::null_mut();
        let status = relate_connect_namespace(db, unknown.as_ptr(), &mut conn, &mut error_json);
        assert!(matches!(status, relate_status::RELATE_ERROR));
        assert!(conn.is_null(), "out-param must be NULL after an error");
        let shape = CStr::from_ptr(error_json).to_str().unwrap();
        let shape: serde_json::Value = serde_json::from_str(shape).unwrap();
        crate::relate_free_string(error_json);
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.ends_with("Graph not found: movies")),
            "error shape: {shape}"
        );
        assert_eq!(shape["code"], "Neo.ClientError.Database.DatabaseNotFound");

        relate_database_close(db);
    }
}

/// Namespace names match case-insensitively (ADR-0117 §3).
#[test]
fn ffi_connect_namespace_is_case_insensitive() {
    let upper = CString::new("SYSTEM").unwrap();
    let mut conn: *mut relate_connection = ptr::null_mut();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let db = open_memory_db();
        assert!(matches!(
            relate_connect_namespace(db, upper.as_ptr(), &mut conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// A NULL name selects the default `neo4j` namespace (ADR-0220):
/// `relate_connect_namespace` is the one connect entry point.
#[test]
fn ffi_connect_namespace_null_name_selects_default() {
    // SAFETY: test drives the FFI with handles it owns; the NULL name is
    // the contract under test.
    unsafe {
        let db = open_memory_db();
        let mut conn: *mut relate_connection = ptr::null_mut();
        assert!(matches!(
            relate_connect_namespace(db, ptr::null(), &mut conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(!conn.is_null());
        // The NULL-name connection runs in "neo4j": the statement's resolved
        // namespace metadata reports it.
        let result = run(conn, "MATCH (n) RETURN count(n) AS n");
        let ns = CStr::from_ptr(relate_result_namespace(result))
            .to_str()
            .unwrap();
        assert_eq!(ns, "neo4j");
        relate_destroy_result(result);
        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// `relate_conn_set_namespace` switches the session namespace between
/// transactions; while an explicit transaction is open it fails with the
/// stable CE text (ADR-0117 §2/§3) on the call's error channel.
#[test]
fn ffi_conn_set_namespace_between_transactions() {
    let neo4j = CString::new("neo4j").unwrap();
    let sys = CString::new("system").unwrap();
    let unknown = CString::new("movies").unwrap();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();

        // Between transactions: succeeds.
        assert!(matches!(
            relate_conn_set_namespace(conn, sys.as_ptr(), ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(matches!(
            relate_conn_set_namespace(conn, neo4j.as_ptr(), ptr::null_mut()),
            relate_status::RELATE_OK
        ));

        // Inside an open transaction: fails with the stable text.
        assert!(matches!(
            relate_conn_begin(conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let mut error_json: *mut c_char = ptr::null_mut();
        let status = relate_conn_set_namespace(conn, sys.as_ptr(), &mut error_json);
        assert!(matches!(status, relate_status::RELATE_ERROR));
        let shape = CStr::from_ptr(error_json).to_str().unwrap();
        let shape: serde_json::Value = serde_json::from_str(shape).unwrap();
        crate::relate_free_string(error_json);
        assert!(
            shape["message"].as_str().is_some_and(
                |m| m.ends_with("Cannot change the namespace while a transaction is open.")
            ),
            "error shape: {shape}"
        );
        assert!(matches!(
            relate_conn_rollback(conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));

        // Unknown name: CE DatabaseNotFound classification.
        let mut error_json: *mut c_char = ptr::null_mut();
        let status = relate_conn_set_namespace(conn, unknown.as_ptr(), &mut error_json);
        assert!(matches!(status, relate_status::RELATE_ERROR));
        let shape = CStr::from_ptr(error_json).to_str().unwrap();
        let shape: serde_json::Value = serde_json::from_str(shape).unwrap();
        crate::relate_free_string(error_json);
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.ends_with("Graph not found: movies")),
            "error shape: {shape}"
        );
        assert_eq!(shape["code"], "Neo.ClientError.Database.DatabaseNotFound");

        // A NULL name fails without being dereferenced.
        let status = relate_conn_set_namespace(conn, ptr::null(), ptr::null_mut());
        assert!(matches!(status, relate_status::RELATE_ERROR));
    }
}
