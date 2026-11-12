#![deny(unsafe_op_in_unsafe_fn)]

//! Relate C FFI — the typed ABI v2 (ADR-0502).
//!
//! Provides a C-compatible API for embedding Relate in the JS, Java,
//! Go and Swift clients via their FFI mechanisms.
//!
//! # Error channel (ADR-0502 §2)
//!
//! One error shape travels with the call that failed — there is no
//! thread-local error slot, so the channel is async-safe by construction
//! (a call run on a worker thread reports its own error). The shape is a
//! JSON object rendered from the engine's one classification
//! ([`relate::error_info`]) beside the failure's message:
//!
//! ```json
//! {"code":"<Neo4j status code>","gql_status":"<GQLSTATUS>",
//!  "stage":"compile|runtime|internal","retryable":<bool>,
//!  "message":"<message>"}
//! ```
//!
//! `message` is the engine's own text for the caller-facing classes; the
//! internal class — engine-internal, IO, storage and locked failures, and
//! caught panics — is genericized to the fixed `internal error` message
//! (ADR-0260, OPLANE_REQ-00340359): paths and engine internals never ride
//! the client's shape. The embedding host's stderr is the host's channel,
//! so the redacted original is only logged when the host opts in with
//! `RELATE_FFI_LOG=1`.
//!
//! Statement failures ride the result (`relate_query_params` returns
//! `RELATE_OK` with an error result); every other fallible status call
//! keeps returning `RELATE_ERROR` and fills a trailing
//! `char **error_json` out-param (NULL opts out), caller-owned and freed
//! with `relate_free_string`. The channel — the renderers and the
//! out-param fill — is one module, [`abi`]: the piece `tabular`, `params`,
//! `result` and `value` are all clients of.
//!
//! # Panic safety
//!
//! Every `#[no_mangle]` entry point wraps its work in
//! [`std::panic::catch_unwind`] so that a Rust panic never unwinds across
//! the FFI boundary (which would be undefined behavior). This covers
//! argument decoding too: the entry points perform their `CStr`/UTF-8
//! decode and parameter lowering *inside* the guarded
//! closure, so the only work outside the boundary is the infallible
//! null-pointer check. If a call panics, the handle is *poisoned*: it is
//! not freed, but every subsequent call on it returns a defined failure
//! (a `RELATE_ERROR` status with the internal-stage error shape, or a
//! `0`/null result accessor) instead of touching the possibly-inconsistent
//! engine state. Recover by closing the handle and opening a fresh one;
//! close/destroy still free a poisoned handle.
//!
//! # Threading contract
//!
//! A `relate_database` may be shared across threads; a
//! `relate_connection` is **not** safe for concurrent use — all calls
//! that share a connection must be serialized by the caller. Distinct
//! connections may be used from distinct threads.
//!
//! # Values
//!
//! Result cells cross as typed value handles (ADR-0502 §1, the `value`
//! module): one exhaustive kind per engine value shape, component
//! accessors by kind, every handle owned by its result. Parameters cross
//! the other way through the typed params builder (ADR-0502 §4, the
//! `params` module), its mirror — no JSON on either path. The result
//! metadata documents — mutation counters, notifications, plan — are
//! serialized through the canonical [`relate_model::wire`] module
//! shared with the other surfaces.

use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

mod abi;
mod params;
mod result;
mod tabular;
mod value;

/// The ABI major version (ADR-0502 §8): 2 for the typed ABI.
#[no_mangle]
pub extern "C" fn relate_abi_version() -> i32 {
    2
}

/// Free a caller-owned string returned by this library — today the
/// `error_json` out-params and `relate_conn_commit`'s `bookmark_out`
/// out-param (ADR-0502 §7). Safe to call with NULL.
///
/// # Safety
/// `s` must be a valid pointer from an `error_json` out-param, or null.
#[no_mangle]
pub unsafe extern "C" fn relate_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: `s` is non-null (checked above) and uniquely owned from an
    // error_json out-param.
    let owned = unsafe { CString::from_raw(s) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(owned)));
}

#[cfg(test)]
#[path = "tests/support.rs"]
mod support;

#[cfg(test)]
#[path = "tests/error_shape.rs"]
mod error_shape_tests;

#[cfg(test)]
#[path = "tests/ffi_api.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/tabular.rs"]
mod tabular_tests;

#[cfg(test)]
#[path = "tests/tabular_namespaces.rs"]
mod tabular_namespace_tests;

#[cfg(test)]
#[path = "tests/tabular_result_metadata.rs"]
mod tabular_result_metadata_tests;

#[cfg(test)]
#[path = "tests/tabular_bookmarks.rs"]
mod tabular_bookmarks_tests;

#[cfg(test)]
#[path = "tests/value_walker.rs"]
mod value_walker_tests;

#[cfg(test)]
#[path = "tests/params_builder.rs"]
mod params_builder_tests;

#[cfg(test)]
#[path = "tests/fuzz_decls.rs"]
mod fuzz_decls_tests;
