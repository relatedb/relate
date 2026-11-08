//! The typed params builder (ADR-0502 §4) — the one parameter path of ABI
//! v2, the inbound mirror of the value walker ([`crate::value`]).
//!
//! A [`relate_params`] is a container builder: the root
//! (`relate_params_new`) is a MAP of named statement parameters, and
//! `relate_params_put_list` / `_put_map` insert a nested LIST / MAP and
//! hand back its child builder. The same putters serve every builder; the
//! container decides the name rule — a MAP takes a non-NULL, unique name, a
//! LIST takes NULL (the value is appended).
//!
//! Lifetime: one tree, one owner. The root owns every child; a child
//! handle is borrowed from it, stays valid (and writable) until
//! `relate_params_free(root)`, and is never freed itself — freeing a
//! child handle is a no-op. There is no build/close step: the builder is
//! read as it stands when `relate_query_params` runs, does not change,
//! and may be extended and reused afterwards.
//!
//! Lowering is typed end to end: each putter builds the engine's
//! [`CypherValue`] from its components at put time, and
//! [`statement_params`] lowers the tree through `relate::cypher_to_value`
//! — the bridge the Python binding uses — with no JSON intermediate.
//!
//! Refusals follow the walker's convention, extended to the error channel
//! (ADR-0502 §2): a NULL builder, a name that breaks the container's rule,
//! an out-of-range component, an unknown zone, a srid/dimension mismatch, a
//! coordinate the vector dtype cannot hold, or nesting past the engine's
//! value-depth cap (ADR-0462) returns `RELATE_ERROR` with the
//! internal-stage error shape in the trailing `error_json` out-param, and
//! leaves the builder unchanged and usable.

#![allow(non_camel_case_types)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::rc::Rc;

use chrono::{TimeZone, Utc};
use relate::{cypher_to_value, Value, MAX_VALUE_DEPTH};
use relate_core::temporal::{zone_offset_at, zone_offset_at_utc};
use relate_model::value::{IsoDuration, PointValue, VectorCoordKind};
use relate_model::{CypherValue, IndexMap};

use crate::abi::{boundary_error_json, panic_message, read_cstr_bounded, relate_status, report};

mod decode;
use self::decode::{
    calendar_date, coordinates, fixed_offset, local_datetime, vector_dtype, wall_time, zone_arg,
    zone_refusal,
};

/// The entry limit of one builder container — a map's named entries or a
/// list's positional items (OPLANE_REQ-00340363, LABS-937): a runaway loop
/// over a putter fails with a typed refusal instead of accumulating
/// without bound. Fixed constant, no knob — the house rule of ADR-0257's
/// 4 MiB/500 and ADR-0462's `MAX_VALUE_DEPTH`.
pub const MAX_PARAM_ENTRIES: usize = 100_000;

/// The byte limit of one bounded string argument (OPLANE_REQ-00340357):
/// parameter names are identifiers and 4 KiB is far above any legitimate
/// one, in the house style of the fixed caps above — and the zone ids and
/// vector dtype names the zoned/vector putters read ride the same bound.
pub const MAX_PARAM_NAME_BYTES: usize = 4096;

/// The builder-wide byte budget: the same one wire-message bound — a
/// parameter set larger than one Bolt message could never ride the wire,
/// so the whole builder (names + values + a fixed per-entry overhead,
/// across every nested container) is refused past it, not just each
/// container separately (OPLANE_REQ-00340363: the aggregate bound).
pub const MAX_PARAM_TOTAL_BYTES: usize = relate_core::MAX_WIRE_MESSAGE_BYTES;

/// The fixed per-entry overhead every put charges the builder budget:
/// covers a scalar value and the entry's own bookkeeping (the big values
/// — strings and vector coordinates — charge their real bytes on top).
const ENTRY_OVERHEAD_BYTES: usize = 16;

/// The pre-copy bound every big-value putter shares (OPLANE_REQ-00340363):
/// a value whose bytes alone could spend more than the whole builder
/// budget is refused before any of them are copied — the string bytes are
/// read and the coordinate buffer allocated before insert could refuse
/// them. `what` names the argument in the refusal.
fn pre_copy_bound(bytes: usize, what: &str) -> Result<(), String> {
    if bytes > MAX_PARAM_TOTAL_BYTES {
        return Err(format!(
            "{what} exceeds the builder's total byte budget of {MAX_PARAM_TOTAL_BYTES}"
        ));
    }
    Ok(())
}

/// One params builder (ADR-0502 §4): a MAP or LIST under construction.
/// Opaque to C.
pub struct relate_params {
    container: Container,
    /// The builder-wide byte budget (OPLANE_REQ-00340363): every entry
    /// charges its name, its value's bytes and a fixed overhead here —
    /// one fact shared by the root and every child, so the aggregate is
    /// bounded, not just each container.
    budget: Rc<Cell<usize>>,
    /// Container nesting depth: 0 for the root (the statement's parameter
    /// map is not itself a value — the only depth-0 builder, and the one
    /// handle `relate_params_free` frees), 1 for a parameter that is a
    /// list or map, and so on — the engine's value depth of the lowered
    /// container.
    depth: u32,
}

/// What a builder collects.
enum Container {
    /// Named entries in insertion order: the root and `put_map` children.
    Map(IndexMap<String, Entry>),
    /// Positional entries: `put_list` children.
    List(Vec<Entry>),
}

impl Container {
    /// The number of entries either kind holds — the entry cap's fact.
    fn len(&self) -> usize {
        match self {
            Container::Map(entries) => entries.len(),
            Container::List(items) => items.len(),
        }
    }

    /// The container as the engine value it stands for.
    fn lower(&self) -> CypherValue {
        match self {
            Container::Map(entries) => CypherValue::Map(
                entries
                    .iter()
                    .map(|(name, entry)| (name.clone(), entry.lower()))
                    .collect(),
            ),
            Container::List(items) => CypherValue::List(items.iter().map(Entry::lower).collect()),
        }
    }
}

/// One put: a finished value, or a nested builder still open for puts.
enum Entry {
    Value(CypherValue),
    Child(Child),
}

/// A nested builder, owned by its parent's entry. Held as a raw pointer
/// (not a `Box`) on purpose: the address handed to C must stay valid —
/// and stay the ONE provenance every access goes through — while the
/// parent's container moves or reallocates its entries. A `Box` entry
/// cannot do that: moving a `Box` re-tags its allocation, which
/// invalidates the outstanding raw handle (Stacked Borrows — Miri flags
/// the write-through-handle-after-read-through-Box sequence as undefined
/// behavior), so the `Box` shape is not a simplification here. Freed
/// exactly once, here.
struct Child(NonNull<relate_params>);

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::into_raw` in `put_child` and
        // is owned by this entry alone; a child handle is never freed
        // through `relate_params_free`.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

impl relate_params {
    /// `budget` is the builder-wide byte counter: the root mints a fresh
    /// one, a child receives its parent's — one tree, one budget, by
    /// construction. The root is the only depth-0 builder
    /// (`relate_params_new` mints it), so the depth alone names it.
    fn new(container: Container, depth: u32, budget: Rc<Cell<usize>>) -> Self {
        relate_params {
            container,
            budget,
            depth,
        }
    }

    /// Insert `entry` under the container's name rule: a MAP takes a
    /// unique name, a LIST takes none; neither container takes a
    /// [`MAX_PARAM_ENTRIES`]'th entry.
    fn insert(
        &mut self,
        name: Option<String>,
        entry: Entry,
        value_bytes: usize,
    ) -> Result<(), String> {
        if self.container.len() >= MAX_PARAM_ENTRIES {
            return Err(format!(
                "parameter container exceeds the entry limit of {MAX_PARAM_ENTRIES}"
            ));
        }
        // The name rule and the duplicate check decide first; the budget
        // is charged only by a put that actually lands, so a refused put
        // never spends it.
        let charge = value_bytes + name.as_ref().map_or(0, |n| n.len()) + ENTRY_OVERHEAD_BYTES;
        let total = self.budget.get().saturating_add(charge);
        if total > MAX_PARAM_TOTAL_BYTES {
            return Err(format!(
                "parameter builder exceeds the total byte budget of {MAX_PARAM_TOTAL_BYTES}"
            ));
        }
        let outcome = match (&mut self.container, name) {
            (Container::Map(entries), Some(name)) => {
                if entries.contains_key(&name) {
                    Err(format!("duplicate parameter name '{name}'"))
                } else {
                    entries.insert(name, entry);
                    Ok(())
                }
            }
            (Container::Map(_), None) => Err("a map builder needs a non-NULL name".to_string()),
            (Container::List(items), None) => {
                items.push(entry);
                Ok(())
            }
            (Container::List(_), Some(_)) => {
                Err("a list builder takes a NULL name; values append".to_string())
            }
        };
        if outcome.is_ok() {
            self.budget.set(total);
        }
        outcome
    }
}

impl Entry {
    fn lower(&self) -> CypherValue {
        match self {
            Entry::Value(v) => v.clone(),
            // SAFETY: a child pointer is live for as long as its entry
            // (see `Child`).
            Entry::Child(child) => unsafe { child.0.as_ref() }.container.lower(),
        }
    }
}

/// Lower a builder into the statement's parameters (ADR-0502 §4): a NULL
/// builder is no parameters; a MAP builder's entries — the root, or a
/// `put_map` child, which the C header and spec let a caller bind as its
/// own parameter set — lower through `cypher_to_value`, the typed bridge
/// every typed binding shares. A LIST builder is refused — parameters
/// are named.
///
/// # Safety
/// `p` must be null or a live builder handle.
pub(crate) unsafe fn statement_params(
    p: *const relate_params,
) -> Result<BTreeMap<String, Value>, String> {
    // SAFETY: `p` is null or live per this function's contract.
    let Some(builder) = (unsafe { p.as_ref() }) else {
        return Ok(BTreeMap::new());
    };
    match &builder.container {
        Container::Map(entries) => Ok(entries
            .iter()
            .map(|(name, entry)| (name.clone(), cypher_to_value(&entry.lower())))
            .collect()),
        Container::List(_) => Err("params must be a map builder, not a list builder".to_string()),
    }
}

/// Run one putter: decode the name, build the value under `catch_unwind`
/// and insert it, reporting a refusal on the error channel. The one guard
/// every putter shares.
///
/// # Safety
/// `p` must be null or a live builder; `name` null or a NUL-terminated
/// string; `error_json` null or writable.
unsafe fn put(
    p: *mut relate_params,
    name: *const c_char,
    error_json: *mut *mut c_char,
    build: impl FnOnce(&relate_params) -> Result<(Entry, usize), String>,
) -> relate_status {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `p` is null or live per this function's contract; calls
        // on one builder are serialized by the caller.
        let builder = unsafe { p.as_mut() }.ok_or_else(|| "null params builder".to_string())?;
        let name = if name.is_null() {
            None
        } else {
            // The bounded name read: one walk, refused past the cap
            // before its bytes are copied (OPLANE_REQ-00340357).
            // SAFETY: `name` is non-null and NUL-terminated per the
            // contract; the read touches at most `MAX_PARAM_NAME_BYTES +
            // 1` bytes of that contract-trusted buffer.
            Some(unsafe { read_cstr_bounded(name, MAX_PARAM_NAME_BYTES, "parameter name") }?)
        };
        let (entry, value_bytes) = build(builder)?;
        builder.insert(name, entry, value_bytes)
    }));
    let result = match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => Err(boundary_error_json(&message)),
        Err(payload) => Err(boundary_error_json(&panic_message(payload))),
    };
    // SAFETY: `error_json` is null or writable per the contract.
    unsafe { report(result, error_json) }
}

/// A finished value as an entry.
fn value(v: CypherValue) -> Result<(Entry, usize), String> {
    // A scalar's bytes are covered by the fixed per-entry overhead; the
    // big values (strings, vectors) state their own charge at their
    // putter, where the size is known before any copy.
    Ok((Entry::Value(v), 0))
}

/// A new, empty root builder: the statement's parameter map. Free it with
/// [`relate_params_free`]. Never NULL.
#[no_mangle]
pub extern "C" fn relate_params_new() -> *mut relate_params {
    Box::into_raw(Box::new(relate_params::new(
        Container::Map(IndexMap::new()),
        0,
        Rc::new(Cell::new(0)),
    )))
}

/// Free a root builder and every child below it. NULL and child handles
/// are no-ops (a child belongs to its root).
///
/// # Safety
/// `p` must be null or a live builder handle.
#[no_mangle]
pub unsafe extern "C" fn relate_params_free(p: *mut relate_params) {
    // SAFETY: `p` is null or live per the contract.
    let Some(builder) = (unsafe { p.as_ref() }) else {
        return;
    };
    // The root is the only depth-0 builder — `relate_params_new` mints
    // it; every child sits at depth >= 1 under a root that frees it.
    if builder.depth != 0 {
        return;
    }
    // SAFETY: a root came from `Box::into_raw` in `relate_params_new`
    // and is freed once, here.
    let boxed = unsafe { Box::from_raw(p) };
    let _ = catch_unwind(AssertUnwindSafe(move || drop(boxed)));
}

/// # Safety
/// See the C header: `p` null or live, `name` null or NUL-terminated,
/// `error_json` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_null(
    p: *mut relate_params,
    name: *const c_char,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe { put(p, name, error_json, |_| value(CypherValue::Null)) }
}

/// # Safety
/// See [`relate_params_put_null`].
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_bool(
    p: *mut relate_params,
    name: *const c_char,
    v: u8,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe { put(p, name, error_json, |_| value(CypherValue::Bool(v != 0))) }
}

/// # Safety
/// See [`relate_params_put_null`].
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_int(
    p: *mut relate_params,
    name: *const c_char,
    v: i64,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe { put(p, name, error_json, |_| value(CypherValue::Int(v))) }
}

/// # Safety
/// See [`relate_params_put_null`].
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_float(
    p: *mut relate_params,
    name: *const c_char,
    v: f64,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe { put(p, name, error_json, |_| value(CypherValue::Float(v))) }
}

/// A STRING of `len` bytes of UTF-8 (an interior NUL crosses whole).
///
/// # Safety
/// See [`relate_params_put_null`]; `v` null or `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_string(
    p: *mut relate_params,
    name: *const c_char,
    v: *const c_char,
    len: usize,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            // A NULL buffer with no bytes is the canonical C spelling of the
            // empty STRING — a marshalling layer may hand a
            // zero-length buffer over as NULL. Bytes to read at NULL stays
            // the refusal; an explicit null is `relate_params_put_null`.
            if v.is_null() {
                if len == 0 {
                    return value(CypherValue::Str(String::new()));
                }
                return Err("null string value".to_string());
            }
            pre_copy_bound(len, "string parameter")?;
            // SAFETY: `v` is non-null and points to `len` bytes per the
            // contract.
            let bytes = std::slice::from_raw_parts(v.cast::<u8>(), len);
            let s = std::str::from_utf8(bytes).map_err(|_| "invalid utf-8".to_string())?;
            value(CypherValue::Str(s.to_string())).map(|(entry, _)| (entry, len))
        })
    }
}

/// # Safety
/// See [`relate_params_put_null`].
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_date(
    p: *mut relate_params,
    name: *const c_char,
    year: i64,
    month: i64,
    day: i64,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            value(CypherValue::Date(calendar_date(year, month, day)?))
        })
    }
}

/// A TIME: the wall clock and its UTC offset. A non-NULL `zone` (an IANA
/// region id) supplies the offset instead, resolved at the current instant
/// as Cypher's `time({timezone: …})` does; a TIME keeps no zone id.
///
/// # Safety
/// See [`relate_params_put_null`]; `zone` null or NUL-terminated.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn relate_params_put_time(
    p: *mut relate_params,
    name: *const c_char,
    hour: i64,
    minute: i64,
    second: i64,
    nanosecond: i64,
    offset_seconds: i64,
    zone: *const c_char,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            let time = wall_time(hour, minute, second, nanosecond)?;
            let offset = match zone_arg(zone)? {
                None => fixed_offset(offset_seconds)?,
                Some(zone) => {
                    let now = Utc::now().naive_utc();
                    let seconds =
                        zone_offset_at_utc(&zone, now).map_err(|e| zone_refusal(&zone, e))?;
                    fixed_offset(i64::from(seconds))?
                }
            };
            value(CypherValue::Time(time, Some(offset)))
        })
    }
}

/// # Safety
/// See [`relate_params_put_null`].
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_local_time(
    p: *mut relate_params,
    name: *const c_char,
    hour: i64,
    minute: i64,
    second: i64,
    nanosecond: i64,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            value(CypherValue::Time(
                wall_time(hour, minute, second, nanosecond)?,
                None,
            ))
        })
    }
}

/// A DATETIME: the local wall clock and its UTC offset — or, with a
/// non-NULL `zone` (an IANA region id), the zone, which the value keeps:
/// the offset is resolved from the zone at that wall clock (ADR-0178: a DST
/// overlap takes the earlier offset; a DST gap refuses) and
/// `offset_seconds` is ignored.
///
/// # Safety
/// See [`relate_params_put_null`]; `zone` null or NUL-terminated.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn relate_params_put_datetime(
    p: *mut relate_params,
    name: *const c_char,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    nanosecond: i64,
    offset_seconds: i64,
    zone: *const c_char,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            let local = local_datetime([year, month, day, hour, minute, second, nanosecond])?;
            let zone = zone_arg(zone)?;
            let offset = match &zone {
                None => fixed_offset(offset_seconds)?,
                Some(zone) => {
                    let seconds = zone_offset_at(zone, local).map_err(|e| zone_refusal(zone, e))?;
                    fixed_offset(i64::from(seconds))?
                }
            };
            let instant = offset
                .from_local_datetime(&local)
                .single()
                .ok_or_else(|| "datetime out of range".to_string())?;
            value(CypherValue::DateTime(instant, zone))
        })
    }
}

/// # Safety
/// See [`relate_params_put_null`].
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn relate_params_put_local_datetime(
    p: *mut relate_params,
    name: *const c_char,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    nanosecond: i64,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            value(CypherValue::LocalDateTime(local_datetime([
                year, month, day, hour, minute, second, nanosecond,
            ])?))
        })
    }
}

/// A DURATION from its four lossless components — the walker's
/// `relate_value_duration` read in reverse.
///
/// # Safety
/// See [`relate_params_put_null`].
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_duration(
    p: *mut relate_params,
    name: *const c_char,
    months: i64,
    days: i64,
    seconds: i64,
    nanos: i64,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            value(CypherValue::Duration(IsoDuration::new(
                months, days, seconds, nanos,
            )))
        })
    }
}

/// A POINT: one of the four `point()` srids with its dimension — 7203
/// (cartesian) and 4326 (WGS-84) with `z` NULL, 9157 (cartesian-3d) and
/// 4979 (WGS-84-3d) with `z` non-NULL. Any other pairing refuses.
///
/// # Safety
/// See [`relate_params_put_null`]; `z` null or readable.
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_point(
    p: *mut relate_params,
    name: *const c_char,
    srid: i32,
    x: f64,
    y: f64,
    z: *const f64,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            // SAFETY: `z` is null or readable per the contract.
            let point = PointValue {
                srid,
                x,
                y,
                z: z.as_ref().copied(),
            };
            if point.crs_name().is_none() {
                return Err(format!(
                    "srid {srid} with {} coordinates is not a point() coordinate system",
                    if point.is_3d() { 3 } else { 2 }
                ));
            }
            value(CypherValue::Point(point))
        })
    }
}

/// Build a VECTOR entry from checked coordinates.
fn vector(coord: VectorCoordKind, values: Vec<CypherValue>) -> Result<(Entry, usize), String> {
    let dim = values.len();
    value(CypherValue::Vector { coord, dim, values }).map(|(entry, _)| (entry, dim * 8))
}

/// A VECTOR of one of the four floating dtypes (`FLOAT16`, `BFLOAT16`,
/// `FLOAT32`, `FLOAT`) from `dim` doubles; each coordinate must be exactly
/// representable in the dtype.
///
/// # Safety
/// See [`relate_params_put_null`]; `dtype` null or NUL-terminated,
/// `values` null or `dim` readable doubles.
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_vector_float(
    p: *mut relate_params,
    name: *const c_char,
    dtype: *const c_char,
    values: *const f64,
    dim: usize,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            let coord = vector_dtype(dtype)?;
            if coord.is_integer() {
                return Err(format!(
                    "vector dtype {} takes integer coordinates (relate_params_put_vector_int)",
                    coord.canonical_name()
                ));
            }
            pre_copy_bound(dim.saturating_mul(8), "vector dimension")?;
            let mut out = Vec::with_capacity(dim);
            for (i, f) in coordinates(values, dim)?.iter().enumerate() {
                if !coord.is_exact(*f) {
                    return Err(format!(
                        "coordinate {i} ({f}) is not exactly representable as {}",
                        coord.canonical_name()
                    ));
                }
                out.push(CypherValue::Float(*f));
            }
            vector(coord, out)
        })
    }
}

/// A VECTOR of one of the four integer dtypes (`INTEGER8`, `INTEGER16`,
/// `INTEGER32`, `INTEGER`) from `dim` int64s; each coordinate must fit the
/// dtype's width.
///
/// # Safety
/// See [`relate_params_put_vector_float`]; `values` null or `dim`
/// readable int64s.
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_vector_int(
    p: *mut relate_params,
    name: *const c_char,
    dtype: *const c_char,
    values: *const i64,
    dim: usize,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe {
        put(p, name, error_json, |_| {
            let coord = vector_dtype(dtype)?;
            if !coord.is_integer() {
                return Err(format!(
                    "vector dtype {} takes floating coordinates (relate_params_put_vector_float)",
                    coord.canonical_name()
                ));
            }
            pre_copy_bound(dim.saturating_mul(8), "vector dimension")?;
            let mut out = Vec::with_capacity(dim);
            for (i, n) in coordinates(values, dim)?.iter().enumerate() {
                if !coord.holds_int(*n) {
                    return Err(format!(
                        "coordinate {i} ({n}) is out of range for {}",
                        coord.canonical_name()
                    ));
                }
                out.push(CypherValue::Int(*n));
            }
            vector(coord, out)
        })
    }
}

/// Insert a nested container and hand its builder back through `child`.
///
/// # Safety
/// See [`relate_params_put_null`]; `child` null or writable.
unsafe fn put_child(
    p: *mut relate_params,
    name: *const c_char,
    container: Container,
    child: *mut *mut relate_params,
    error_json: *mut *mut c_char,
) -> relate_status {
    // Checked beside the write it guards, not inside the build closure the
    // write leans on.
    if child.is_null() {
        // SAFETY: the out-param is null, which `report` tolerates as
        // opt-out.
        return unsafe {
            report(
                Err(boundary_error_json("null child out pointer")),
                error_json,
            )
        };
    }
    let mut minted: *mut relate_params = ptr::null_mut();
    let status = unsafe {
        put(p, name, error_json, |parent| {
            let depth = parent.depth + 1;
            if depth > MAX_VALUE_DEPTH {
                return Err(format!(
                    "parameter nesting exceeds the value depth limit of {MAX_VALUE_DEPTH}"
                ));
            }
            // The one provenance rule (see `Child`): the raw pointer
            // `Box::into_raw` hands out is the child handle C keeps AND
            // the only way this crate ever touches the child again — no
            // `Box` is reconstructed on the Rust side until `Child`'s
            // `Drop`, so moving the entry (or the parent's container)
            // re-tags nothing the handle depends on. The child shares the
            // parent's builder-wide byte budget.
            let raw = Box::into_raw(Box::new(relate_params::new(
                container,
                depth,
                Rc::clone(&parent.budget),
            )));
            minted = raw;
            // SAFETY: `raw` is a fresh non-null `Box` allocation.
            Ok((Entry::Child(Child(NonNull::new_unchecked(raw))), 0))
        })
    };
    if matches!(status, relate_status::RELATE_OK) {
        // SAFETY: `child` was checked non-null above and is writable per
        // the contract.
        unsafe { *child = minted };
    }
    status
}

/// A nested LIST: `*child` receives its builder (borrowed from the root).
///
/// # Safety
/// See [`relate_params_put_null`]; `child` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_list(
    p: *mut relate_params,
    name: *const c_char,
    child: *mut *mut relate_params,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe { put_child(p, name, Container::List(Vec::new()), child, error_json) }
}

/// A nested MAP: `*child` receives its builder (borrowed from the root).
///
/// # Safety
/// See [`relate_params_put_null`]; `child` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_params_put_map(
    p: *mut relate_params,
    name: *const c_char,
    child: *mut *mut relate_params,
    error_json: *mut *mut c_char,
) -> relate_status {
    unsafe { put_child(p, name, Container::Map(IndexMap::new()), child, error_json) }
}
