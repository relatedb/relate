//! The typed value walker (ADR-0502 §1) — the one result value path of ABI
//! v2.
//!
//! A result cell crosses as a [`relate_value`] handle. Every handle — a
//! cell's root and every child minted below it — is owned by its result and
//! freed by `relate_destroy_result`; there is no per-value free. Roots
//! are minted with the result; a value's children (list items, path
//! elements, node labels, map and property entries) are minted lazily, one
//! level at a time, on the first descent into that value.
//!
//! Discrimination is structural at every depth: [`kind_of`] maps the
//! engine's [`CypherValue`] onto [`relate_value_kind`] with one
//! exhaustive match, so a user map is `MAP`, an entity is `NODE` /
//! `RELATIONSHIP` and a nested path is `PATH` wherever they sit.
//!
//! Out-of-kind access is a defined refusal, never undefined behaviour. The
//! refusal takes the one form each return type allows:
//!
//! - pointer returns (`string`, `element_id`, `list_get`, `map_get`, …)
//!   return NULL — an in-kind call never does, and a Cypher `null` is a
//!   handle of kind `NULL`, never a NULL handle;
//! - count returns (`kind`, `list_len`, `map_len`, `vector_dim`) return -1;
//! - scalar reads return `RELATE_ERROR` from a
//!   [`relate_status`] and leave their out-params untouched.
//!
//! A NULL handle or a NULL out-param is refused the same way.

#![allow(non_camel_case_types)]

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::OnceLock;

use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};
use relate::{node_element_id, rel_element_id, Namespace};
use relate_core::temporal::zone_region;
use relate_model::{Coord, CypherValue, IndexMap, VectorCoords};

use crate::abi::relate_status;

/// The kind of a value handle (ADR-0502 §1): one enumerator per
/// [`CypherValue`] shape the engine produces on this surface, mirrored in
/// `include/relate.h` (`enum relate_value_kind`). Minimal — no
/// reserved values (ADR-0502 §5).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum relate_value_kind {
    RELATE_V_NULL = 0,
    RELATE_V_BOOL = 1,
    RELATE_V_INT = 2,
    RELATE_V_FLOAT = 3,
    RELATE_V_STRING = 4,
    RELATE_V_NODE = 5,
    RELATE_V_RELATIONSHIP = 6,
    RELATE_V_PATH = 7,
    RELATE_V_LIST = 8,
    RELATE_V_MAP = 9,
    RELATE_V_POINT = 10,
    RELATE_V_DATE = 11,
    RELATE_V_TIME = 12,
    RELATE_V_LOCAL_TIME = 13,
    RELATE_V_DATETIME = 14,
    RELATE_V_LOCAL_DATETIME = 15,
    RELATE_V_DURATION = 16,
    RELATE_V_VECTOR = 17,
}

/// The kind of an engine value — the one exhaustive match from
/// [`CypherValue`] onto the ABI's kinds (no `_ =>` arm: a new engine
/// variant does not compile until it is given a kind).
///
/// `None` only for `Geometry`, the one `CypherValue` shape this ABI carries
/// no kind for (ADR-0502 §5): the engine's `Value` has no geometry variant,
/// so `value_to_cypher` never produces one on this surface. Should one ever
/// arrive, `relate_value_kind` reports it as -1 (a kind this ABI
/// version does not carry) and every accessor refuses it.
pub(crate) fn kind_of(v: &CypherValue) -> Option<relate_value_kind> {
    use relate_value_kind::*;
    Some(match v {
        CypherValue::Null => RELATE_V_NULL,
        CypherValue::Bool(_) => RELATE_V_BOOL,
        CypherValue::Int(_) => RELATE_V_INT,
        CypherValue::Float(_) => RELATE_V_FLOAT,
        CypherValue::Str(_) => RELATE_V_STRING,
        CypherValue::List(_) => RELATE_V_LIST,
        CypherValue::Map(_) => RELATE_V_MAP,
        CypherValue::Node { .. } => RELATE_V_NODE,
        CypherValue::Relationship { .. } => RELATE_V_RELATIONSHIP,
        CypherValue::Path { .. } => RELATE_V_PATH,
        CypherValue::Date(_) => RELATE_V_DATE,
        CypherValue::Time(_, Some(_)) => RELATE_V_TIME,
        CypherValue::Time(_, None) => RELATE_V_LOCAL_TIME,
        CypherValue::DateTime(..) => RELATE_V_DATETIME,
        CypherValue::LocalDateTime(_) => RELATE_V_LOCAL_DATETIME,
        CypherValue::Duration(_) => RELATE_V_DURATION,
        CypherValue::Point(_) => RELATE_V_POINT,
        CypherValue::Vector { .. } => RELATE_V_VECTOR,
        CypherValue::Geometry(_) => return None,
    })
}

/// Where a handle's value lives.
enum Slot {
    /// A value the handle owns: a cell root, or a node label (the engine
    /// keeps labels as `String`s, so each label handle owns its `Str`).
    Owned(CypherValue),
    /// A value inside an ancestor handle's value — a list item, a map or
    /// property value, a path element. The pointer targets a heap buffer
    /// of the ancestor's value (a `Vec` or `IndexMap` slot), which the
    /// result never mutates and frees only with every handle at
    /// `relate_destroy_result`.
    Tree(*const CypherValue),
}

/// One value handle (ADR-0502 §1). Opaque to C; owned by its result.
pub struct relate_value {
    slot: Slot,
    /// The result's resolved namespace — the namespace entity element ids
    /// are spelled in (ADR-0502 §6).
    ns: Namespace,
    /// The kind's one text component, NUL-terminated: a STRING's payload,
    /// a RELATIONSHIP's type, a TIME/DATETIME zone id, a VECTOR's dtype.
    text: OnceLock<Option<Box<[u8]>>>,
    /// A NODE's / RELATIONSHIP's element ids.
    ids: OnceLock<Option<EntityIds>>,
    /// The children, minted on first descent.
    children: OnceLock<Children>,
}

/// The `elementId()` spellings of an entity (ADR-0502 §6). `None` only in
/// a namespace that mints no element ids (`system`, which has no
/// elements).
struct EntityIds {
    element: Option<CString>,
    start: Option<CString>,
    end: Option<CString>,
}

/// A value's children, one level. `items` is `Some` exactly for the
/// list-shaped kinds (LIST, PATH, NODE labels); `entries` is `Some` exactly
/// for the map-shaped kinds (MAP, NODE and RELATIONSHIP properties).
struct Children {
    items: Option<Box<[relate_value]>>,
    entries: Option<Entries>,
}

/// A map-shaped value's entries in the engine's order: the NUL-terminated
/// keys and the value handles, index for index.
struct Entries {
    keys: Box<[Box<[u8]>]>,
    values: Box<[relate_value]>,
}

/// `s` as a NUL-terminated byte buffer. Built by hand rather than as a
/// `CString` so a string carrying an interior NUL crosses whole (the
/// length out-param reports its full byte length).
fn nul_terminated(s: &str) -> Box<[u8]> {
    let mut bytes = Vec::with_capacity(s.len() + 1);
    bytes.extend_from_slice(s.as_bytes());
    bytes.push(0);
    bytes.into_boxed_slice()
}

/// The list-shaped view of a value: LIST items, PATH elements alternating
/// node/relationship, NODE labels. `None` for every other kind.
fn list_items(v: &CypherValue, ns: Namespace) -> Option<Vec<relate_value>> {
    let tree = |item: &CypherValue| relate_value::new(Slot::Tree(item), ns);
    match v {
        CypherValue::List(items) => Some(items.iter().map(tree).collect()),
        CypherValue::Path {
            nodes,
            relationships,
        } => {
            let mut elements = Vec::with_capacity(nodes.len() + relationships.len());
            for (i, node) in nodes.iter().enumerate() {
                elements.push(tree(node));
                if let Some(rel) = relationships.get(i) {
                    elements.push(tree(rel));
                }
            }
            Some(elements)
        }
        CypherValue::Node { labels, .. } => Some(
            labels
                .iter()
                .map(|l| relate_value::new(Slot::Owned(CypherValue::Str(l.clone())), ns))
                .collect(),
        ),
        _ => None,
    }
}

/// The map-shaped view of a value: MAP entries, NODE and RELATIONSHIP
/// properties. `None` for every other kind.
fn map_entries(v: &CypherValue) -> Option<&IndexMap<String, CypherValue>> {
    match v {
        CypherValue::Map(m) => Some(m),
        CypherValue::Node { properties, .. } | CypherValue::Relationship { properties, .. } => {
            Some(properties)
        }
        _ => None,
    }
}

/// The kind's one text component (see [`relate_value::text`]).
fn text_of(v: &CypherValue) -> Option<&str> {
    match v {
        CypherValue::Str(s) => Some(s),
        CypherValue::Relationship { rel_type, .. } => Some(rel_type),
        // A TIME carries only an offset: its zone id is always "".
        CypherValue::Time(_, Some(_)) => Some(""),
        // The engine's classifier answers whether the kept zone names an
        // IANA region; an offset-only zone (`Z`, `+01:00`) reads "".
        CypherValue::DateTime(_, zone) => Some(zone_region(zone.as_deref()).unwrap_or("")),
        CypherValue::Vector { coord, .. } => Some(coord.canonical_name()),
        _ => None,
    }
}

impl relate_value {
    fn new(slot: Slot, ns: Namespace) -> Self {
        relate_value {
            slot,
            ns,
            text: OnceLock::new(),
            ids: OnceLock::new(),
            children: OnceLock::new(),
        }
    }

    /// A cell's root handle, owning the cell's value.
    pub(crate) fn root(value: CypherValue, ns: Namespace) -> Self {
        Self::new(Slot::Owned(value), ns)
    }

    fn value(&self) -> &CypherValue {
        match &self.slot {
            Slot::Owned(v) => v,
            // SAFETY: a `Tree` pointer targets a heap slot of an ancestor's
            // value; the ancestor handle and its value live, unmutated,
            // until the owning result is destroyed, and every handle dies
            // with it (see `Slot::Tree`).
            Slot::Tree(p) => unsafe { &**p },
        }
    }

    fn kind(&self) -> Option<relate_value_kind> {
        kind_of(self.value())
    }

    fn text(&self) -> Option<&[u8]> {
        self.text
            .get_or_init(|| text_of(self.value()).map(nul_terminated))
            .as_deref()
    }

    fn ids(&self) -> Option<&EntityIds> {
        self.ids
            .get_or_init(|| {
                let c = |id: Option<String>| {
                    id.map(|s| CString::new(s).expect("an element id contains no interior NUL"))
                };
                match self.value() {
                    CypherValue::Node { id, .. } => Some(EntityIds {
                        element: c(node_element_id(*id, self.ns)),
                        start: None,
                        end: None,
                    }),
                    CypherValue::Relationship {
                        id,
                        start_id,
                        end_id,
                        ..
                    } => Some(EntityIds {
                        element: c(rel_element_id(*id, self.ns)),
                        start: c(node_element_id(*start_id, self.ns)),
                        end: c(node_element_id(*end_id, self.ns)),
                    }),
                    _ => None,
                }
            })
            .as_ref()
    }

    fn children(&self) -> &Children {
        self.children.get_or_init(|| {
            let v = self.value();
            Children {
                items: list_items(v, self.ns).map(Vec::into_boxed_slice),
                entries: map_entries(v).map(|m| Entries {
                    keys: m.keys().map(|k| nul_terminated(k)).collect(),
                    values: m
                        .values()
                        .map(|item| relate_value::new(Slot::Tree(item), self.ns))
                        .collect(),
                }),
            }
        })
    }
}

/// An ABI index as a `usize`; a negative index is out of range.
pub(crate) fn index(i: i64) -> Option<usize> {
    usize::try_from(i).ok()
}

/// Run an accessor on a handle under `catch_unwind`, mapping a NULL
/// handle, a refusal (`None`) or a caught panic to `None` — the one guard
/// every walker accessor shares. The caller supplies its own refusal
/// value (`unwrap_or`), so no sentinel is threaded through the accessor's
/// own `Option`.
///
/// # Safety
/// `v` must be null or a handle minted from a live result.
unsafe fn with_value<T>(
    v: *const relate_value,
    op: impl FnOnce(&relate_value) -> Option<T>,
) -> Option<T> {
    if v.is_null() {
        return None;
    }
    // SAFETY: `v` is non-null and live per this function's contract; a
    // caught panic is the `None` refusal.
    catch_unwind(AssertUnwindSafe(|| op(unsafe { &*v }))).unwrap_or_default()
}

/// Read one scalar into `out`: `RELATE_OK` with `*out` written, or
/// `RELATE_ERROR` (refusal) with `*out` untouched.
///
/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
unsafe fn read_scalar<T>(
    v: *const relate_value,
    out: *mut T,
    op: impl FnOnce(&CypherValue) -> Option<T>,
) -> relate_status {
    if out.is_null() {
        return relate_status::RELATE_ERROR;
    }
    // SAFETY: forwarded contract.
    match unsafe { with_value(v, |h| op(h.value())) } {
        Some(x) => {
            // SAFETY: `out` is non-null (checked above) and writable.
            unsafe { *out = x };
            relate_status::RELATE_OK
        }
        None => relate_status::RELATE_ERROR,
    }
}

/// A NUL-terminated buffer as a C pointer, writing its byte length (the
/// trailing NUL excluded) into `len` unless `len` is NULL.
///
/// # Safety
/// `len` must be null or writable.
unsafe fn text_ptr(bytes: &[u8], len: *mut usize) -> *const c_char {
    if !len.is_null() {
        // SAFETY: `len` is non-null and writable per the caller contract.
        unsafe { *len = bytes.len() - 1 };
    }
    bytes.as_ptr().cast()
}

/// The handle's kind as a `relate_value_kind` enumerator, or -1 for a
/// NULL handle (or a kind this ABI version does not carry, see
/// [`kind_of`]).
///
/// # Safety
/// `v` must be null or a handle minted from a live result.
#[no_mangle]
pub unsafe extern "C" fn relate_value_kind(v: *const relate_value) -> i32 {
    unsafe { with_value(v, |h| h.kind().map(|k| k as i32)).unwrap_or(-1) }
}

/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_bool(v: *const relate_value, out: *mut u8) -> relate_status {
    unsafe {
        read_scalar(v, out, |cv| match cv {
            CypherValue::Bool(b) => Some(u8::from(*b)),
            _ => None,
        })
    }
}

/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_int(v: *const relate_value, out: *mut i64) -> relate_status {
    unsafe {
        read_scalar(v, out, |cv| match cv {
            CypherValue::Int(i) => Some(*i),
            _ => None,
        })
    }
}

/// A FLOAT's value as a double.
fn float_of(cv: &CypherValue) -> Option<f64> {
    match cv {
        CypherValue::Float(f) => Some(*f),
        _ => None,
    }
}

/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_float(
    v: *const relate_value,
    out: *mut f64,
) -> relate_status {
    unsafe { read_scalar(v, out, float_of) }
}

/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_is_nan(
    v: *const relate_value,
    out: *mut u8,
) -> relate_status {
    unsafe { read_scalar(v, out, |cv| float_of(cv).map(|f| u8::from(f.is_nan()))) }
}

/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_is_inf(
    v: *const relate_value,
    out: *mut u8,
) -> relate_status {
    unsafe { read_scalar(v, out, |cv| float_of(cv).map(|f| u8::from(f.is_infinite()))) }
}

/// A STRING's payload or a RELATIONSHIP's type, NUL-terminated and
/// result-owned; `len` (nullable) receives the byte length.
///
/// # Safety
/// `v` must be null or a live handle; `len` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_string(
    v: *const relate_value,
    len: *mut usize,
) -> *const c_char {
    use relate_value_kind::{RELATE_V_RELATIONSHIP, RELATE_V_STRING};
    unsafe {
        with_value(v, |h| match h.kind()? {
            RELATE_V_STRING | RELATE_V_RELATIONSHIP => h.text().map(|t| text_ptr(t, len)),
            _ => None,
        })
        .unwrap_or(ptr::null())
    }
}

/// The element id spelled exactly as Cypher `elementId()` (ADR-0502 §6).
///
/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_element_id(v: *const relate_value) -> *const c_char {
    unsafe {
        with_value(v, |h| h.ids()?.element.as_ref().map(|s| s.as_ptr())).unwrap_or(ptr::null())
    }
}

/// `elementId(startNode(r))` of a RELATIONSHIP.
///
/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_start_element_id(v: *const relate_value) -> *const c_char {
    unsafe { with_value(v, |h| h.ids()?.start.as_ref().map(|s| s.as_ptr())).unwrap_or(ptr::null()) }
}

/// `elementId(endNode(r))` of a RELATIONSHIP.
///
/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_end_element_id(v: *const relate_value) -> *const c_char {
    unsafe { with_value(v, |h| h.ids()?.end.as_ref().map(|s| s.as_ptr())).unwrap_or(ptr::null()) }
}

/// An entity's legacy numeric id — the INTEGER Cypher `id()` returns for
/// the same entity. An entity value always carries an id, so the only
/// refusal is the wrong kind.
fn entity_id_of(cv: &CypherValue) -> Option<i64> {
    match cv {
        CypherValue::Node { id, .. } | CypherValue::Relationship { id, .. } => Some(*id),
        _ => None,
    }
}

/// `id(n)` / `id(r)` — the deprecated numeric entity id (ADR-0506),
/// exactly the INTEGER the engine's `id()` returns for the same entity.
/// Driver providers that decide node / relationship equality by numeric
/// id read it here; `WHERE elementId(n) = $id` stays on
/// [`relate_value_element_id`].
///
/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_id(v: *const relate_value, out: *mut i64) -> relate_status {
    unsafe { read_scalar(v, out, entity_id_of) }
}

/// `id(startNode(r))` — the start node's numeric id (ADR-0506).
///
/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_start_id(
    v: *const relate_value,
    out: *mut i64,
) -> relate_status {
    unsafe {
        read_scalar(v, out, |cv| match cv {
            CypherValue::Relationship { start_id, .. } => Some(*start_id),
            _ => None,
        })
    }
}

/// `id(endNode(r))` — the end node's numeric id (ADR-0506).
///
/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_end_id(
    v: *const relate_value,
    out: *mut i64,
) -> relate_status {
    unsafe {
        read_scalar(v, out, |cv| match cv {
            CypherValue::Relationship { end_id, .. } => Some(*end_id),
            _ => None,
        })
    }
}

/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_list_len(v: *const relate_value) -> i64 {
    unsafe {
        with_value(v, |h| {
            h.children().items.as_ref().map(|items| items.len() as i64)
        })
        .unwrap_or(-1)
    }
}

/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_list_get(
    v: *const relate_value,
    i: i64,
) -> *const relate_value {
    unsafe {
        with_value(v, |h| {
            let items = h.children().items.as_ref()?;
            items.get(index(i)?).map(|c| c as *const relate_value)
        })
        .unwrap_or(ptr::null())
    }
}

/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_map_len(v: *const relate_value) -> i64 {
    unsafe { with_value(v, |h| map_entries(h.value()).map(|m| m.len() as i64)).unwrap_or(-1) }
}

/// # Safety
/// `v` must be null or a live handle; `len` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_map_key(
    v: *const relate_value,
    i: i64,
    len: *mut usize,
) -> *const c_char {
    unsafe {
        with_value(v, |h| {
            let entries = h.children().entries.as_ref()?;
            entries.keys.get(index(i)?).map(|k| text_ptr(k, len))
        })
        .unwrap_or(ptr::null())
    }
}

/// # Safety
/// `v` must be null or a live handle; `key` null or a NUL-terminated
/// string.
#[no_mangle]
pub unsafe extern "C" fn relate_value_map_get(
    v: *const relate_value,
    key: *const c_char,
) -> *const relate_value {
    if key.is_null() {
        return ptr::null();
    }
    unsafe {
        with_value(v, |h| {
            // SAFETY: `key` is non-null and NUL-terminated per the contract.
            let key = CStr::from_ptr(key).to_str().ok()?;
            let at = map_entries(h.value())?.get_index_of(key)?;
            let entries = h.children().entries.as_ref()?;
            entries.values.get(at).map(|c| c as *const relate_value)
        })
        .unwrap_or(ptr::null())
    }
}

/// The calendar date of a DATE, DATETIME (local wall clock) or
/// LOCAL_DATETIME.
fn date_of(cv: &CypherValue) -> Option<NaiveDate> {
    match cv {
        CypherValue::Date(d) => Some(*d),
        CypherValue::DateTime(dt, _) => Some(dt.naive_local().date()),
        CypherValue::LocalDateTime(dt) => Some(dt.date()),
        _ => None,
    }
}

/// The wall-clock time of a TIME, LOCAL_TIME, DATETIME (local wall clock)
/// or LOCAL_DATETIME.
fn time_of(cv: &CypherValue) -> Option<NaiveTime> {
    match cv {
        CypherValue::Time(t, _) => Some(*t),
        CypherValue::DateTime(dt, _) => Some(dt.naive_local().time()),
        CypherValue::LocalDateTime(dt) => Some(dt.time()),
        _ => None,
    }
}

/// Declare one temporal component accessor: `$name(v, int64_t *out)`
/// reading `$part` off [`date_of`] or [`time_of`].
macro_rules! temporal_component {
    ($(#[$doc:meta])* $name:ident, $of:ident, $part:expr) => {
        $(#[$doc])*
        ///
        /// # Safety
        /// `v` must be null or a live handle; `out` null or writable.
        #[no_mangle]
        pub unsafe extern "C" fn $name(
            v: *const relate_value,
            out: *mut i64,
        ) -> relate_status {
            unsafe { read_scalar(v, out, |cv| $of(cv).map($part)) }
        }
    };
}

temporal_component!(
    /// The (proleptic Gregorian) year of a DATE, DATETIME or LOCAL_DATETIME.
    relate_value_year, date_of, |d: NaiveDate| i64::from(d.year())
);
temporal_component!(
    /// The month (1-12) of a DATE, DATETIME or LOCAL_DATETIME.
    relate_value_month, date_of, |d: NaiveDate| i64::from(d.month())
);
temporal_component!(
    /// The day of month (1-31) of a DATE, DATETIME or LOCAL_DATETIME.
    relate_value_day, date_of, |d: NaiveDate| i64::from(d.day())
);
temporal_component!(
    /// The hour (0-23) of a TIME, LOCAL_TIME, DATETIME or LOCAL_DATETIME.
    relate_value_hour, time_of, |t: NaiveTime| i64::from(t.hour())
);
temporal_component!(
    /// The minute (0-59) of a TIME, LOCAL_TIME, DATETIME or LOCAL_DATETIME.
    relate_value_minute, time_of, |t: NaiveTime| i64::from(t.minute())
);
temporal_component!(
    /// The second (0-59) of a TIME, LOCAL_TIME, DATETIME or LOCAL_DATETIME.
    relate_value_second, time_of, |t: NaiveTime| i64::from(t.second())
);
temporal_component!(
    /// The sub-second nanoseconds of a TIME, LOCAL_TIME, DATETIME or
    /// LOCAL_DATETIME.
    relate_value_nanosecond, time_of, |t: NaiveTime| i64::from(t.nanosecond())
);

/// The UTC offset in seconds of a TIME or DATETIME.
///
/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_offset_seconds(
    v: *const relate_value,
    out: *mut i64,
) -> relate_status {
    unsafe {
        read_scalar(v, out, |cv| match cv {
            CypherValue::Time(_, Some(offset)) => Some(i64::from(offset.local_minus_utc())),
            CypherValue::DateTime(dt, _) => Some(i64::from(dt.offset().local_minus_utc())),
            _ => None,
        })
    }
}

/// The IANA zone id of a TIME or DATETIME, `""` when offset-only.
///
/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_zone(v: *const relate_value) -> *const c_char {
    use relate_value_kind::{RELATE_V_DATETIME, RELATE_V_TIME};
    unsafe {
        with_value(v, |h| match h.kind()? {
            RELATE_V_TIME | RELATE_V_DATETIME => h.text().map(|t| t.as_ptr().cast::<c_char>()),
            _ => None,
        })
        .unwrap_or(ptr::null())
    }
}

/// A DURATION as its four lossless components.
///
/// # Safety
/// `v` must be null or a live handle; every out-param null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_duration(
    v: *const relate_value,
    months: *mut i64,
    days: *mut i64,
    seconds: *mut i64,
    nanos: *mut i64,
) -> relate_status {
    if months.is_null() || days.is_null() || seconds.is_null() || nanos.is_null() {
        return relate_status::RELATE_ERROR;
    }
    let parts = unsafe {
        with_value(v, |h| match h.value() {
            CypherValue::Duration(d) => Some((d.months, d.days, d.seconds, d.nanoseconds)),
            _ => None,
        })
    };
    let Some(parts) = parts else {
        return relate_status::RELATE_ERROR;
    };
    // SAFETY: every out-param is non-null (checked above) and writable.
    unsafe {
        (*months, *days, *seconds, *nanos) = parts;
    }
    relate_status::RELATE_OK
}

/// A POINT's srid, dimension (2 or 3) and coordinates; `z` is written only
/// for a 3D point.
///
/// # Safety
/// `v` must be null or a live handle; every out-param null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_point(
    v: *const relate_value,
    srid: *mut i32,
    dim: *mut i32,
    x: *mut f64,
    y: *mut f64,
    z: *mut f64,
) -> relate_status {
    if srid.is_null() || dim.is_null() || x.is_null() || y.is_null() || z.is_null() {
        return relate_status::RELATE_ERROR;
    }
    let point = unsafe {
        with_value(v, |h| match h.value() {
            CypherValue::Point(p) => Some((p.srid, p.x, p.y, p.z)),
            _ => None,
        })
    };
    let Some((p_srid, p_x, p_y, p_z)) = point else {
        return relate_status::RELATE_ERROR;
    };
    // SAFETY: every out-param is non-null (checked above) and writable.
    unsafe {
        *srid = p_srid;
        *x = p_x;
        *y = p_y;
        match p_z {
            Some(pz) => {
                *dim = 3;
                *z = pz;
            }
            None => *dim = 2,
        }
    }
    relate_status::RELATE_OK
}

/// A VECTOR's coordinate dtype: the engine's canonical coordinate-type
/// name (`INTEGER8`, `INTEGER16`, `INTEGER32`, `INTEGER`, `FLOAT16`,
/// `BFLOAT16`, `FLOAT32`, `FLOAT`).
///
/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_vector_dtype(v: *const relate_value) -> *const c_char {
    unsafe {
        with_value(v, |h| match h.kind()? {
            relate_value_kind::RELATE_V_VECTOR => h.text().map(|t| t.as_ptr().cast::<c_char>()),
            _ => None,
        })
        .unwrap_or(ptr::null())
    }
}

/// The packed coordinates of a VECTOR (ADR-0706).
fn vector_coords(cv: &CypherValue) -> Option<&VectorCoords> {
    match cv {
        CypherValue::Vector { coords, .. } => Some(coords),
        _ => None,
    }
}

/// # Safety
/// `v` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn relate_value_vector_dim(v: *const relate_value) -> i64 {
    unsafe { with_value(v, |h| vector_coords(h.value()).map(|xs| xs.len() as i64)).unwrap_or(-1) }
}

/// Coordinate `i` of a floating-point VECTOR.
///
/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_vector_float(
    v: *const relate_value,
    i: i64,
    out: *mut f64,
) -> relate_status {
    unsafe {
        read_scalar(v, out, |cv| {
            let i = index(i)?;
            // A float-kind vector only: the `Int` family under a float
            // kind is a legal value and widens (ADR-0706), but an
            // integer-kind vector keeps the accessor split — its
            // coordinates read through `relate_value_vector_int`.
            let CypherValue::Vector { coord, coords } = cv else {
                return None;
            };
            if coord.is_integer() || i >= coords.len() {
                return None;
            }
            Some(coords.coord_f64(i))
        })
    }
}

/// Coordinate `i` of an integer VECTOR.
///
/// # Safety
/// `v` must be null or a live handle; `out` null or writable.
#[no_mangle]
pub unsafe extern "C" fn relate_value_vector_int(
    v: *const relate_value,
    i: i64,
    out: *mut i64,
) -> relate_status {
    unsafe {
        read_scalar(v, out, |cv| {
            let coords = vector_coords(cv)?;
            let i = index(i)?;
            if i >= coords.len() {
                return None;
            }
            match coords.coord_at(i) {
                Coord::I(n) => Some(n),
                Coord::F(_) => None,
            }
        })
    }
}
