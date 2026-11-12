use crate::abi::relate_status;
use crate::result::*;
use crate::support::{cell, int_of, open_memory, run, take_error_json};
use crate::tabular::*;
use relate::INTERNAL_MESSAGE;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

#[test]
fn ffi_return_one() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();
        let result = run(conn, "RETURN 1 AS n");
        assert_eq!(relate_column_count(result), 1);
        assert_eq!(relate_row_count(result), 1);
        assert_eq!(int_of(cell(result, 0, 0)), 1);
        relate_destroy_result(result);
        relate_disconnect(conn);
        relate_database_close(db);
    }
}

#[test]
fn ffi_begin_commit_isolation() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, w) = open_memory();
        let mut r: *mut relate_connection = ptr::null_mut();
        assert!(matches!(
            relate_connect_namespace(db, ptr::null(), &mut r, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(matches!(
            relate_conn_begin(w, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let created = run(w, "CREATE (n:Person {name: 'Ada'})");
        relate_destroy_result(created);
        let counted = run(r, "MATCH (n:Person) RETURN count(n) AS n");
        assert_eq!(int_of(cell(counted, 0, 0)), 0);
        relate_destroy_result(counted);
        // The commit succeeds and hands its bookmark over through the
        // out-param (ADR-0502 §7) — caller-owned, freed here.
        let mut committed: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_commit(w, &mut committed, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(!committed.is_null());
        crate::relate_free_string(committed);
        let counted = run(r, "MATCH (n:Person) RETURN count(n) AS n");
        assert_eq!(int_of(cell(counted, 0, 0)), 1);
        relate_destroy_result(counted);
        relate_disconnect(w);
        relate_disconnect(r);
        relate_database_close(db);
    }
}

/// A panic inside a tabular operation is caught at the FFI boundary, fills
/// the failing call's error-json out-param with the internal-stage shape,
/// and poisons the handle so every later call on it fails with a defined
/// status instead of touching inconsistent state.
#[test]
fn ffi_tabular_panic_is_caught_poisons_and_fills_the_error_shape() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        // Connection panic: caught, poisoned, reported — with the panic
        // payload genericized, not passed through (OPLANE_REQ-00340359).
        let panicked = guard_conn(conn, |_c| panic!("boom in conn op"));
        let shape = panicked.expect_err("a panic is a failure");
        let parsed: serde_json::Value = serde_json::from_str(shape.as_str()).unwrap();
        assert_eq!(parsed["stage"], "internal");
        assert_eq!(
            parsed["message"], INTERNAL_MESSAGE,
            "the panic reason never rides the shape: {parsed}"
        );

        // A later call on the poisoned connection: defined failure, same
        // channel.
        let q = CString::new("RETURN 1 AS n").unwrap();
        let mut result: *mut relate_query_result = ptr::null_mut();
        let mut error_json: *mut std::os::raw::c_char = ptr::null_mut();
        assert!(matches!(
            relate_query_params(conn, q.as_ptr(), ptr::null(), &mut result, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        let parsed = take_error_json(error_json);
        assert!(
            parsed["message"]
                .as_str()
                .is_some_and(|m| m.contains("poisoned")),
            "the poisoned rejection rides the shape: {parsed}"
        );

        // Database panic: same contract, through the checkpoint call.
        let panicked = guard_db(db, |_d| panic!("boom in db op"));
        assert!(panicked.is_err());
        let mut error_json: *mut std::os::raw::c_char = ptr::null_mut();
        assert!(matches!(
            relate_checkpoint(db, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        let parsed = take_error_json(error_json);
        assert!(
            parsed["message"]
                .as_str()
                .is_some_and(|m| m.contains("poisoned")),
            "the poisoned rejection rides the shape: {parsed}"
        );

        // Closing poisoned handles still frees them without crashing.
        assert!(matches!(relate_disconnect(conn), relate_status::RELATE_OK));
        assert!(matches!(
            relate_database_close(db),
            relate_status::RELATE_OK
        ));
    }
}

/// The query entry point overwrites the caller's result out-param on every
/// call, so a stale pointer never survives into the caller's slot. Seed
/// the out-param with a real result, then run a failing statement: the
/// slot must hold the NEW error result (a statement failure rides the
/// result, ADR-0502 §2), and the seeded result is freed exactly once
/// through its original pointer.
#[test]
fn ffi_query_overwrites_result_out_param_on_statement_failure() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        // A successful query seeds the out-param with a real result.
        let real = run(conn, "RETURN 1 AS n");

        // A failing statement must overwrite the caller's out-param with
        // its own error result, not leave the stale pointer.
        let bad = CString::new("INVALID QUERY !!!").unwrap();
        let mut result: *mut relate_query_result = real;
        assert!(matches!(
            relate_query_params(
                conn,
                bad.as_ptr(),
                ptr::null(),
                &mut result,
                ptr::null_mut()
            ),
            relate_status::RELATE_OK
        ));
        assert!(!result.is_null(), "the statement failure installs a result");
        assert_ne!(result, real, "the stale seeded pointer must be gone");
        assert_eq!(relate_result_is_error(result), 1);
        relate_destroy_result(result);

        // Free the seeded result through its own pointer. The failed call
        // installed a second result, so both are freed exactly once.
        relate_destroy_result(real);

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// A failing `relate_database_open` leaves the database out-param NULL
/// even when the caller pre-seeded it with a non-null value.
#[test]
fn ffi_database_open_error_nulls_out_param() {
    // A non-null, never-dereferenced sentinel makes the assertion meaningful.
    let sentinel = ptr::NonNull::<relate_database>::dangling().as_ptr();
    let mut db: *mut relate_database = sentinel;
    // A remote scheme is rejected by the engine (ADR-0044).
    let bad_uri = CString::new("s3://bucket/db").unwrap();
    // SAFETY: `db` is an out-param the callee only overwrites and `bad_uri`
    // is a valid C string; the sentinel is never dereferenced.
    unsafe {
        let status = relate_database_open(bad_uri.as_ptr(), &mut db, ptr::null_mut());
        assert!(matches!(status, relate_status::RELATE_ERROR));
        assert!(db.is_null(), "out-param must be NULL after a failed open");
    }
}

/// Connecting through a poisoned database fails and leaves the connection
/// out-param NULL, even when pre-seeded with a non-null value.
#[test]
fn ffi_connect_on_poisoned_db_nulls_out_param() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        // Poison the database by unwinding inside a guarded operation.
        assert!(guard_db(db, |_d| panic!("poison db")).is_err());

        let sentinel = ptr::NonNull::<relate_connection>::dangling().as_ptr();
        let mut conn2: *mut relate_connection = sentinel;
        let status = relate_connect_namespace(db, ptr::null(), &mut conn2, ptr::null_mut());
        assert!(matches!(status, relate_status::RELATE_ERROR));
        assert!(
            conn2.is_null(),
            "out-param must be NULL after a failed connect"
        );

        relate_disconnect(conn);
        assert!(matches!(
            relate_database_close(db),
            relate_status::RELATE_OK
        ));
    }
}

/// A query on a poisoned connection fails and leaves the result out-param
/// NULL, even when pre-seeded with a non-null value.
#[test]
fn ffi_query_on_poisoned_conn_nulls_out_param() {
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let (db, conn) = open_memory();

        // Poison the connection by unwinding inside a guarded operation.
        assert!(guard_conn(conn, |_c| panic!("poison conn")).is_err());

        let sentinel = ptr::NonNull::<relate_query_result>::dangling().as_ptr();
        let mut result: *mut relate_query_result = sentinel;
        let q = CString::new("RETURN 1 AS n").unwrap();
        let status =
            relate_query_params(conn, q.as_ptr(), ptr::null(), &mut result, ptr::null_mut());
        assert!(matches!(status, relate_status::RELATE_ERROR));
        assert!(
            result.is_null(),
            "out-param must be NULL after a failed query"
        );

        relate_disconnect(conn);
        relate_database_close(db);
    }
}

/// A read-only open rejects writes through the error channel: the failing
/// statement rides the result (ADR-0502 §2) with the engine's
/// read-only-write classification beside the engine message.
#[test]
fn ffi_read_only_open_rejects_writes() {
    let uri = CString::new(":memory:").unwrap();
    let write = CString::new("CREATE ()").unwrap();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let mut db: *mut relate_database = ptr::null_mut();
        let mut conn: *mut relate_connection = ptr::null_mut();
        assert!(matches!(
            relate_database_open_read_only(uri.as_ptr(), &mut db, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(matches!(
            relate_connect_namespace(db, ptr::null(), &mut conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let result = run(conn, "RETURN 1 AS n");
        relate_destroy_result(result);

        // The write is a statement failure: an error result, never a
        // status error.
        let mut failed: *mut relate_query_result = ptr::null_mut();
        assert!(matches!(
            relate_query_params(
                conn,
                write.as_ptr(),
                ptr::null(),
                &mut failed,
                ptr::null_mut()
            ),
            relate_status::RELATE_OK
        ));
        assert_eq!(relate_result_is_error(failed), 1);
        let shape = CStr::from_ptr(relate_result_error_json(failed))
            .to_str()
            .unwrap();
        let shape: serde_json::Value = serde_json::from_str(shape).unwrap();
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.contains("read-only")),
            "the engine message rides the shape: {shape}"
        );
        relate_destroy_result(failed);

        relate_disconnect(conn);
        relate_database_close(db);

        // The writable control: the same statement is a good result.
        let mut writable: *mut relate_database = ptr::null_mut();
        let mut wconn: *mut relate_connection = ptr::null_mut();
        assert!(matches!(
            relate_database_open(uri.as_ptr(), &mut writable, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(matches!(
            relate_connect_namespace(writable, ptr::null(), &mut wconn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let result = run(wconn, "CREATE ()");
        assert_eq!(relate_result_is_error(result), 0);
        relate_destroy_result(result);
        relate_disconnect(wconn);
        relate_database_close(writable);
    }
}
