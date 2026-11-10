//! Conversion builtins: `toString`/`toInteger`/`toFloat`/`toBoolean` and
//! their `OrNull` variants, the `toStringList`/`toFloatList`/
//! `toIntegerList` list conversions, plus the shared coercion helpers.
//! CIP-271's *static* `LIST<T>` rejection lives in the front's validation
//! layer (`relate_front::validate::static_types`); this module owns only
//! the value-level conversion policy. String → number parsing lives in the
//! [`string`] submodule (ADR-0175).

use std::borrow::Cow;
use std::sync::Arc;

use relate_core::{Error, ErrorKind, Result, Value};

use crate::ctx::is_cypher5;
use crate::eval::numeric::float_to_i64;
use crate::overlay::ExecGraph;

use string::{float_from_string, integer_from_string, IntegerString};

mod string;

pub(super) fn bi_tostring(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    // The row's `ARITY_EXACTLY_ONE` contract rejects a second argument
    // before any argument is evaluated (LABS-785).
    to_string_value(vals.first(), cypher_version)
}

pub(super) fn bi_tostringornull(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    Ok(to_string_value(vals.first(), cypher_version).unwrap_or(Value::Null))
}
pub(super) fn bi_toint(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    to_integer_value(vals.first())
}

pub(super) fn bi_tointegerornull(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(to_integer_value(vals.first()).unwrap_or(Value::Null))
}

pub(super) fn bi_tointegerlist(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    convert_list(&vals, ListProjection::Integer)
}

pub(super) fn bi_tofloat(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    to_float_value(vals.first())
}

pub(super) fn bi_tofloatornull(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(to_float_value(vals.first()).unwrap_or(Value::Null))
}

pub(super) fn bi_tofloatlist(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    convert_list(&vals, ListProjection::Float)
}

pub(super) fn bi_tostringlist(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    convert_list(&vals, ListProjection::String { cypher_version })
}

/// One `toStringList()` element. An element is converted with the
/// `toString()` scalar policy, except that the dialect decides what a
/// container, a graph value and null become rather than raising: in Cypher
/// 25 they render in display form (`null` included, matching TCK
/// TypeConversion4 [10]); under an explicit `CYPHER 5` preamble they become
/// null elements (TypeConversion4 [9]). A byte array is not a
/// stringifiable element in either dialect.
fn to_string_element(v: &Value, cypher_version: Option<u32>) -> Value {
    let cypher5 = is_cypher5(cypher_version);
    match v {
        Value::Null if cypher5 => Value::Null,
        Value::Null => Value::String("null".into()),
        Value::Bytes(_) => Value::Null,
        Value::List(_)
        | Value::Map(_)
        | Value::Node(_)
        | Value::Relationship(_)
        | Value::Path(_) => {
            if cypher5 {
                Value::Null
            } else {
                Value::String(v.to_display_string().into())
            }
        }
        Value::Vector { .. } => Value::String(v.to_cypher_literal().into()),
        Value::String(s) => Value::String(Arc::clone(s)),
        Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_) => Value::String(super::render_accepted_scalar(v)),
    }
}
pub(super) fn bi_tobool(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    to_boolean_value(vals.first())
}

pub(super) fn bi_tobooleanornull(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(to_boolean_value(vals.first()).unwrap_or(Value::Null))
}

/// ADR-0497's `toBooleanList` add: the element-wise form of `toBoolean()`
/// (unconvertible elements become null, never an error).
pub(super) fn bi_tobooleanlist(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    convert_list(&vals, ListProjection::Boolean)
}

/// The element projection a list conversion applies: `toIntegerList()`
/// truncates to integers, `toFloatList()` widens to floats,
/// `toBooleanList()` maps each element with the `toBoolean` rules, and
/// `toStringList()` renders each element with the scalar `toString()`
/// policy whose dialect `cypher_version` selects.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum ListProjection {
    Integer,
    Float,
    Boolean,
    String { cypher_version: Option<u32> },
}

/// Shared body of `toIntegerList()`/`toFloatList()`/`toBooleanList()`/
/// `toStringList()`: convert a list or vector's elements per `projection`. A
/// VECTOR's coordinates are first normalised to FLOAT elements, so they
/// always convert through FLOAT, as in Neo4j (ADR-0168); a list's elements
/// are projected as they are by [`convert_element`].
fn convert_list(vals: &[Value], projection: ListProjection) -> Result<Value> {
    let Some(value) = vals.first() else {
        return Ok(Value::Null);
    };
    if value.is_null() {
        return Ok(Value::Null);
    }
    let coerce_float = |v: &Value| v.as_number().map(Value::Float).unwrap_or(Value::Null);
    let items: Cow<'_, [Value]> = match value {
        Value::List(xs) => Cow::Borrowed(xs.as_slice()),
        Value::Vector { values, .. } => Cow::Owned(values.iter().map(coerce_float).collect()),
        _ => return Err(Error::ty("list conversion requires a list or vector")),
    };
    let converted = items
        .iter()
        .map(|v| convert_element(v, projection))
        .collect::<Vec<_>>();
    Ok(Value::List(converted.into()))
}

/// One list element of `toIntegerList()`/`toFloatList()`/`toBooleanList()`/
/// `toStringList()`, converted with the scalar `OrNull` rules (ADR-0175,
/// mirroring Neo4j's `CypherFunctions`, which routes every element through
/// the `OrNull` paths): null stays null; a FLOAT element converts per
/// ADR-0168 ([`float_to_i64`]: NaN → 0, saturate, truncate toward zero); a
/// STRING element parses with the same string parser as
/// `toIntegerOrNull()`/`toFloatOrNull()`
/// ([`integer_from_string`]/[`float_from_string`]) — a string that fails to
/// parse, including an out-of-range numeric string, becomes a NULL element
/// and never raises; a BOOLEAN element converts to 1/0 in `toIntegerList()`
/// and to null in `toFloatList()`; `toBooleanList()` applies the
/// `toBooleanOrNull()` element policy (a boolean or integer converts, a
/// `'true'`/`'false'` string converts, anything else becomes null); every
/// other element type coerces to null. `toStringList()` keeps the
/// `toString()` element policy instead ([`to_string_element`]: containers
/// and null render per dialect, a vector in its literal form).
fn convert_element(v: &Value, projection: ListProjection) -> Value {
    match (projection, v) {
        (ListProjection::String { cypher_version }, v) => to_string_element(v, cypher_version),
        (ListProjection::Boolean, v) => to_boolean_value(Some(v)).unwrap_or(Value::Null),
        (_, Value::Null) => Value::Null,
        (ListProjection::Integer, Value::Int(i)) => Value::Int(*i),
        (ListProjection::Integer, Value::Float(f)) => Value::Int(float_to_i64(*f)),
        (ListProjection::Integer, Value::Bool(b)) => Value::Int(i64::from(*b)),
        (ListProjection::Integer, Value::String(s)) => match integer_from_string(s) {
            IntegerString::InRange(i) => Value::Int(i),
            IntegerString::OutOfRange | IntegerString::NotANumber => Value::Null,
        },
        (ListProjection::Float, Value::Float(f)) => Value::Float(*f),
        (ListProjection::Float, Value::Int(i)) => Value::Float(*i as f64),
        (ListProjection::Float, Value::String(s)) => {
            float_from_string(s).map_or(Value::Null, Value::Float)
        }
        _ => Value::Null,
    }
}

/// The one `toString()` value-level type error — raised when a value's type
/// is not stringifiable in the statement's dialect.
fn tostring_type_error() -> Error {
    Error::ty("InvalidArgumentValue: toString()")
}

/// Shared body of `toString()`/`toStringOrNull()`: convert an optional
/// value to its string form. Null and absent input yield `Value::Null`;
/// only a non-convertible *type* errors, which the `OrNull` variant maps to
/// null. `cypher_version` selects the dialect: CIP-271 (containers render
/// in display form) is Cypher-25-only, while an explicit `CYPHER 5`
/// preamble keeps the pre-CIP-271 contract (CIP-150) in which every
/// container is non-stringifiable.
fn to_string_value(v: Option<&Value>, cypher_version: Option<u32>) -> Result<Value> {
    let Some(v) = v else {
        return Ok(Value::Null);
    };
    // The one type error this function raises: `toString()` reports it,
    // `toStringOrNull()` maps it to null.
    let type_error = tostring_type_error;
    let cypher5 = is_cypher5(cypher_version);
    // The routing match is exhaustive over `Value`: a new variant fails
    // compilation here instead of being deferred to a renderer's `Option`.
    match v {
        Value::Null => Ok(Value::Null),
        // A byte array is a homogeneous LIST<INTEGER> (ADR-0327) and is
        // rejected in every dialect.
        Value::Bytes(_) => Err(type_error()),
        // One arm for the container and graph-value vocabulary: CIP-271
        // (Cypher 25) renders every one in display form — a temporal nested
        // in any container renders at full precision (ADR-0448) — while the
        // CYPHER-5 dialect keeps the pre-CIP-271 contract (CIP-150) and
        // rejects it. The dialect test sits inside, so the vocabulary has
        // one spelling.
        Value::List(_)
        | Value::Map(_)
        | Value::Node(_)
        | Value::Relationship(_)
        | Value::Path(_) => {
            if cypher5 {
                Err(type_error())
            } else {
                Ok(Value::String(v.to_display_string().into()))
            }
        }
        // A Vector renders in its literal form — the one scalar outside
        // the shared renderer's acceptance set. Every dialect renders it.
        Value::Vector { .. } => Ok(Value::String(v.to_cypher_literal().into())),
        // A STRING passes through as the same shared payload (no copy).
        Value::String(s) => Ok(Value::String(Arc::clone(s))),
        // Every other scalar renders through the one display policy
        // (temporals at full precision, ADR-0448).
        Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_) => Ok(Value::String(super::render_accepted_scalar(v))),
    }
}

fn to_boolean_value(v: Option<&Value>) -> Result<Value> {
    match v {
        Some(Value::Bool(b)) => Ok(Value::Bool(*b)),
        // Neo4j's integer arm: zero is false, every other integer true
        // (`WITH toBoolean(sum(0)) …` never raises).
        Some(Value::Int(i)) => Ok(Value::Bool(*i != 0)),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Ok(Value::Bool(true)),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Ok(Value::Bool(false)),
        Some(Value::String(_)) | Some(Value::Null) | None => Ok(Value::Null),
        _ => Err(Error::ty("InvalidArgumentValue: toBoolean()")),
    }
}

/// Shared body of `toInteger()`/`toIntegerOrNull()` (ADR-0168, string rules
/// ADR-0175): floats go through [`float_to_i64`] and never fail; a
/// string goes through [`integer_from_string`] — Neo4j's `parseLong` →
/// exact-decimal → Cypher25-literal-fallback pipeline. An out-of-range
/// exact decimal is a `NumberOutOfRange` error, which the `OrNull` variant
/// maps to null; a non-numeric string (every NaN and Infinity spelling
/// included) is null.
fn to_integer_value(v: Option<&Value>) -> Result<Value> {
    Ok(match v {
        Some(Value::Int(i)) => Value::Int(*i),
        Some(Value::Float(f)) => Value::Int(float_to_i64(*f)),
        // Neo4j converts booleans to 1/0 (live 2026.10.1: toInteger(true)
        // is 1); the list projection already does the same for elements.
        Some(Value::Bool(b)) => Value::Int(i64::from(*b)),
        Some(Value::String(s)) => match integer_from_string(s) {
            IntegerString::InRange(i) => Value::Int(i),
            IntegerString::OutOfRange => {
                // ADR-0175's 22N03 mapping: NumberOutOfRange is the
                // ArgumentError class (the official TCK's List11 detail),
                // so the kind travels as data.
                return Err(Error::argument_value_kind(
                    ErrorKind::NumberOutOfRange,
                    format!(
                        "toInteger() expected 'input' to be of type INTEGER \
                         and in the range {} to {} but found {}",
                        i64::MIN,
                        i64::MAX,
                        string::trim_java(s)
                    ),
                ));
            }
            IntegerString::NotANumber => Value::Null,
        },
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("InvalidArgumentValue: toInteger()")),
    })
}

/// Shared body of `toFloat()`/`toFloatOrNull()` (string rules ADR-0175):
/// convert an optional value to a float. A string goes through
/// [`float_from_string`] — Neo4j's `Double.parseDouble` + Cypher25-literal
/// pipeline. Parse failures and null return `Value::Null`; only a
/// non-convertible *type* errors, which the `OrNull` variant maps to null.
fn to_float_value(v: Option<&Value>) -> Result<Value> {
    Ok(match v {
        Some(Value::Float(f)) => Value::Float(*f),
        Some(Value::Int(i)) => Value::Float(*i as f64),
        Some(Value::String(s)) => float_from_string(s).map_or(Value::Null, Value::Float),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("InvalidArgumentValue: toFloat()")),
    })
}

#[cfg(test)]
mod tests;
