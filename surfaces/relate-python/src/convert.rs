//! Python <-> `CypherValue` mapping. The pyo3 classes live in
//! [`crate::graph_values`], [`crate::temporal_values`] and
//! [`crate::vector_values`].
//!
//! `Value` and `CypherValue` cross only through `relate::value_to_cypher`
//! and `relate::cypher_to_value`. UUIDs stay strings on this surface
//! (CIP-246). This module does not convert `Value` itself.

use crate::graph_values::{Node, Path, Point, Relationship};
use crate::temporal_values::{Duration, Temporal, TemporalParts};
use crate::vector_values::Vector;
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, TimeZone, Timelike};
use indexmap::IndexMap;
use pyo3::exceptions::{PyOverflowError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyInt, PyList, PyModule, PyString, PyTuple,
};
use relate::{node_element_id, rel_element_id, Namespace};
use relate_model::value::{IsoDuration, PointValue};
use relate_model::CypherValue;
use std::collections::{HashMap, HashSet};

// ============================================================
// CypherValue -> Python
// ============================================================

/// A string-keyed engine map — an entity's properties or a Cypher map — as a
/// Python dict, each value converted at full fidelity.
fn properties_to_py<'py>(
    py: Python<'py>,
    properties: &IndexMap<String, CypherValue>,
    ns: Namespace,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (k, v) in properties {
        dict.set_item(k, cypher_value_to_py(py, v, ns)?)?;
    }
    Ok(dict)
}

/// The one owner of the no-segment policy at this boundary: the same fact
/// the engine's `elementId()` states as a typed error — a namespace whose
/// elements have no element ids (`system`) — raised here as a Python error.
/// The sibling surfaces spell the same refusal their own way (the C ABI a
/// NULL, Bolt a typed encode error); this one follows the engine's own.
fn element_id_or_err(id: Option<String>) -> PyResult<String> {
    id.ok_or_else(|| PyRuntimeError::new_err("elements of this namespace have no element ids"))
}

/// Build a [`Node`]. `ns` is the result's resolved namespace — the one input
/// `elementId()` reads besides the id (ADR-0116 §7), so the element id is the
/// engine's own `node_element_id`, never a re-spelling.
pub(super) fn make_node(
    py: Python<'_>,
    id: i64,
    labels: &[String],
    properties: &IndexMap<String, CypherValue>,
    ns: Namespace,
) -> PyResult<Py<Node>> {
    Py::new(
        py,
        Node {
            id,
            element_id: element_id_or_err(node_element_id(id, ns))?,
            labels: labels.to_vec(),
            properties: properties_to_py(py, properties, ns)?.unbind(),
        },
    )
}

/// Build a [`Relationship`], its own and its endpoints' element ids from the
/// engine's helpers (see [`make_node`]).
pub(super) fn make_rel(
    py: Python<'_>,
    id: i64,
    start_id: i64,
    end_id: i64,
    rel_type: &str,
    properties: &IndexMap<String, CypherValue>,
    ns: Namespace,
) -> PyResult<Py<Relationship>> {
    Py::new(
        py,
        Relationship {
            id,
            element_id: element_id_or_err(rel_element_id(id, ns))?,
            rel_type: rel_type.to_string(),
            start_node: start_id,
            end_node: end_id,
            start_node_element_id: element_id_or_err(node_element_id(start_id, ns))?,
            end_node_element_id: element_id_or_err(node_element_id(end_id, ns))?,
            properties: properties_to_py(py, properties, ns)?.unbind(),
        },
    )
}

pub(super) fn node_to_pynode(py: Python<'_>, v: &CypherValue, ns: Namespace) -> PyResult<Py<Node>> {
    match v {
        CypherValue::Node {
            id,
            labels,
            properties,
        } => make_node(py, *id, labels, properties, ns),
        _ => Err(PyRuntimeError::new_err(
            "path element expected to be a node was not a node",
        )),
    }
}

pub(super) fn rel_to_pyrel(
    py: Python<'_>,
    v: &CypherValue,
    ns: Namespace,
) -> PyResult<Py<Relationship>> {
    match v {
        CypherValue::Relationship {
            id,
            start_id,
            end_id,
            rel_type,
            properties,
        } => make_rel(py, *id, *start_id, *end_id, rel_type, properties, ns),
        _ => Err(PyRuntimeError::new_err(
            "path element expected to be a relationship was not a relationship",
        )),
    }
}

/// Build a fixed-offset `datetime.timezone` (abi3-safe: goes through the Python
/// `datetime` module rather than the datetime C API).
pub(super) fn py_fixed_tz<'py>(
    dtmod: &Bound<'py, PyModule>,
    offset_secs: i32,
) -> PyResult<Bound<'py, PyAny>> {
    let td = dtmod.call_method1("timedelta", (0i32, offset_secs))?;
    dtmod.call_method1("timezone", (td,))
}

pub(super) fn date_to_py(py: Python<'_>, d: &NaiveDate) -> PyResult<PyObject> {
    let dtmod = py.import("datetime")?;
    Ok(dtmod
        .call_method1("date", (d.year(), d.month(), d.day()))?
        .unbind())
}

pub(super) fn time_to_py(
    py: Python<'_>,
    t: &NaiveTime,
    off: &Option<FixedOffset>,
) -> PyResult<PyObject> {
    let dtmod = py.import("datetime")?;
    let micro = (t.nanosecond() / 1000).min(999_999);
    let tzinfo: PyObject = match off {
        Some(o) => py_fixed_tz(&dtmod, o.local_minus_utc())?.unbind(),
        None => py.None(),
    };
    Ok(dtmod
        .call_method1("time", (t.hour(), t.minute(), t.second(), micro, tzinfo))?
        .unbind())
}

pub(super) fn datetime_to_py(py: Python<'_>, dt: &DateTime<FixedOffset>) -> PyResult<PyObject> {
    let dtmod = py.import("datetime")?;
    let tz = py_fixed_tz(&dtmod, dt.offset().local_minus_utc())?.unbind();
    let micro = (dt.nanosecond() / 1000).min(999_999);
    Ok(dtmod
        .call_method1(
            "datetime",
            (
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second(),
                micro,
                tz,
            ),
        )?
        .unbind())
}

pub(super) fn localdatetime_to_py(
    py: Python<'_>,
    ndt: &chrono::NaiveDateTime,
) -> PyResult<PyObject> {
    let dtmod = py.import("datetime")?;
    let micro = (ndt.nanosecond() / 1000).min(999_999);
    Ok(dtmod
        .call_method1(
            "datetime",
            (
                ndt.year(),
                ndt.month(),
                ndt.day(),
                ndt.hour(),
                ndt.minute(),
                ndt.second(),
                micro,
            ),
        )?
        .unbind())
}

/// Whether a stdlib temporal carries the value without loss: `datetime` and
/// `time` hold whole microseconds, and a stdlib `datetime.datetime` has a
/// fixed offset but no place for an IANA zone name. The zone half is the
/// engine's own classifier — `zone_region` answers `Some` only for an IANA
/// region, so the offset-only spellings the engine also stores (`Z`, `UTC`,
/// `+01:00`, and nothing at all) stay lossless, exactly as the engine's
/// renderer treats them. The decision is read off the value's own fields,
/// never off its rendered text.
fn stdlib_lossless(nanos: u32, zone: Option<&str>) -> bool {
    nanos % 1_000 == 0 && relate_core::temporal::zone_region(zone).is_none()
}

/// The engine's canonical ISO-8601 text for a temporal value, plus the
/// engine's kind name for it. Both come from the engine's one renderer
/// (`Temporal::iso_string`, ADR-0448) through the inverse bridge, so the
/// binding owns no second copy of the format: the zone-suffix classifier, the
/// whole-minute seconds omission and the trimmed nanosecond digits all stay in
/// the engine.
fn temporal_render(val: &CypherValue) -> PyResult<(String, &'static str)> {
    match relate::cypher_to_value(val) {
        relate::Value::Temporal(temporal) => Ok((temporal.iso_string(), temporal.kind_name())),
        _ => Err(PyRuntimeError::new_err(
            "temporal value expected to convert back to a temporal engine value",
        )),
    }
}

/// Build the rich [`Temporal`] that carries the full fidelity of `val`.
fn rich_temporal(py: Python<'_>, parts: TemporalParts, val: &CypherValue) -> PyResult<PyObject> {
    let (text, kind) = temporal_render(val)?;
    Ok(Py::new(py, Temporal::new(parts, kind, text))?.into_any())
}

/// Convert a temporal [`CypherValue`]: its stdlib counterpart when that is
/// lossless, [`Temporal`] when it is not.
fn temporal_to_py(py: Python<'_>, val: &CypherValue) -> PyResult<PyObject> {
    match val {
        CypherValue::Date(d) => date_to_py(py, d),
        CypherValue::Time(t, off) => {
            if stdlib_lossless(t.nanosecond(), None) {
                time_to_py(py, t, off)
            } else {
                rich_temporal(
                    py,
                    TemporalParts::Time {
                        time: *t,
                        offset: *off,
                    },
                    val,
                )
            }
        }
        CypherValue::DateTime(dt, zone) => {
            if stdlib_lossless(dt.nanosecond(), zone.as_deref()) {
                datetime_to_py(py, dt)
            } else {
                rich_temporal(
                    py,
                    TemporalParts::DateTime {
                        local: *dt,
                        zone: zone.clone(),
                    },
                    val,
                )
            }
        }
        CypherValue::LocalDateTime(ndt) => {
            if stdlib_lossless(ndt.nanosecond(), None) {
                localdatetime_to_py(py, ndt)
            } else {
                rich_temporal(py, TemporalParts::LocalDateTime(*ndt), val)
            }
        }
        CypherValue::Duration(dur) => duration_to_py(py, dur),
        _ => Err(PyRuntimeError::new_err(
            "temporal conversion reached a non-temporal value",
        )),
    }
}

/// A duration result: always the lossless [`Duration`] (ADR-0502 §1) —
/// never a `timedelta`, which would merge `PT36H` into `P1DT12H`, drop
/// months and floor nanoseconds.
pub(super) fn duration_to_py(py: Python<'_>, dur: &IsoDuration) -> PyResult<PyObject> {
    Ok(Py::new(py, Duration { value: dur.clone() })?.into_any())
}

/// Convert the engine-native notification list (ADR-0363 decision 9) to
/// Python dicts — the shape `QueryResult.notifications` carries. The key
/// set (`code`, `gql_status`, `severity`, `category`, `title`, `description`,
/// `remedy`, and `operator_id` on operator-scoped kinds) comes from the
/// shared `notifications_to_wire_json` encoder, so this module never
/// re-spells it.
pub(super) fn notifications_to_py(
    py: Python<'_>,
    notifications: &[relate::Notification],
) -> PyResult<Vec<Py<PyDict>>> {
    let wire = relate_model::wire::notifications_to_wire_json(notifications);
    json_to_py(py, &wire)?.bind(py).extract()
}

/// Convert a `serde_json::Value` (from the canonical wire encoder) to a Python
/// object. Used for geometries, whose self-describing wire shape is the Python
/// representation, and for the notification and plan documents.
pub(super) fn json_to_py(py: Python<'_>, v: &serde_json::Value) -> PyResult<PyObject> {
    use serde_json::Value as J;
    match v {
        J::Null => Ok(py.None()),
        J::Bool(b) => Ok(b.into_pyobject(py).unwrap().to_owned().into_any().unbind()),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py).unwrap().into_any().unbind())
            } else if let Some(u) = n.as_u64() {
                Ok(u.into_pyobject(py).unwrap().into_any().unbind())
            } else {
                Ok(n.as_f64()
                    .unwrap_or(f64::NAN)
                    .into_pyobject(py)
                    .unwrap()
                    .into_any()
                    .unbind())
            }
        }
        J::String(s) => Ok(s.into_pyobject(py).unwrap().into_any().unbind()),
        J::Array(a) => {
            let mut items = Vec::with_capacity(a.len());
            for e in a {
                items.push(json_to_py(py, e)?);
            }
            Ok(PyList::new(py, &items)?.into_any().unbind())
        }
        J::Object(o) => {
            let dict = PyDict::new(py);
            for (k, val) in o {
                dict.set_item(k, json_to_py(py, val)?)?;
            }
            Ok(dict.into_any().unbind())
        }
    }
}

/// Convert one engine value to its Python object, at every depth. `ns` is the
/// result's resolved namespace, which entity element ids are spelled in.
pub(super) fn cypher_value_to_py(
    py: Python<'_>,
    val: &CypherValue,
    ns: Namespace,
) -> PyResult<PyObject> {
    match val {
        CypherValue::Null => Ok(py.None()),
        CypherValue::Bool(b) => Ok(b.into_pyobject(py).unwrap().to_owned().into_any().unbind()),
        CypherValue::Int(i) => Ok(i.into_pyobject(py).unwrap().into_any().unbind()),
        CypherValue::Float(f) => Ok(f.into_pyobject(py).unwrap().into_any().unbind()),
        CypherValue::Str(s) => Ok(s.into_pyobject(py).unwrap().into_any().unbind()),
        CypherValue::List(items) => {
            let mut py_items = Vec::with_capacity(items.len());
            for v in items {
                py_items.push(cypher_value_to_py(py, v, ns)?);
            }
            Ok(PyList::new(py, &py_items)?.into_any().unbind())
        }
        CypherValue::Map(map) => Ok(properties_to_py(py, map, ns)?.into_any().unbind()),
        CypherValue::Node {
            id,
            labels,
            properties,
        } => Ok(make_node(py, *id, labels, properties, ns)?.into_any()),
        CypherValue::Relationship {
            id,
            start_id,
            end_id,
            rel_type,
            properties,
        } => Ok(make_rel(py, *id, *start_id, *end_id, rel_type, properties, ns)?.into_any()),
        CypherValue::Path {
            nodes,
            relationships,
        } => {
            let mut nodes_py = Vec::with_capacity(nodes.len());
            for n in nodes {
                nodes_py.push(node_to_pynode(py, n, ns)?);
            }
            let mut rs = Vec::with_capacity(relationships.len());
            for r in relationships {
                rs.push(rel_to_pyrel(py, r, ns)?);
            }
            Ok(Py::new(
                py,
                Path {
                    nodes: nodes_py,
                    relationships: rs,
                },
            )?
            .into_any())
        }
        CypherValue::Date(_)
        | CypherValue::Time(..)
        | CypherValue::DateTime(..)
        | CypherValue::LocalDateTime(_)
        | CypherValue::Duration(_) => temporal_to_py(py, val),
        // Point::new is the one construction site — it computes `crs` from
        // the canonical wire mapping.
        CypherValue::Point(p) => Ok(Py::new(py, Point::new(p.srid, p.x, p.y, p.z))?.into_any()),
        // The one construction site: `Vector::from_engine` selects the
        // coordinate family from the kind (private fields — a Vector can
        // never carry a kind/payload mismatch).
        CypherValue::Vector { coord, values, .. } => {
            Ok(Py::new(py, Vector::from_engine(*coord, values)?)?.into_any())
        }
        // Geometries surface as their canonical, self-describing wire shape
        // (a plain dict), reusing the shared encoder.
        CypherValue::Geometry(_) => json_to_py(py, &relate_model::wire::value_to_wire_json(val)),
    }
}

// ============================================================
// Python -> CypherValue (parameters)
// ============================================================

/// The Python type name of `obj` for error messages ("unknown" if even that
/// fails).
pub(super) fn py_type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type()
        .name()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Validate and convert the `params` argument of `execute` /
/// `execute_arrow` (ADR-0219): it must be a dict with string keys. Errors
/// are worded for DB-API callers (the Python layer maps `TypeError` to
/// `ProgrammingError`), not in pyo3's internal conversion terms.
pub(super) fn py_params_to_cypher(
    obj: &Bound<'_, PyAny>,
) -> PyResult<HashMap<String, CypherValue>> {
    let dict = obj.downcast::<PyDict>().map_err(|_| {
        PyTypeError::new_err(format!(
            "query parameters must be a dict mapping string keys to values, got '{}'",
            py_type_name(obj)
        ))
    })?;
    let mut params = HashMap::new();
    for (key, value) in dict.iter() {
        let k: String = key.extract().map_err(|_| {
            PyTypeError::new_err(format!(
                "query parameter keys must be strings, got a key of type '{}'",
                py_type_name(&key)
            ))
        })?;
        let v = py_to_cypher_value(&value)?;
        params.insert(k, v);
    }
    Ok(params)
}

/// Recognize a Python `datetime` / `date` / `time` / `timedelta` and convert it
/// to the matching `CypherValue`. Returns `Ok(None)` for anything else.
pub(super) fn try_temporal(obj: &Bound<'_, PyAny>) -> PyResult<Option<CypherValue>> {
    let py = obj.py();
    let dtmod = py.import("datetime")?;
    let datetime_cls = dtmod.getattr("datetime")?;
    let date_cls = dtmod.getattr("date")?;
    let time_cls = dtmod.getattr("time")?;
    let timedelta_cls = dtmod.getattr("timedelta")?;

    // `datetime` is a subclass of `date`, so test it first.
    if obj.is_instance(&datetime_cls)? {
        return Ok(Some(py_datetime_to_cypher(obj)?));
    }
    if obj.is_instance(&date_cls)? {
        let y: i32 = obj.getattr("year")?.extract()?;
        let m: u32 = obj.getattr("month")?.extract()?;
        let d: u32 = obj.getattr("day")?.extract()?;
        let date = NaiveDate::from_ymd_opt(y, m, d)
            .ok_or_else(|| PyTypeError::new_err("date parameter out of range"))?;
        return Ok(Some(CypherValue::Date(date)));
    }
    if obj.is_instance(&time_cls)? {
        return Ok(Some(py_time_to_cypher(obj)?));
    }
    if obj.is_instance(&timedelta_cls)? {
        let days = timedelta_component(obj, "days")?;
        let seconds = timedelta_component(obj, "seconds")?;
        let micros = timedelta_component(obj, "microseconds")?;
        // A subclass can shadow `microseconds` with any i64; the nanosecond
        // scale-up is checked so an out-of-range value raises the same typed
        // error instead of wrapping in release (or panicking in debug).
        let nanos = micros
            .checked_mul(1000)
            .ok_or_else(|| PyOverflowError::new_err("timedelta.microseconds out of range"))?;
        return Ok(Some(CypherValue::Duration(IsoDuration::new(
            0, days, seconds, nanos,
        ))));
    }
    Ok(None)
}

/// Extract one `timedelta` component as `i64`. CPython bounds a stdlib
/// `timedelta`, but a subclass can shadow `days`/`seconds`/`microseconds`, and
/// pyo3's own out-of-range message names the C width instead of the parameter.
fn timedelta_component(obj: &Bound<'_, PyAny>, field: &str) -> PyResult<i64> {
    let value = obj.getattr(field)?;
    let int = value.downcast::<PyInt>().map_err(|_| {
        PyTypeError::new_err(format!(
            "timedelta parameter field '{field}' must be an integer, got '{}'",
            py_type_name(&value)
        ))
    })?;
    int.extract::<i64>().map_err(|_| {
        PyOverflowError::new_err(format!(
            "timedelta parameter field '{field}' is out of range for a 64-bit Relate duration"
        ))
    })
}

/// The UTC offset of an aware `datetime`/`time` as the domain value (a
/// chrono `FixedOffset`), or `None` if the object is naive. `base` names the
/// stdlib class (`"datetime"` / `"time"`) whose `utcoffset` is called — the
/// same subclass-shadowing hardening as [`timedelta_component`]: a subclass
/// overriding `utcoffset()` (or a tzinfo returning a `timedelta` subclass
/// that shadows `days`/`seconds`) cannot feed an arbitrary value in. The
/// stdlib method validates the tzinfo's answer (a `timedelta` strictly
/// within ±24h), the components are read through the base `timedelta`
/// descriptors, and the arithmetic is checked so no value wraps into a wrong
/// offset — an out-of-range offset is this function's `{base}`-worded error,
/// never a range check a caller repeats.
fn py_utc_offset(obj: &Bound<'_, PyAny>, base: &str) -> PyResult<Option<FixedOffset>> {
    let dtmod = obj.py().import("datetime")?;
    let off = dtmod.getattr(base)?.getattr("utcoffset")?.call1((obj,))?;
    if off.is_none() {
        return Ok(None);
    }
    let timedelta_cls = dtmod.getattr("timedelta")?;
    let component = |field: &str| -> PyResult<i64> {
        timedelta_cls
            .getattr(field)?
            .call_method1("__get__", (&off,))?
            .extract()
    };
    let out_of_range = || PyOverflowError::new_err(format!("{base} UTC offset out of range"));
    let (days, seconds) = (component("days")?, component("seconds")?);
    let secs = days
        .checked_mul(86_400)
        .and_then(|d| d.checked_add(seconds))
        .ok_or_else(out_of_range)?;
    let offset = i32::try_from(secs)
        .ok()
        .and_then(FixedOffset::east_opt)
        .ok_or_else(out_of_range)?;
    Ok(Some(offset))
}

pub(super) fn py_datetime_to_cypher(obj: &Bound<'_, PyAny>) -> PyResult<CypherValue> {
    let y: i32 = obj.getattr("year")?.extract()?;
    let mo: u32 = obj.getattr("month")?.extract()?;
    let d: u32 = obj.getattr("day")?.extract()?;
    let h: u32 = obj.getattr("hour")?.extract()?;
    let mi: u32 = obj.getattr("minute")?.extract()?;
    let s: u32 = obj.getattr("second")?.extract()?;
    let micro: u32 = obj.getattr("microsecond")?.extract()?;
    let ndt = NaiveDate::from_ymd_opt(y, mo, d)
        .and_then(|date| date.and_hms_micro_opt(h, mi, s, micro))
        .ok_or_else(|| PyTypeError::new_err("datetime parameter out of range"))?;
    match py_utc_offset(obj, "datetime")? {
        None => Ok(CypherValue::LocalDateTime(ndt)),
        Some(offset) => {
            let dt = offset
                .from_local_datetime(&ndt)
                .single()
                .ok_or_else(|| PyTypeError::new_err("ambiguous datetime parameter"))?;
            Ok(CypherValue::DateTime(dt, None))
        }
    }
}

pub(super) fn py_time_to_cypher(obj: &Bound<'_, PyAny>) -> PyResult<CypherValue> {
    let h: u32 = obj.getattr("hour")?.extract()?;
    let mi: u32 = obj.getattr("minute")?.extract()?;
    let s: u32 = obj.getattr("second")?.extract()?;
    let micro: u32 = obj.getattr("microsecond")?.extract()?;
    let nt = NaiveTime::from_hms_micro_opt(h, mi, s, micro)
        .ok_or_else(|| PyTypeError::new_err("time parameter out of range"))?;
    Ok(CypherValue::Time(nt, py_utc_offset(obj, "time")?))
}

/// The `params` argument of `execute` / `execute_arrow`: `None` means no
/// parameters; anything else must be a valid dict (ADR-0219).
pub(super) fn optional_params_to_cypher(
    params: Option<&Bound<'_, PyAny>>,
) -> PyResult<HashMap<String, CypherValue>> {
    match params {
        Some(obj) => py_params_to_cypher(obj),
        None => Ok(HashMap::new()),
    }
}

/// Recognize a `uuid.UUID` (by `isinstance` against the stdlib `uuid`
/// module) and render it as its canonical lowercase hyphenated string
/// (CIP-246: UUIDs stay strings on this surface; ADR-0219). Returns
/// `Ok(None)` for anything else.
pub(super) fn try_uuid(obj: &Bound<'_, PyAny>) -> PyResult<Option<CypherValue>> {
    let uuid_cls = obj.py().import("uuid")?.getattr("UUID")?;
    if obj.is_instance(&uuid_cls)? {
        let s: String = obj.str()?.extract()?;
        return Ok(Some(CypherValue::Str(s)));
    }
    Ok(None)
}

/// bytes/bytearray cross as a list of byte values 0-255 — the only
/// lossless representation while the engine has no binary scalar
/// (ADR-0219). PEP 249's `Binary` constructor *is* `bytes`.
fn bytes_to_int_list(bytes: &[u8]) -> CypherValue {
    CypherValue::List(
        bytes
            .iter()
            .map(|byte| CypherValue::Int(i64::from(*byte)))
            .collect(),
    )
}

/// The shared list/tuple arm: convert each element of a Python sequence
/// (already downcast by the caller) into a `LIST`, tracking the container
/// for cycle detection.
fn py_seq_to_list<'py>(
    obj: &Bound<'py, PyAny>,
    items: impl Iterator<Item = Bound<'py, PyAny>>,
    depth: usize,
    visiting: &mut HashSet<usize>,
) -> PyResult<CypherValue> {
    enter_container(obj, visiting)?;
    let mut converted = Vec::with_capacity(items.size_hint().0);
    for item in items {
        converted.push(py_to_cypher_value_at(&item, depth + 1, visiting)?);
    }
    leave_container(obj, visiting);
    Ok(CypherValue::List(converted))
}

/// Maximum list/dict nesting depth accepted when converting parameters and when
/// copying properties for `data()` / `to_dict()`. Deeper
/// structures raise rather than recursing until the Rust stack overflows and
/// aborts the host interpreter (LABS-114).
pub(super) const PY_VALUE_DEPTH_CAP: usize = 128;

/// Convert a Python parameter object to a [`CypherValue`], rejecting nesting
/// deeper than [`PY_VALUE_DEPTH_CAP`] and self-referential lists/maps.
pub(super) fn py_to_cypher_value(obj: &Bound<'_, PyAny>) -> PyResult<CypherValue> {
    // `visiting` holds the `PyObject*` of every container on the current
    // recursion path, so a list/dict that (transitively) contains itself is
    // rejected instead of recursing forever.
    let mut visiting = HashSet::new();
    py_to_cypher_value_at(obj, 0, &mut visiting)
}

pub(super) fn py_to_cypher_value_at(
    obj: &Bound<'_, PyAny>,
    depth: usize,
    visiting: &mut HashSet<usize>,
) -> PyResult<CypherValue> {
    if depth > PY_VALUE_DEPTH_CAP {
        return Err(PyValueError::new_err(format!(
            "parameter nesting exceeds the maximum depth of {PY_VALUE_DEPTH_CAP}"
        )));
    }
    if obj.is_none() {
        return Ok(CypherValue::Null);
    }
    if let Ok(b) = obj.downcast::<PyBool>() {
        return Ok(CypherValue::Bool(b.is_true()));
    }
    if let Ok(i) = obj.downcast::<PyInt>() {
        let v = i.extract::<i64>().map_err(|_| {
            PyOverflowError::new_err(
                "integer parameter is out of range for a 64-bit Relate integer",
            )
        })?;
        return Ok(CypherValue::Int(v));
    }
    if let Ok(f) = obj.downcast::<PyFloat>() {
        return Ok(CypherValue::Float(f.extract::<f64>()?));
    }
    if let Ok(s) = obj.downcast::<PyString>() {
        return Ok(CypherValue::Str(s.to_string()));
    }
    if let Some(temporal) = try_temporal(obj)? {
        return Ok(temporal);
    }
    if let Ok(dur) = obj.downcast::<Duration>() {
        return Ok(CypherValue::Duration(dur.get().value.clone()));
    }
    if let Ok(vector) = obj.downcast::<Vector>() {
        return Ok(vector.get().to_cypher());
    }
    if let Ok(pt) = obj.downcast::<Point>() {
        let p = pt.borrow();
        return Ok(CypherValue::Point(PointValue {
            srid: p.srid,
            x: p.x,
            y: p.y,
            z: p.z,
        }));
    }
    if let Some(uuid) = try_uuid(obj)? {
        return Ok(uuid);
    }
    if let Ok(b) = obj.downcast::<PyBytes>() {
        return Ok(bytes_to_int_list(b.as_bytes()));
    }
    if let Ok(ba) = obj.downcast::<PyByteArray>() {
        // SAFETY: no Python code runs while the bytes are read.
        return Ok(bytes_to_int_list(unsafe { ba.as_bytes() }));
    }
    if let Ok(list) = obj.downcast::<PyList>() {
        return py_seq_to_list(obj, list.iter(), depth, visiting);
    }
    // Tuples convert exactly like lists (ADR-0219).
    if let Ok(tuple) = obj.downcast::<PyTuple>() {
        return py_seq_to_list(obj, tuple.iter(), depth, visiting);
    }
    if let Ok(dict) = obj.downcast::<PyDict>() {
        // Dicts are always maps (ADR-0219): points are made only from an
        // explicit `relate.Point` (above) or in Cypher via `point()`.
        enter_container(obj, visiting)?;
        let mut map = IndexMap::new();
        for (k, v) in dict.iter() {
            let key: String = k.extract().map_err(|_| {
                PyTypeError::new_err(format!(
                    "map keys must be strings, got a key of type '{}'",
                    py_type_name(&k)
                ))
            })?;
            let value = py_to_cypher_value_at(&v, depth + 1, visiting)?;
            map.insert(key, value);
        }
        leave_container(obj, visiting);
        return Ok(CypherValue::Map(map));
    }

    Err(PyTypeError::new_err(format!(
        "Relate cannot convert a parameter of Python type '{}' to a graph value",
        py_type_name(obj)
    )))
}

/// Record `obj` as being on the current recursion path. Errors if it is already
/// present, i.e. the structure references itself.
pub(super) fn enter_container(
    obj: &Bound<'_, PyAny>,
    visiting: &mut HashSet<usize>,
) -> PyResult<()> {
    if !visiting.insert(obj.as_ptr() as usize) {
        return Err(PyValueError::new_err(
            "value contains a cycle: a list or map references itself",
        ));
    }
    Ok(())
}

/// Remove `obj` from the current recursion path once its children are handled.
pub(super) fn leave_container(obj: &Bound<'_, PyAny>, visiting: &mut HashSet<usize>) {
    visiting.remove(&(obj.as_ptr() as usize));
}
