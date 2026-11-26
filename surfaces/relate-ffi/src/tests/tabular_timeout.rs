use crate::abi::relate_status;
use crate::relate_free_string;
use crate::result::*;
use crate::support::{cell, open_memory_db, run, str_of};
use crate::tabular::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

/// ADR-0692: `relate_conn_set_statement_timeout` bounds the connection's
/// statements. A statement past its budget is a statement failure riding the
/// result (ADR-0502 §2) with the client-configuration timeout code, the
/// connection stays usable, and `0` clears the bound.
#[test]
fn ffi_statement_timeout_cancels_and_clears() {
    let name = CString::new("neo4j").unwrap();
    let mut conn: *mut relate_connection = ptr::null_mut();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let db = open_memory_db();
        assert!(matches!(
            relate_connect_namespace(db, name.as_ptr(), &mut conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        assert!(matches!(
            relate_conn_set_statement_timeout(conn, 100, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let long = CString::new("UNWIND range(1, 2000000000) AS x RETURN count(x) AS n").unwrap();
        let mut failed: *mut relate_query_result = ptr::null_mut();
        assert!(matches!(
            relate_query_params(
                conn,
                long.as_ptr(),
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
        assert_eq!(
            shape["code"],
            "Neo.ClientError.Transaction.TransactionTimedOutClientConfiguration"
        );
        assert_eq!(shape["message"], "query timed out after 100ms");
        relate_destroy_result(failed);

        // The connection survives, and a cleared bound lets a statement run.
        assert!(matches!(
            relate_conn_set_statement_timeout(conn, 0, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
        let result = run(conn, "RETURN 'ok' AS s");
        assert_eq!(str_of(cell(result, 0, 0)), "ok");
        relate_destroy_result(result);

        // A null connection is a boundary error, not a crash.
        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_set_statement_timeout(ptr::null_mut(), 1, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        relate_free_string(error_json);
        relate_disconnect(conn);
        relate_database_close(db);
    }
}
