//! The C ABI's per-transaction metadata input (ADR-0610):
//! `relate_conn_set_tx_metadata`, the embedded channel's C spelling —
//! the params builder as the metadata map, the later-keys-win merge, the
//! read-back through `CALL tx.getMetaData`, the boundary's clear and the
//! rollback's discard, and the entry point's own refusals.
//!
//! The gated side needs the process-wide `value-provenance` opt-in, so
//! every test here calls this binary's union fixture
//! (`support::preview_union_for_this_binary`): one test binary — the C
//! ABI's modules are private, so every FFI suite is a `#[path]` module
//! of it — one first-call-wins set, shared with the namespaces suite's
//! `ontology` opt-in; whichever test's `Once` lands, every gated open
//! sees both features. The closed side (the authored preview error
//! while the opt-in is off) cannot live in this binary — no process
//! flips its own gate — and is pinned in the engine's preview-gate
//! suite, beside the `Connection::set_tx_metadata` contract the entry
//! point forwards to.

use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

use crate::abi::relate_status;
use crate::params::*;
use crate::relate_free_string;
use crate::result::*;
use crate::support::{cell, int_of, run, str_of, take_error_json, Session};
use crate::tabular::*;
use crate::value::relate_value_kind::*;
use crate::value::*;

/// A Rust string as an owned C string for one call.
fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// Assert a builder or status call succeeded.
fn ok(status: relate_status) {
    assert!(matches!(status, relate_status::RELATE_OK));
}

/// The live metadata map's entry count, read back through one
/// `CALL tx.getMetaData()` row.
///
/// # Safety
/// `conn` must be a live connection; the result the row rides is
/// destroyed before this helper returns.
unsafe fn live_metadata_len(conn: *mut relate_connection) -> usize {
    // SAFETY: test drives the FFI with a handle it owns; `run` asserts a
    // good result and the result dies before the return.
    unsafe {
        let result = run(conn, "CALL tx.getMetaData() YIELD metadata RETURN metadata");
        let map = cell(result, 0, 0);
        assert_eq!(relate_value_kind(map), RELATE_V_MAP as i32);
        let len = relate_value_map_len(map);
        assert!(len >= 0, "an in-kind map length");
        relate_destroy_result(result);
        len as usize
    }
}

/// One metadata entry read back through `tx.getMetaData`, with `read`
/// deciding the component (`str_of` / `int_of` — the support hub's
/// in-kind readers).
///
/// # Safety
/// `conn` must be a live connection and the key present in the live map.
unsafe fn metadata_entry<T>(
    conn: *mut relate_connection,
    key: &str,
    read: unsafe fn(*const relate_value) -> T,
) -> T {
    // SAFETY: as `live_metadata_len`; the entry handle is result-owned
    // and read before the result dies.
    unsafe {
        let result = run(conn, "CALL tx.getMetaData() YIELD metadata RETURN metadata");
        let map = cell(result, 0, 0);
        let value = relate_value_map_get(map, c(key).as_ptr());
        assert!(!value.is_null(), "the metadata key '{key}' is present");
        let out = read(value);
        relate_destroy_result(result);
        out
    }
}

/// A root builder holding `by: 'app'` and `run: 7` — the sample map.
fn by_app_run_7() -> *mut relate_params {
    let params = relate_params_new();
    // SAFETY: test owns the builder and drives its putters with strings
    // it owns; the error_json opt-out is covered by the ok assert.
    unsafe {
        ok(relate_params_put_string(
            params,
            c("by").as_ptr(),
            c("app").as_ptr(),
            3,
            ptr::null_mut(),
        ));
        ok(relate_params_put_int(
            params,
            c("run").as_ptr(),
            7,
            ptr::null_mut(),
        ));
    }
    params
}

/// The full embedded-channel round trip: the builder lowers into the
/// connection's per-transaction map, the map is live inside the next
/// transaction (`tx.getMetaData` reads it back), the transaction's write
/// carries it across its commit boundary, and the boundary clears it —
/// the next read sees an empty map.
#[test]
fn ffi_set_tx_metadata_rides_the_next_transaction() {
    crate::support::preview_union_for_this_binary();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let params = by_app_run_7();
        ok(relate_conn_set_tx_metadata(conn, params, ptr::null_mut()));

        ok(relate_conn_begin(conn, ptr::null_mut()));
        // Read it back: the live map the seed lowered to, inside the
        // transaction that will carry it.
        assert_eq!(live_metadata_len(conn), 2);
        assert_eq!(metadata_entry(conn, "by", str_of), "app");
        assert_eq!(metadata_entry(conn, "run", int_of), 7);
        let write = run(conn, "CREATE (n:Person {name: 'Ada'})");
        relate_destroy_result(write);
        let mut bookmark: *mut c_char = ptr::null_mut();
        ok(relate_conn_commit(conn, &mut bookmark, ptr::null_mut()));
        relate_free_string(bookmark);

        // Cleared after the boundary: nothing rides a later transaction.
        assert_eq!(live_metadata_len(conn), 0);
        relate_params_free(params);
    }
}

/// The merge is the channel's contract: two seeds merge in call order —
/// later keys win — a rollback discards the merged map, and the builder
/// is read, not consumed: the same root still binds a statement's
/// parameters afterwards.
#[test]
fn ffi_set_tx_metadata_merges_later_keys_and_keeps_the_builder_usable() {
    crate::support::preview_union_for_this_binary();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let first = by_app_run_7();
        let second = relate_params_new();
        ok(relate_params_put_string(
            second,
            c("by").as_ptr(),
            c("job").as_ptr(),
            3,
            ptr::null_mut(),
        ));

        ok(relate_conn_set_tx_metadata(conn, first, ptr::null_mut()));
        ok(relate_conn_set_tx_metadata(conn, second, ptr::null_mut()));
        ok(relate_conn_begin(conn, ptr::null_mut()));
        // Later keys win: `by` is the second seed's, `run` the first's.
        assert_eq!(live_metadata_len(conn), 2);
        assert_eq!(metadata_entry(conn, "by", str_of), "job");
        ok(relate_conn_rollback(conn, ptr::null_mut()));
        // The rollback discards the merged map.
        assert_eq!(live_metadata_len(conn), 0);

        // The second builder still binds statement parameters: it was
        // read, not modified or consumed.
        let bound = crate::support::run_query(conn, "RETURN $by AS by", second);
        assert_eq!(str_of(cell(bound, 0, 0)), "job");
        relate_destroy_result(bound);
        relate_params_free(first);
        relate_params_free(second);
    }
}

/// The entry point's own refusals: a list builder (metadata is a map)
/// and a NULL connection, both through the error channel; a NULL builder
/// is an empty map — a no-op seed while the opt-in is on.
#[test]
fn ffi_set_tx_metadata_refusals() {
    crate::support::preview_union_for_this_binary();
    // SAFETY: test drives the FFI with handles, strings and out-params it
    // owns.
    unsafe {
        let session = Session::open();
        let (_, conn) = session.handles();
        let root = relate_params_new();
        let mut list: *mut relate_params = ptr::null_mut();
        ok(relate_params_put_list(
            root,
            c("xs").as_ptr(),
            &mut list,
            ptr::null_mut(),
        ));

        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_set_tx_metadata(conn, list, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        let shape = take_error_json(error_json);
        assert!(
            shape["message"]
                .as_str()
                .is_some_and(|m| m.contains("map builder")),
            "list builder refusal: {shape}"
        );

        // A NULL connection is the guards' own refusal, before any
        // builder is read.
        let mut error_json: *mut c_char = ptr::null_mut();
        assert!(matches!(
            relate_conn_set_tx_metadata(ptr::null_mut(), root, &mut error_json),
            relate_status::RELATE_ERROR
        ));
        let shape = take_error_json(error_json);
        assert_eq!(shape["message"], "null connection");

        // NULL metadata is an empty map: a no-op seed while the opt-in
        // is on, leaving nothing to read back.
        ok(relate_conn_set_tx_metadata(
            conn,
            ptr::null(),
            ptr::null_mut(),
        ));
        assert_eq!(live_metadata_len(conn), 0);
        relate_params_free(root);
    }
}
