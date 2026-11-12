//! The shared test-driver hub: the connection openers, the query runners
//! and the value readers every FFI suite drives the ABI through. One home
//! for them, so a suite imports `crate::support`, never a sibling suite.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use crate::abi::relate_status;
use crate::relate_free_string;
use crate::result::*;
use crate::tabular::*;
use crate::value::*;

/// Open a `:memory:` database with no connection, asserting success.
///
/// # Safety
/// The returned handle is owned by the caller and must be freed with
/// `relate_database_close`.
pub(crate) unsafe fn open_memory_db() -> *mut relate_database {
    let uri = CString::new(":memory:").unwrap();
    let mut db: *mut relate_database = ptr::null_mut();
    // SAFETY: test drives the FFI with out-params it owns; the NULL
    // error_json out-param opts out of the detail the success assert
    // already covers.
    unsafe {
        assert!(matches!(
            relate_database_open(uri.as_ptr(), &mut db, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
    }
    db
}

/// Open a `:memory:` database with one connection (the NULL-name default
/// `neo4j` namespace), asserting success.
///
/// # Safety
/// The returned handles are owned by the caller and must be freed with
/// `relate_disconnect` and `relate_database_close`.
pub(crate) unsafe fn open_memory() -> (*mut relate_database, *mut relate_connection) {
    let db = unsafe { open_memory_db() };
    let mut conn: *mut relate_connection = ptr::null_mut();
    // SAFETY: test drives the FFI with an out-param it owns.
    unsafe {
        assert!(matches!(
            // The one connect entry point (ADR-0220): NULL name = "neo4j".
            relate_connect_namespace(db, ptr::null(), &mut conn, ptr::null_mut()),
            relate_status::RELATE_OK
        ));
    }
    (db, conn)
}

/// Run `cypher` with `params` through the query entry point, asserting
/// the transport succeeded AND the statement produced a good result (the
/// caller destroys it) — the one run helper every suite shares
/// (`run` is its no-params spelling, `params_builder`'s runs delegate
/// here).
///
/// # Safety
/// `conn` must be a live connection from `open_memory`; `params` null or
/// a live builder.
pub(crate) unsafe fn run_query(
    conn: *mut relate_connection,
    cypher: &str,
    params: *const crate::params::relate_params,
) -> *mut relate_query_result {
    let q = CString::new(cypher).unwrap();
    let mut result: *mut relate_query_result = ptr::null_mut();
    let mut error_json: *mut c_char = ptr::null_mut();
    // SAFETY: test drives the FFI with a handle, strings and out-params it
    // owns.
    unsafe {
        assert!(
            matches!(
                relate_query_params(conn, q.as_ptr(), params, &mut result, &mut error_json),
                relate_status::RELATE_OK
            ),
            "query failed: {}",
            take_error_json(error_json)
        );
    }
    assert!(!result.is_null());
    // The status is only the transport verdict: a failing statement rides
    // the result, so a good result is asserted here too.
    // SAFETY: test owns the result handle and reads its result-owned
    // strings before destroying it.
    unsafe {
        assert_eq!(
            relate_result_is_error(result),
            0,
            "statement failed: {:?}",
            CStr::from_ptr(relate_result_error_json(result))
        );
    }
    result
}

/// `run_query` with no parameters.
///
/// # Safety
/// `conn` must be a live connection from `open_memory`.
pub(crate) unsafe fn run(conn: *mut relate_connection, cypher: &str) -> *mut relate_query_result {
    unsafe { run_query(conn, cypher, ptr::null()) }
}

/// The root handle of cell (`row`, `col`), asserted present.
///
/// # Safety
/// `result` must be a live good result.
pub(crate) unsafe fn cell(
    result: *mut relate_query_result,
    row: i64,
    col: i64,
) -> *const relate_value {
    let v = unsafe { relate_value_at(result, row, col) };
    assert!(!v.is_null(), "cell ({row}, {col}) is in range");
    v
}

/// An INT handle's value, asserted in-kind.
///
/// # Safety
/// `v` must be a live handle.
pub(crate) unsafe fn int_of(v: *const relate_value) -> i64 {
    unsafe { component(relate_value_int, v) }
}

/// A STRING (or RELATIONSHIP type) handle's text, asserted in-kind.
///
/// # Safety
/// `v` must be a live handle.
pub(crate) unsafe fn str_of(v: *const relate_value) -> String {
    unsafe { text(relate_value_string(v, ptr::null_mut())) }
}

/// A result-owned C string as a Rust string; NULL panics.
///
/// # Safety
/// `p` must be a non-NULL, live result-owned NUL-terminated string (NULL panics).
pub(crate) unsafe fn text(p: *const c_char) -> String {
    assert!(!p.is_null(), "an in-kind string accessor");
    unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string()
}

/// One i64 temporal component read through `read`, asserted in-kind.
///
/// # Safety
/// `v` must be a live handle.
pub(crate) unsafe fn component(
    read: unsafe extern "C" fn(*const relate_value, *mut i64) -> relate_status,
    v: *const relate_value,
) -> i64 {
    let mut out = 0;
    assert!(matches!(
        unsafe { read(v, &mut out) },
        relate_status::RELATE_OK
    ));
    out
}

/// Take a caller-owned `error_json` out-param string, parse it as the
/// ADR-0502 §2 shape, and free it — the one copy of the read the error
/// channel's tests share.
///
/// # Safety
/// `error_json` must be non-null and hold a string the FFI filled in.
pub(crate) unsafe fn take_error_json(error_json: *mut c_char) -> serde_json::Value {
    // SAFETY: the caller guarantees a non-null FFI-owned NUL-terminated
    // string; `relate_free_string` takes it back exactly once.
    unsafe {
        let text = text(error_json);
        relate_free_string(error_json);
        serde_json::from_str(&text).expect("the error channel carries JSON")
    }
}
