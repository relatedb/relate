//! Tabular / multi-connection C ABI (language clients) — the one client API
//! of ABI v2 (ADR-0502 §3).
//!
//! This surface carries the crate-wide contracts:
//!
//! - **One error channel (ADR-0502 §2).** A statement failure rides the
//!   result — [`relate_query_params`] returns `RELATE_OK` with an
//!   error result ([`crate::result::pack_error_result`]); every other failure fills the
//!   failing call's trailing `char **error_json` out-param with the one
//!   error shape ([`crate::abi::error_shape_json`]), caller-owned and freed
//!   with [`crate::relate_free_string`]. There is no thread-local
//!   error slot.
//! - **Panic containment.** Every entry point runs its work under
//!   [`std::panic::catch_unwind`]; a caught panic poisons the database or
//!   connection handle (later calls return `RELATE_ERROR` / `0` / NULL
//!   with the internal-stage error shape). Close/destroy entry points
//!   still free poisoned handles.
//! - **One value path (ADR-0502 §1).** Result cells cross as typed value
//!   handles ([`crate::value`]): each cell converts once with
//!   [`relate::value_to_cypher`] into a result-owned root handle, read
//!   through [`crate::result::relate_value_at`] and the walker's
//!   accessors. Parameters arrive as the typed params builder
//!   ([`crate::params`], ADR-0502 §4), lowered without a JSON intermediate.
//!
//! The result handle, its packers and its accessors live in their own
//! [`crate::result`] module; this module owns the database and connection
//! handles and their lifecycle entry points.

#![allow(non_camel_case_types)]

use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use relate::{Connection, Database, OpenMode, OpenOptions};

use crate::abi::{
    boundary_error_json, engine_error_json, panic_message, read_cstr, relate_status, report,
    RenderedError,
};
use crate::params::{self, relate_params};
use crate::result::{pack_error_result, pack_result, relate_query_result};

fn ok() -> relate_status {
    relate_status::RELATE_OK
}

/// Error message returned by every operation on a poisoned handle — the
/// guards' own refusal, beside the poison flags that raise it.
const POISONED_MESSAGE: &str = "handle poisoned by a previous panic; free it and open a new one";

pub struct relate_database {
    db: Database,
    /// Set once a panic has been caught while operating on this handle; a
    /// poisoned database rejects every further operation. Atomic because a
    /// database may be shared across threads (connections may not).
    poisoned: AtomicBool,
}

pub struct relate_connection {
    conn: Mutex<Connection>,
    /// Set once a panic has been caught while operating on this connection.
    poisoned: AtomicBool,
}

/// What the one handle guard needs from a handle type: its poison flag and
/// the refusal text for a null pointer.
trait Guarded {
    const NULL_MESSAGE: &'static str;
    fn poisoned(&self) -> &AtomicBool;
}

impl Guarded for relate_database {
    const NULL_MESSAGE: &'static str = "null database";
    fn poisoned(&self) -> &AtomicBool {
        &self.poisoned
    }
}

impl Guarded for relate_connection {
    const NULL_MESSAGE: &'static str = "null connection";
    fn poisoned(&self) -> &AtomicBool {
        &self.poisoned
    }
}

/// The one null/poison gate over both handle types: reject a null or
/// poisoned handle, then run `op` on it under the shared containment
/// ([`contain`]), which poisons the handle on unwind.
///
/// # Safety
/// `ptr` must be null or a valid pointer previously returned by the
/// handle's constructor.
unsafe fn guard<H: Guarded>(
    ptr: *mut H,
    op: impl FnOnce(&H) -> Result<(), RenderedError>,
) -> Result<(), RenderedError> {
    if ptr.is_null() {
        return Err(boundary_error_json(H::NULL_MESSAGE));
    }
    // SAFETY: `ptr` is non-null and valid per this function's contract.
    let handle = unsafe { &*ptr };
    if handle.poisoned().load(Ordering::SeqCst) {
        return Err(boundary_error_json(POISONED_MESSAGE));
    }
    contain(handle.poisoned(), || op(handle))
}

/// Run a database operation under `catch_unwind`, rejecting null or poisoned
/// handles and poisoning the handle if `op` unwinds. The `Err` half is the
/// rendered ADR-0502 §2 error shape ([`RenderedError`]), ready for the
/// caller's `error_json` out-param.
///
/// # Safety
/// `db` must be null or a valid pointer previously returned by
/// [`relate_database_open`].
pub(crate) unsafe fn guard_db<F>(db: *mut relate_database, op: F) -> Result<(), RenderedError>
where
    F: FnOnce(&Database) -> Result<(), RenderedError>,
{
    // SAFETY: forwarded contract.
    unsafe { guard(db, |handle| op(&handle.db)) }
}

/// Run a connection operation under `catch_unwind` with the connection mutex
/// held, rejecting null or poisoned handles and poisoning the connection if
/// `op` unwinds. The `Err` half is the rendered ADR-0502 §2 error shape
/// ([`RenderedError`]), ready for the caller's `error_json` out-param.
///
/// # Safety
/// `conn` must be null or a valid pointer previously returned by
/// [`relate_connect_namespace`].
pub(crate) unsafe fn guard_conn<F>(conn: *mut relate_connection, op: F) -> Result<(), RenderedError>
where
    F: FnOnce(&mut Connection) -> Result<(), RenderedError>,
{
    // SAFETY: forwarded contract.
    unsafe {
        guard(conn, |handle| {
            let mut c = handle
                .conn
                .lock()
                .map_err(|e| boundary_error_json(&format!("connection mutex poisoned: {e}")))?;
            op(&mut c)
        })
    }
}

/// The containment skeleton every handle guard shares (ADR-0502 §2):
/// `op` runs under `catch_unwind`; its failure passes through, an unwind
/// poisons the handle (every later call refuses without touching the
/// possibly-inconsistent state) and reports the genericized panic
/// message. The guards add their own null/poisoned rejection and lock
/// acquisition around this. Plain safe code — the raw-pointer
/// dereferences stay in the guards, where they belong; the reference
/// `poisoned` belongs to the live handle `op` runs on by construction
/// (the guard holds it).
fn contain(
    poisoned: &AtomicBool,
    op: impl FnOnce() -> Result<(), RenderedError>,
) -> Result<(), RenderedError> {
    match catch_unwind(AssertUnwindSafe(op)) {
        Ok(result) => result,
        Err(payload) => {
            poisoned.store(true, Ordering::SeqCst);
            Err(boundary_error_json(&panic_message(payload)))
        }
    }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
unsafe fn database_open(
    uri: *const c_char,
    out: *mut *mut relate_database,
    mode: OpenMode,
) -> Result<(), RenderedError> {
    if out.is_null() {
        return Err(boundary_error_json("null out pointer"));
    }
    // SAFETY: `out` is non-null (checked above) and writable per the caller contract.
    unsafe {
        *out = ptr::null_mut();
    }
    // Decode and open inside the guarded closure so a panic never unwinds
    // across the FFI boundary.
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `uri` is null or a valid C string per the caller
        // contract; the one decode idiom, like every other boundary read.
        let uri = unsafe { read_cstr(uri) }.map_err(|m| boundary_error_json(&m))?;
        match Database::open_with(&uri, OpenOptions::with_mode(mode)) {
            Ok(db) => {
                // SAFETY: `out` is non-null (checked above) and writable per the caller contract.
                unsafe {
                    *out = Box::into_raw(Box::new(relate_database {
                        db,
                        poisoned: AtomicBool::new(false),
                    }));
                }
                Ok(())
            }
            Err(e) => Err(engine_error_json(&e)),
        }
    })) {
        Ok(result) => result,
        Err(payload) => Err(boundary_error_json(&panic_message(payload))),
    }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_database_open(
    uri: *const c_char,
    out: *mut *mut relate_database,
    error_json: *mut *mut c_char,
) -> relate_status {
    // SAFETY: forwarded to `database_open`, which checks `out` and `uri`.
    unsafe { report(database_open(uri, out, OpenMode::Writable), error_json) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_database_open_read_only(
    uri: *const c_char,
    out: *mut *mut relate_database,
    error_json: *mut *mut c_char,
) -> relate_status {
    // SAFETY: forwarded to `database_open`, which checks `out` and `uri`.
    unsafe { report(database_open(uri, out, OpenMode::ReadOnly), error_json) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_database_close(db: *mut relate_database) -> relate_status {
    if db.is_null() {
        return relate_status::RELATE_ERROR;
    }
    // SAFETY: `db` is non-null (checked above) and uniquely owned from
    // `relate_database_open` or `relate_database_open_read_only`.
    let boxed = unsafe { Box::from_raw(db) };
    // Dropping a poisoned handle's engine could, in principle, panic from an
    // inconsistent state; contain any unwind so it never crosses into C.
    let _ = catch_unwind(AssertUnwindSafe(move || drop(boxed)));
    ok()
}

/// The one connect entry point (ADR-0220): nulls and pre-nulls `out`, runs
/// the connect under [`guard_db`] (panic containment + poisoning), and
/// boxes the connection into `out` on success. A NULL `name` selects the
/// default `neo4j` namespace; a non-NULL `name` is decoded with [`read_cstr`]
/// and connected into that namespace.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_connect_namespace(
    db: *mut relate_database,
    name: *const c_char,
    out: *mut *mut relate_connection,
    error_json: *mut *mut c_char,
) -> relate_status {
    if out.is_null() {
        // SAFETY: the out-param is null, which `report` tolerates as opt-out.
        return unsafe { report(Err(boundary_error_json("null out pointer")), error_json) };
    }
    // SAFETY: `out` is non-null (checked above) and writable per the caller contract.
    unsafe {
        *out = ptr::null_mut();
    }
    let result = unsafe {
        guard_db(db, |d| {
            let conn = if name.is_null() {
                d.connect()
            } else {
                // SAFETY: `name` is null or a valid C string per the caller contract.
                d.connect_namespace(&read_cstr(name).map_err(|m| boundary_error_json(&m))?)
            };
            match conn {
                Ok(conn) => {
                    // SAFETY: `out` is non-null (checked above) and writable per the caller contract.
                    *out = Box::into_raw(Box::new(relate_connection {
                        conn: Mutex::new(conn),
                        poisoned: AtomicBool::new(false),
                    }));
                    Ok(())
                }
                Err(e) => Err(engine_error_json(&e)),
            }
        })
    };
    // SAFETY: the out-param contract was checked above; `report` fills it on failure.
    unsafe { report(result, error_json) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_conn_set_namespace(
    conn: *mut relate_connection,
    name: *const c_char,
    error_json: *mut *mut c_char,
) -> relate_status {
    let result = unsafe {
        guard_conn(conn, |c| {
            // SAFETY: `name` is null or a valid C string per the caller contract.
            let name = read_cstr(name).map_err(|m| boundary_error_json(&m))?;
            c.set_namespace(&name).map_err(|e| engine_error_json(&e))
        })
    };
    // SAFETY: `error_json` is null or a writable out-param per the caller contract.
    unsafe { report(result, error_json) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_disconnect(conn: *mut relate_connection) -> relate_status {
    if conn.is_null() {
        return relate_status::RELATE_ERROR;
    }
    // SAFETY: `conn` is non-null (checked above) and uniquely owned from
    // `relate_connect_namespace`.
    let boxed = unsafe { Box::from_raw(conn) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(boxed)));
    ok()
}

/// The one query entry point (ADR-0220). `params` is a typed params builder
/// (ADR-0502 §4), read and not modified; NULL runs the statement with no
/// parameters.
///
/// A statement failure rides the result (ADR-0502 §2): the call returns
/// `RELATE_OK` with an error result (`relate_result_is_error` is
/// 1). `RELATE_ERROR` is reserved for connection-level failures,
/// reported through the trailing `error_json` out-param like every other
/// status call.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_query_params(
    conn: *mut relate_connection,
    cypher: *const c_char,
    params: *const relate_params,
    out: *mut *mut relate_query_result,
    error_json: *mut *mut c_char,
) -> relate_status {
    if out.is_null() {
        // SAFETY: the out-param is null, which `report` tolerates as opt-out.
        return unsafe { report(Err(boundary_error_json("null out pointer")), error_json) };
    }
    // SAFETY: `out` is non-null (checked above) and writable per the caller contract.
    unsafe {
        *out = ptr::null_mut();
    }
    // All fallible decoding (UTF-8 decode, params lowering) runs inside the
    // guarded closure so the whole body is covered by `catch_unwind`; only
    // the infallible null checks are left outside.
    let result = unsafe {
        guard_conn(conn, |c| {
            // SAFETY: `cypher` is null or a valid C string per the caller contract.
            let cypher = read_cstr(cypher).map_err(|m| boundary_error_json(&m))?;
            // SAFETY: `params` is null or a live builder per the caller contract.
            let params = params::statement_params(params).map_err(|m| boundary_error_json(&m))?;
            match c.query_with_params(&cypher, params) {
                // SAFETY: `out` is non-null (checked above) and writable per
                // the caller contract; both halves install exactly one
                // result the caller destroys.
                Ok(r) => {
                    *out = pack_result(r);
                    Ok(())
                }
                Err(e) => {
                    *out = pack_error_result(&e);
                    Ok(())
                }
            }
        })
    };
    // SAFETY: the out-param contract was checked above; `report` fills it on failure.
    unsafe { report(result, error_json) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_conn_begin(
    conn: *mut relate_connection,
    error_json: *mut *mut c_char,
) -> relate_status {
    let result = unsafe { guard_conn(conn, |c| c.begin().map_err(|e| engine_error_json(&e))) };
    // SAFETY: `error_json` is null or a writable out-param per the caller contract.
    unsafe { report(result, error_json) }
}

/// Commit the connection's explicit transaction (ADR-0502 §7; the status
/// shape is ADR-0510): `RELATE_OK` with `*bookmark_out` the
/// transaction's bookmark as a caller-owned string — the engine's
/// `relate:<n>` commit-sequence form, freed with
/// [`crate::relate_free_string`] — or `*bookmark_out` NULL when the
/// transaction committed nothing (no write ran, or every write was a
/// no-op): no bookmark is fabricated for a commit that crossed no WAL
/// boundary. A failure keeps the convention of every fallible status
/// call: `RELATE_ERROR` with `*bookmark_out` NULL and the trailing
/// `error_json` out-param filled with the ADR-0502 §2 shape — the status
/// alone distinguishes a failure from an empty commit, even for a caller
/// that opted out of the detail. A NULL `bookmark_out` is a refused call
/// (`RELATE_ERROR`, the transaction NOT committed): every payload
/// out-param of this ABI refuses NULL, and commit is the durability
/// boundary — the bookmark is never silently discarded.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_conn_commit(
    conn: *mut relate_connection,
    bookmark_out: *mut *mut c_char,
    error_json: *mut *mut c_char,
) -> relate_status {
    if bookmark_out.is_null() {
        // SAFETY: `error_json` is null or a writable out-param per the
        // caller contract.
        return unsafe { report(Err(boundary_error_json("null out pointer")), error_json) };
    }
    // SAFETY: `bookmark_out` is non-null (checked above) and writable per
    // the caller contract; both outcomes below leave it set exactly once.
    unsafe {
        *bookmark_out = ptr::null_mut();
    }
    let mut bookmark: Option<String> = None;
    let result = unsafe {
        guard_conn(conn, |c| match c.commit() {
            Ok(bm) => {
                bookmark = bm;
                Ok(())
            }
            Err(e) => Err(engine_error_json(&e)),
        })
    };
    if let Err(json) = result {
        // SAFETY: `error_json` is null or a writable out-param per the
        // caller contract; `report` fills it and returns RELATE_ERROR
        // with `*bookmark_out` still NULL.
        return unsafe { report(Err(json), error_json) };
    }
    if let Some(bm) = bookmark {
        // The engine's ADR-0310 form `relate:<n>` — a colon and
        // decimal digits, no interior NUL; construction is the invariant.
        let c = CString::new(bm).expect("bookmark contains no interior NUL");
        // SAFETY: `bookmark_out` is non-null (checked above) and writable
        // per the caller contract.
        unsafe {
            *bookmark_out = c.into_raw();
        }
    }
    ok()
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_conn_rollback(
    conn: *mut relate_connection,
    error_json: *mut *mut c_char,
) -> relate_status {
    let result = unsafe { guard_conn(conn, |c| c.rollback().map_err(|e| engine_error_json(&e))) };
    // SAFETY: `error_json` is null or a writable out-param per the caller contract.
    unsafe { report(result, error_json) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_checkpoint(
    db: *mut relate_database,
    error_json: *mut *mut c_char,
) -> relate_status {
    let result = unsafe { guard_db(db, |d| d.checkpoint().map_err(|e| engine_error_json(&e))) };
    // SAFETY: `error_json` is null or a writable out-param per the caller contract.
    unsafe { report(result, error_json) }
}
