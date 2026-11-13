//! The C ABI as a C client sees it (ADR-0502): the `#[no_mangle]`
//! surface lives in the FFI crate's private modules, so a Rust caller
//! declares it `extern "C"` — exactly what every real C client does.
//! The declarations mirror `surfaces/relate-ffi/include/relate.h`;
//! keep them in step with the header (the fuzz harness is a client, not
//! a second definition of the ABI).

#![allow(non_camel_case_types)]

use std::os::raw::c_char;

/// The opaque builder handle (ADR-0502 §4).
#[repr(C)]
pub struct relate_params {
    _private: [u8; 0],
}

/// The opaque database handle.
#[repr(C)]
pub struct relate_database {
    _private: [u8; 0],
}

/// The opaque connection handle.
#[repr(C)]
pub struct relate_connection {
    _private: [u8; 0],
}

/// The opaque result handle (ADR-0502 §1/§3).
#[repr(C)]
pub struct relate_query_result {
    _private: [u8; 0],
}

/// The call status (ADR-0502 §2).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum relate_status {
    RELATE_OK = 0,
    RELATE_ERROR = 1,
}

extern "C" {
    pub fn relate_params_new() -> *mut relate_params;
    pub fn relate_params_free(p: *mut relate_params);
    pub fn relate_free_string(s: *mut c_char);
    pub fn relate_database_open(
        uri: *const c_char,
        out: *mut *mut relate_database,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_database_close(db: *mut relate_database) -> relate_status;
    pub fn relate_connect_namespace(
        db: *mut relate_database,
        name: *const c_char,
        out: *mut *mut relate_connection,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_disconnect(conn: *mut relate_connection) -> relate_status;
    pub fn relate_query_params(
        conn: *mut relate_connection,
        cypher: *const c_char,
        params: *const relate_params,
        out: *mut *mut relate_query_result,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_result_is_error(result: *mut relate_query_result) -> u8;
    pub fn relate_result_error_json(result: *mut relate_query_result) -> *const c_char;
    pub fn relate_destroy_result(result: *mut relate_query_result);
    pub fn relate_params_put_null(
        p: *mut relate_params,
        name: *const c_char,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_bool(
        p: *mut relate_params,
        name: *const c_char,
        v: u8,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_int(
        p: *mut relate_params,
        name: *const c_char,
        v: i64,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_float(
        p: *mut relate_params,
        name: *const c_char,
        v: f64,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_string(
        p: *mut relate_params,
        name: *const c_char,
        v: *const c_char,
        len: usize,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_date(
        p: *mut relate_params,
        name: *const c_char,
        year: i64,
        month: i64,
        day: i64,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_local_time(
        p: *mut relate_params,
        name: *const c_char,
        hour: i64,
        minute: i64,
        second: i64,
        nanosecond: i64,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_duration(
        p: *mut relate_params,
        name: *const c_char,
        months: i64,
        days: i64,
        seconds: i64,
        nanos: i64,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_vector_float(
        p: *mut relate_params,
        name: *const c_char,
        dtype: *const c_char,
        values: *const f64,
        dim: usize,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_list(
        p: *mut relate_params,
        name: *const c_char,
        child: *mut *mut relate_params,
        error_json: *mut *mut c_char,
    ) -> relate_status;
    pub fn relate_params_put_map(
        p: *mut relate_params,
        name: *const c_char,
        child: *mut *mut relate_params,
        error_json: *mut *mut c_char,
    ) -> relate_status;
}

/// Take the caller-owned error channel back: free the string if the call
/// filled one, and reset the out-param — the one reclaim policy every
/// site in this harness shares.
pub fn reclaim(err: &mut *mut c_char) {
    if !err.is_null() {
        // SAFETY: a non-null error string was handed out by the ABI and is
        // freed exactly once here before the out-param is reset.
        unsafe { relate_free_string(*err) };
        *err = std::ptr::null_mut();
    }
}
