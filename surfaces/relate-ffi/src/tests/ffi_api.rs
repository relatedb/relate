use super::*;
use crate::abi::relate_status;
use crate::result::{
    relate_column_count, relate_destroy_result, relate_result_elapsed_ms, relate_result_error_json,
    relate_result_is_error, relate_result_namespace, relate_result_notifications,
    relate_result_plan_json, relate_result_query_type, relate_result_stats, relate_row_count,
    relate_value_at,
};
use crate::support::{cell, int_of, open_memory, run, take_error_json};
use crate::tabular::{
    guard_conn, relate_conn_begin, relate_conn_commit, relate_connect_namespace,
    relate_database_close, relate_database_open, relate_disconnect, relate_query_params,
};
use relate::{error_info, Error};
use std::ffi::{CStr, CString};
use std::ptr;

/// `relate_abi_version` reports the typed ABI (ADR-0502 §8): 2.
#[test]
fn ffi_abi_version_is_two() {
    assert_eq!(relate_abi_version(), 2);
}

/// A statement failure rides the result (ADR-0502 §2): the call returns
/// RELATE_OK with an error result — `is_error` is 1 and the error JSON
/// carries the engine classification (`error_info`) beside the engine
/// message. A syntax error is the compile-stage code and is not retryable;
/// every metadata accessor reports absence on an error result.
#[test]
fn ffi_statement_failure_rides_the_result_with_the_engine_classification() {
    let bad = CString::new("INVALID QUERY !!!").unwrap();
    let mut result: *mut crate::result::relate_query_result = ptr::null_mut();
    let mut error_json: *mut c_char = ptr::null_mut();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();
        assert!(matches!(
            relate_query_params(
                conn,
                bad.as_ptr(),
                ptr::null(),
                &mut result,
                &mut error_json
            ),
            relate_status::RELATE_OK
        ));
        // The statement failure is a result, never a status error: the
        // out-param stays untouched.
        assert!(error_json.is_null());
        assert!(!result.is_null());
        assert_eq!(relate_result_is_error(result), 1);
        let shape = CStr::from_ptr(relate_result_error_json(result))
            .to_str()
            .unwrap();
        let shape: serde_json::Value = serde_json::from_str(shape).unwrap();
        // The classification is the engine's own for the syntax class —
        // read off `error_info`, not re-derived here.
        let expected = error_info(&Error::Syntax("INVALID QUERY !!!".to_string()));
        assert_eq!(shape["code"], expected.neo4j_code);
        assert_eq!(shape["gql_status"], expected.gql_status);
        assert_eq!(shape["stage"], "compile");
        assert_eq!(shape["retryable"], false);
        assert!(
            shape["message"].as_str().is_some_and(|m| !m.is_empty()),
            "the engine message rides the shape: {shape}"
        );

        // The error result reports absence everywhere else: no columns, no
        // rows, no metadata, no timings.
        assert_eq!(relate_column_count(result), 0);
        assert_eq!(relate_row_count(result), 0);
        assert!(relate_result_query_type(result).is_null());
        assert!(relate_result_namespace(result).is_null());
        assert!(relate_result_stats(result).is_null());
        assert!(relate_result_notifications(result).is_null());
        assert!(relate_result_plan_json(result).is_null());
        assert!(relate_value_at(result, 0, 0).is_null());
        assert_eq!(relate_result_elapsed_ms(result), 0.0);

        relate_destroy_result(result);
        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// A good result is not an error result: `is_error` is 0 and the error
/// JSON accessor is NULL.
#[test]
fn ffi_good_result_is_not_an_error_result() {
    // SAFETY: test drives the FFI with handles and out-params it owns.
    unsafe {
        let (db, conn) = open_memory();
        let result = run(conn, "RETURN 1 AS n");
        assert_eq!(relate_result_is_error(result), 0);
        assert!(relate_result_error_json(result).is_null());
        assert_eq!(int_of(cell(result, 0, 0)), 1);
        relate_destroy_result(result);
        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// The retryable write-write conflict (ADR-0439) crosses the commit call's
/// error channel with the engine's classification: the
/// `Neo.TransientError.Transaction.Outdated` code and `retryable: true` —
/// the two facts a driver's `execute_write` retries on (ADR-0502 §2).
#[test]
fn ffi_write_conflict_commit_carries_the_retryable_transient_code() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, winner) = open_memory();
        let setup = run(
            winner,
            "CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE",
        );
        relate_destroy_result(setup);
        let mut loser: *mut crate::tabular::relate_connection = ptr::null_mut();
        assert!(matches!(
            relate_connect_namespace(db, ptr::null(), &mut loser, ptr::null_mut()),
            relate_status::RELATE_OK
        ));

        assert!(matches!(
            relate_conn_begin(winner, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(matches!(
            relate_conn_begin(loser, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        for conn in [winner, loser] {
            let created = run(conn, "CREATE (:P {id: 77})");
            relate_destroy_result(created);
        }
        // The winner's commit succeeds and hands its bookmark over through
        // the out-param (ADR-0502 §7) — caller-owned, freed here.
        let mut winner_bookmark: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(winner, &mut winner_bookmark, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(!winner_bookmark.is_null(), "the winning transaction wrote");
        relate_free_string(winner_bookmark);

        let mut bookmark: *mut c_char = ptr::null_mut();
        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(loser, &mut bookmark, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        assert!(bookmark.is_null(), "a failed commit hands over no bookmark");
        assert!(!error_json.is_null(), "the failure fills the out-param");
        let shape = take_error_json(error_json);
        let expected = error_info(&Error::Conflict);
        assert_eq!(shape["code"], expected.neo4j_code);
        assert_eq!(shape["code"], "Neo.TransientError.Transaction.Outdated");
        assert_eq!(shape["gql_status"], expected.gql_status);
        assert_eq!(shape["stage"], "runtime");
        assert_eq!(shape["retryable"], true);

        relate_disconnect(loser);
        relate_disconnect(winner);
        relate_database_close(db);
    }
}

/// A failed `relate_database_open` fills the trailing error-json
/// out-param with the same ADR-0502 §2 shape (here: a URI scheme the
/// engine rejects, ADR-0044), and a NULL out-param is a tolerated opt-out.
#[test]
fn ffi_database_open_failure_fills_the_error_json_out_param() {
    let bad_uri = CString::new("s3://bucket/db").unwrap();
    let mut db: *mut crate::tabular::relate_database = ptr::null_mut();
    // SAFETY: test drives the FFI with strings and out-params it owns.
    unsafe {
        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_database_open(bad_uri.as_ptr(), &mut db, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        assert!(db.is_null(), "out-param must be NULL after a failed open");
        assert!(!error_json.is_null(), "the failure fills the out-param");
        let shape = take_error_json(error_json);
        // The one shape, on this channel too: the five keys, the engine
        // classification beside the engine message.
        assert_eq!(shape.as_object().unwrap().len(), 5);
        assert!(
            shape["code"].as_str().is_some_and(|c| !c.is_empty()),
            "the Neo4j status code crosses: {shape}"
        );
        assert!(
            shape["gql_status"].as_str().is_some_and(|g| !g.is_empty()),
            "the GQLSTATUS crosses: {shape}"
        );
        assert!(
            shape["stage"].as_str().is_some_and(|s| !s.is_empty()),
            "the stage crosses: {shape}"
        );
        assert!(shape["retryable"].is_boolean());
        assert!(
            shape["message"].as_str().is_some_and(|m| !m.is_empty()),
            "the engine message crosses: {shape}"
        );

        // A NULL out-param opts out of the detail; the status still
        // reports the failure.
        let mut db2: *mut crate::tabular::relate_database = ptr::null_mut();
        assert!(matches!(
            relate_database_open(bad_uri.as_ptr(), &mut db2, ptr::null_mut()),
            relate_status::RELATE_ERROR
        ));
        assert!(db2.is_null());
    }
}

/// A connection-level failure of the query entry point is a status error,
/// not an error result: the result out-param stays NULL and the error
/// shape rides the trailing out-param (a poisoned connection here — the
/// internal stage, per ADR-0502 §2's boundary rule).
#[test]
fn ffi_query_connection_failure_is_a_status_error_not_an_error_result() {
    let q = CString::new("RETURN 1 AS n").unwrap();
    let mut result: *mut crate::result::relate_query_result = ptr::null_mut();
    let mut error_json: *mut c_char = ptr::null_mut();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();
        // Poison the connection by unwinding inside a guarded operation.
        assert!(guard_conn(conn, |_c| panic!("poison conn")).is_err());

        assert!(matches!(
            relate_query_params(conn, q.as_ptr(), ptr::null(), &mut result, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        assert!(result.is_null(), "a status error fabricates no result");
        let shape = take_error_json(error_json);
        assert_eq!(shape["stage"], "internal");
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.contains("poisoned")),
            "the boundary message rides the shape: {shape}"
        );

        // A null result out-pointer is a boundary failure too, reported the
        // same way.
        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_query_params(
                conn,
                q.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                &mut error_json
            ),
            relate_status::RELATE_ERROR
        ));
        let shape = take_error_json(error_json);
        assert_eq!(shape["stage"], "internal");

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

// LABS-680 (ADR-0477) safe mode is an engine-level opt-in; the FFI exposes no
// safe-mode entry point, so the engine default is covered by
// `labs680_safe_mode` in the engine crate rather than asserted here.
