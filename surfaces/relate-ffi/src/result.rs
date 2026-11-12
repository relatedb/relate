//! The query result (ADR-0502 §3) — the result handle, its packers (a
//! statement's rows and metadata, or a statement failure as an error
//! result) and every read accessor, including the cell value handles
//! ([`crate::value`]) and the result's own destruction. Its own module
//! because `tabular` — the database and connection handles — is a client
//! of it, not its home.

#![allow(non_camel_case_types)]

use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use relate::{value_to_cypher, Error, QueryResult};
use relate_model::wire;

use crate::abi::engine_error_json;
use crate::value::{self, relate_value};

/// The result handle (ADR-0502 §2) models its two modes as one fact: a
/// statement either produced a rows frame or failed with the one error
/// shape. An error result reports the empty frame through every rows
/// accessor — the statement never produced those fields.
pub struct relate_query_result {
    body: ResultBody,
}

enum ResultBody {
    Rows(Rows),
    /// The ADR-0502 §2 error shape when the statement failed:
    /// [`relate_result_is_error`] derives from the variant, so there is
    /// one fact, not two.
    Error(CString),
}

/// The success frame: rows plus every metadata field, each stamped once at
/// construction.
struct Rows {
    columns: Vec<CString>,
    /// One root value handle per cell (ADR-0502 §1), row-major. The handles
    /// — and every child minted below them — are owned here and freed with
    /// the result; the `Vec` buffers are never mutated after construction,
    /// so a handle's address is stable for the result's lifetime.
    cells: Vec<Vec<relate_value>>,
    /// The statement's resolved namespace (ADR-0117 §7), stamped at
    /// construction: `"neo4j"`, `"system"` or `"system_ontologies"`.
    namespace: CString,
    /// The statement's query type (ADR-0162), stamped at construction:
    /// `"r"` (read), `"w"` (a write with no returned columns), `"rw"` (a
    /// write with returned columns) or `"s"` (schema), the split per
    /// ADR-0332.
    query_type: CString,
    /// The canonical 11 mutation counters as a JSON object string
    /// (`stats_to_wire_json`), stamped at construction (ADR-0216).
    stats_json: CString,
    /// The statement's structured plan as a `relate.plan/v1` JSON
    /// document (LABS-370), stamped at construction: `None` when the
    /// statement carried no plan (a plain run — the accessor then returns
    /// NULL, the C convention for absent), else the agent-native
    /// projection of the plan tree — the schema our own bindings hand out,
    /// distinct from the Neo4j-facing Bolt/HTTP shape.
    plan_json: Option<CString>,
    /// The statement's query notifications (ADR-0363) as a JSON array string
    /// in the engine-native shape (`notifications_to_wire_json`), stamped at
    /// construction: `[]` on a plain run, populated by `EXPLAIN`/`PROFILE`.
    notifications_json: CString,
    /// The statement's commit bookmark (ADR-0502 §7): the engine's
    /// `relate:<n>` string verbatim, stamped at construction for an
    /// auto-commit write. `None` on every statement that committed nothing
    /// — a read, a write whose row pipeline was empty, an `EXPLAIN`, a
    /// statement inside an explicit transaction — the
    /// accessor then returns NULL, the C convention for absent.
    bookmark: Option<CString>,
    /// The engine-reported statement elapsed in wall-clock milliseconds
    /// (ADR-0221), stamped at construction.
    elapsed_ms: f64,
    /// The statement's PARSE phase in milliseconds (ADR-0491) — the
    /// front-end parse and lowering window of `elapsed_ms` — stamped at
    /// construction.
    parse_ms: f64,
    /// The statement's PLAN phase in milliseconds (ADR-0491) — binding,
    /// logical planning, admission classification — stamped at
    /// construction.
    plan_ms: f64,
    /// The statement's EXECUTE phase in milliseconds (ADR-0491) — the
    /// dispatch, or the EXPLAIN plan render — stamped at construction.
    ///
    execute_ms: f64,
}

/// Run a read-only result accessor under `catch_unwind`; a null result or a
/// caught panic yields `fallback`. The one guard for every result-accessor
/// return type — a new accessor adds a call, not a copy.
///
/// # Safety
/// `result` must be null or a valid pointer previously returned by
/// [`relate_query_params`].
unsafe fn guard_result<T, F>(result: *mut relate_query_result, fallback: T, op: F) -> T
where
    F: FnOnce(&relate_query_result) -> T,
{
    if result.is_null() {
        return fallback;
    }
    // SAFETY: `result` is non-null and valid per this function's contract.
    match catch_unwind(AssertUnwindSafe(|| op(unsafe { &*result }))) {
        Ok(v) => v,
        Err(_) => fallback,
    }
}

/// [`guard_result`] over the rows frame: an error result takes the same
/// `fallback` a null result does — the empty frame, one expression per
/// accessor.
///
/// # Safety
/// As [`guard_result`].
unsafe fn guard_rows<T, F>(result: *mut relate_query_result, fallback: T, op: F) -> T
where
    F: FnOnce(&Rows) -> T,
{
    // SAFETY: forwarded under this function's identical contract.
    unsafe {
        guard_result(result, None, |r| match &r.body {
            ResultBody::Rows(rows) => Some(op(rows)),
            ResultBody::Error(_) => None,
        })
    }
    .unwrap_or(fallback)
}

/// Pack a query result: each cell converts exactly once into a root value
/// handle (ADR-0502 §1) spelled in the statement's resolved namespace.
/// Every field is spelled — the success frame is exhaustive, so a new
/// field is a compile error here.
pub(crate) fn pack_result(r: QueryResult) -> *mut relate_query_result {
    // Column names come from identifiers or generated aliases and cannot
    // carry an interior NUL. A violation is an engine invariant break and
    // panics (caught by the caller's catch_unwind) rather than silently
    // truncating a column name.
    let columns: Vec<CString> = r
        .header
        .columns
        .iter()
        .map(|c| CString::new(c.as_str()).expect("column name contains an interior NUL"))
        .collect();
    let ns = r.header.namespace;
    let cells: Vec<Vec<relate_value>> = r
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|v| relate_value::root(value_to_cypher(v), ns))
                .collect()
        })
        .collect();
    // `Namespace::as_str` and `QueryType::as_str` are fixed strings with no
    // interior NUL; construction is the stamp (ADR-0285).
    let namespace =
        CString::new(r.header.namespace.as_str()).expect("namespace contains an interior NUL");
    let query_type =
        CString::new(r.header.query_type.as_str()).expect("query type contains an interior NUL");
    let stats_json =
        CString::new(wire::stats_to_wire_json(&relate::query_stats_to_wire(&r.stats)).to_string())
            .expect("wire JSON contains an interior NUL");
    // The agent-native plan document (LABS-370): projected here, at the
    // one result-construction boundary, from the engine's plan tree through
    // the shared model projection — empty when the statement planned none.
    // The canonical read half of `QueryResult::plan`, like every other
    // surface that renders a tree.
    let plan_json = relate::plan_wire_json(&r).map(|document| {
        CString::new(document.to_string()).expect("the plan JSON contains no interior NUL")
    });
    // ADR-0363 decision 9: serialized once here, through the shared
    // engine-native serializer every surface projects from.
    let notifications_json =
        CString::new(wire::notifications_to_wire_json(r.notifications()).to_string())
            .expect("wire JSON contains an interior NUL");
    let elapsed_ms = r.elapsed_ms();
    // The ADR-0491 phase split, stamped at the same construction boundary
    // from the same engine result.
    let parse_ms = r.parse_ms();
    let plan_ms = r.plan_ms();
    let execute_ms = r.execute_ms();
    // The engine's ADR-0310 form `relate:<n>` — a colon and decimal
    // digits, no interior NUL; construction is the stamp.
    let bookmark = r
        .bookmark
        .map(|b| CString::new(b).expect("bookmark contains no interior NUL"));
    Box::into_raw(Box::new(relate_query_result {
        body: ResultBody::Rows(Rows {
            columns,
            cells,
            namespace,
            query_type,
            stats_json,
            plan_json,
            notifications_json,
            bookmark,
            elapsed_ms,
            parse_ms,
            plan_ms,
            execute_ms,
        }),
    }))
}

/// Pack a statement failure as an error result (ADR-0502 §2): the handle
/// carries the one error shape and every other accessor reports the empty
/// frame — the statement never produced them.
pub(crate) fn pack_error_result(e: &Error) -> *mut relate_query_result {
    Box::into_raw(Box::new(relate_query_result {
        body: ResultBody::Error(engine_error_json(e).into_c_string()),
    }))
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_column_count(result: *mut relate_query_result) -> i64 {
    unsafe { guard_rows(result, 0, |r| r.columns.len() as i64) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_row_count(result: *mut relate_query_result) -> i64 {
    unsafe { guard_rows(result, 0, |r| r.cells.len() as i64) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_column_name(
    result: *mut relate_query_result,
    col: i64,
) -> *const c_char {
    unsafe {
        guard_rows(result, ptr::null(), |r| {
            // The one i64 -> usize index conversion, like relate_value_at.
            value::index(col)
                .and_then(|col| r.columns.get(col))
                .map(|c| c.as_ptr())
                .unwrap_or(ptr::null())
        })
    }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_is_error(result: *mut relate_query_result) -> u8 {
    unsafe { guard_result(result, 0, |r| matches!(r.body, ResultBody::Error(_)) as u8) }
}

/// The ADR-0502 §2 error shape when the result is an error result, NULL on
/// a good result. Result-owned and valid until `relate_destroy_result`.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_error_json(
    result: *mut relate_query_result,
) -> *const c_char {
    unsafe {
        guard_result(result, ptr::null(), |r| match &r.body {
            ResultBody::Error(json) => json.as_ptr(),
            ResultBody::Rows(_) => ptr::null(),
        })
    }
}

/// The auto-commit statement's commit bookmark (ADR-0502 §7): the engine's
/// `relate:<n>` string verbatim, result-owned and valid until
/// [`relate_destroy_result`]. NULL when the statement committed
/// nothing — a read, a write whose row pipeline was empty, an `EXPLAIN`,
/// a statement inside an explicit transaction (its commit's bookmark
/// arrives through [`relate_conn_commit`]'s `bookmark_out`
/// out-param), and every error result. NULL result -> NULL.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_bookmark(result: *mut relate_query_result) -> *const c_char {
    unsafe {
        guard_rows(result, ptr::null(), |r| {
            r.bookmark.as_ref().map_or(ptr::null(), |bm| bm.as_ptr())
        })
    }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_query_type(
    result: *mut relate_query_result,
) -> *const c_char {
    unsafe { guard_rows(result, ptr::null(), |r| r.query_type.as_ptr()) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_namespace(
    result: *mut relate_query_result,
) -> *const c_char {
    unsafe { guard_rows(result, ptr::null(), |r| r.namespace.as_ptr()) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_stats(result: *mut relate_query_result) -> *const c_char {
    unsafe { guard_rows(result, ptr::null(), |r| r.stats_json.as_ptr()) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_notifications(
    result: *mut relate_query_result,
) -> *const c_char {
    unsafe { guard_rows(result, ptr::null(), |r| r.notifications_json.as_ptr()) }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_plan_json(
    result: *mut relate_query_result,
) -> *const c_char {
    unsafe {
        guard_rows(result, ptr::null(), |r| {
            r.plan_json
                .as_ref()
                .map_or(ptr::null(), |json| json.as_ptr())
        })
    }
}

/// The engine-reported statement elapsed in wall-clock milliseconds
/// (ADR-0221); `0.0` on an error result. A null result yields `0.0`.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_elapsed_ms(result: *mut relate_query_result) -> f64 {
    unsafe { guard_rows(result, 0.0, |r| r.elapsed_ms) }
}

/// The statement's PARSE phase in wall-clock milliseconds (ADR-0491): the
/// front-end parse and lowering window of [`relate_result_elapsed_ms`].
/// The three phase accessors are disjoint sub-windows whose sum never
/// exceeds the elapsed total. `0.0` on an error result. A null result
/// yields `0.0`.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_parse_ms(result: *mut relate_query_result) -> f64 {
    unsafe { guard_rows(result, 0.0, |r| r.parse_ms) }
}

/// The statement's PLAN phase in wall-clock milliseconds (ADR-0491): the
/// binding, logical planning and admission classification window of
/// [`relate_result_elapsed_ms`]. `0.0` on an error result. A null
/// result yields `0.0`.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_plan_ms(result: *mut relate_query_result) -> f64 {
    unsafe { guard_rows(result, 0.0, |r| r.plan_ms) }
}

/// The statement's EXECUTE phase in wall-clock milliseconds (ADR-0491): the
/// dispatch window of [`relate_result_elapsed_ms`] — or the EXPLAIN
/// plan render for an `EXPLAIN` statement. `0.0` on an error result. A
/// null result yields `0.0`.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_result_execute_ms(result: *mut relate_query_result) -> f64 {
    unsafe { guard_rows(result, 0.0, |r| r.execute_ms) }
}

/// The root value handle of cell (`row`, `col`) (ADR-0502 §1), owned by
/// the result and freed by `relate_destroy_result`. NULL out of range
/// (either index negative or past the end — an error result has no cells)
/// and for a NULL result.
///
/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_value_at(
    result: *mut relate_query_result,
    row: i64,
    col: i64,
) -> *const relate_value {
    unsafe {
        guard_rows(result, ptr::null(), |r| {
            let (row, col) = (value::index(row), value::index(col));
            row.zip(col)
                .and_then(|(row, col)| r.cells.get(row)?.get(col))
                .map_or(ptr::null(), |v| v as *const relate_value)
        })
    }
}

/// # Safety
/// Pointer arguments must be null or valid for this call, matching the C header.
#[no_mangle]
pub unsafe extern "C" fn relate_destroy_result(result: *mut relate_query_result) {
    if result.is_null() {
        return;
    }
    // SAFETY: `result` is non-null (checked above) and uniquely owned from a query call.
    let boxed = unsafe { Box::from_raw(result) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(boxed)));
}
