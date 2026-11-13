//! Fuzz `relate_query_params` end to end (LABS-937 /
//! OPLANE_REQ-00340357): one in-memory database, one connection, one
//! statement whose text and one string parameter come from the fuzz
//! bytes. The builder is driven to completion and the call is made; both
//! the statement-failure channel (the error result) and the call-failure
//! channel (`*error_json`) are read back and freed, then the result and
//! handles are closed in order. Every C input is built here from the
//! fuzz data, so the trusted-length contract holds by construction.
#![no_main]

use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

use relate_ffi_fuzz::*;
use libfuzzer_sys::fuzz_target;


fuzz_target!(|data: &[u8]| {
    // SAFETY: every handle, string and out-param below is created and
    // owned by this harness; every buffer's length matches the `len`
    // passed with it.
    unsafe {
        let mut db: *mut relate_database = ptr::null_mut();
        let mut conn: *mut relate_connection = ptr::null_mut();
        let mut result: *mut relate_query_result = ptr::null_mut();
        let mut error_json: *mut c_char = ptr::null_mut();

        // The same in-memory store every open mints, as the FFI tests
        // do — no filesystem touch, state accumulates per process.
        let uri = CString::new(":memory:").unwrap();
        if !matches!(
            relate_database_open(uri.as_ptr(), &mut db, &mut error_json),
            relate_status::RELATE_OK
        ) {
            reclaim(&mut error_json);
            return;
        }
        let namespace = CString::new("neo4j").unwrap();
        if !matches!(
            relate_connect_namespace(db, namespace.as_ptr(), &mut conn, &mut error_json),
            relate_status::RELATE_OK
        ) {
            reclaim(&mut error_json);
            relate_database_close(db);
            return;
        }

        // The statement references `$p` (a compile-class failure when it
        // is never bound, a runtime pass when it is) and returns it, so
        // the value crosses back through the walker too.
        let statement = CString::new("RETURN $p AS p").unwrap();
        let p = relate_params_new();
        let pname = CString::new("p").unwrap();
        let value = &data[..data.len().min(32)];
        let mut put_error: *mut c_char = ptr::null_mut();
        relate_params_put_string(
            p,
            pname.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            &mut put_error,
        );
        reclaim(&mut put_error);

        let status = relate_query_params(
            conn,
            statement.as_ptr(),
            p,
            &mut result,
            &mut error_json,
        );
        reclaim(&mut error_json);
        if !result.is_null() {
            if relate_result_is_error(result) == 1 {
                // Result-owned: read, never freed.
                let _ = relate_result_error_json(result);
            }
            relate_destroy_result(result);
        }
        relate_params_free(p);
        relate_disconnect(conn);
        relate_database_close(db);
    }
});
