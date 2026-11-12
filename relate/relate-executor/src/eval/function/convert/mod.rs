//! Conversion builtins: `toString`/`toInteger`/`toFloat`/`toBoolean` and
//! their `OrNull` variants, the `toStringList`/`toFloatList`/
//! `toIntegerList` list conversions, plus the shared coercion helpers.
//! CIP-271's *static* `LIST<T>` rejection lives in the front's validation
//! layer (`relate_front::validate::static_types`); this module owns only
//! the value-level conversion policy. String → number parsing lives in the
//! [`string`] submodule (ADR-0175).

use std::borrow::Cow;
use std::sync::Arc;

use relate_core::{Error, Result, Value};

use crate::eval::numeric::float_to_i64;
use crate::overlay::ExecGraph;

use string::{float_from_string, integer_from_string, IntegerString};

mod string;

pub(super) fn bi_tostring(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // The row's `ARITY_EXACTLY_ONE` contract rejects a second argument
    // before any argument is evaluated (LABS-785).
    to_string_value(vals.first())
}

pub(super) fn bi_tostringornull(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(to_string_value(vals.first()).unwrap_or(Value::Null))
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

pub(super) fn bi_tostringlist(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    convert_list(&vals, ListProjection::String)
}

/// One `toStringList()` element, delegated to the `toString()` scalar
/// policy ([`to_string_value`]) with the two element exceptions that
/// policy does not carry: null renders in display form (`"null"`, TCK
/// TypeConversion4 [10]) instead of staying null, and a byte array is not
/// a stringifiable element (the scalar form's type error becomes a null
/// element — an unconvertible element never raises, the `OrNull` element
/// rule ADR-0175 records).
fn to_string_element(v: &Value) -> Value {
    match v {
        Value::Null => Value::String("null".into()),
        Value::Bytes(_) => Value::Null,
        _ => to_string_value(Some(v)).unwrap_or(Value::Null),
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
/// policy.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum ListProjection {
    Integer,
    Float,
    Boolean,
    String,
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
/// `toStringList()`, converted with the scalar `OrNull` bodies
/// ([`to_integer_value`]/[`to_float_value`]/[`to_boolean_value`]/
/// [`to_string_element`]): Neo4j routes every element through the `OrNull`
/// paths (ADR-0175), so the scalar bodies' type errors become null
/// elements instead of raising — null stays null; a FLOAT converts per
/// ADR-0168 ([`float_to_i64`]: NaN → 0, saturate, truncate toward zero); a
/// BOOLEAN converts to 1/0 in `toIntegerList()` and to null in
/// `toFloatList()`; a string that fails to parse, including an
/// out-of-range numeric string, becomes a NULL element and never raises.
fn convert_element(v: &Value, projection: ListProjection) -> Value {
    match projection {
        ListProjection::String => to_string_element(v),
        ListProjection::Boolean => to_boolean_value(Some(v)).unwrap_or(Value::Null),
        ListProjection::Integer => to_integer_value(Some(v)).unwrap_or(Value::Null),
        ListProjection::Float => to_float_value(Some(v)).unwrap_or(Value::Null),
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
/// null. CIP-271: every container and graph value renders in display form
/// (a temporal nested in any container renders at full precision,
/// ADR-0448).
fn to_string_value(v: Option<&Value>) -> Result<Value> {
    let Some(v) = v else {
        return Ok(Value::Null);
    };
    // The one type error this function raises: `toString()` reports it,
    // `toStringOrNull()` maps it to null.
    let type_error = tostring_type_error;
    // The routing match is exhaustive over `Value`: a new variant fails
    // compilation here instead of being deferred to a renderer's `Option`.
    match v {
        Value::Null => Ok(Value::Null),
        // A byte array is a homogeneous LIST<INTEGER> (ADR-0327) and is
        // rejected in every dialect.
        Value::Bytes(_) => Err(type_error()),
        // One arm for the container and graph-value vocabulary: CIP-271
        // renders every one in display form — a temporal nested in any
        // container renders at full precision (ADR-0448) — so the
        // vocabulary has one spelling.
        Value::List(_)
        | Value::Map(_)
        | Value::Node(_)
        | Value::Relationship(_)
        | Value::Path(_) => Ok(Value::String(v.to_display_string().into())),
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
/// exact decimal is an `InvalidArgumentValue` error (ADR-0565), which the
/// `OrNull` variant maps to null; a non-numeric string (every NaN and
/// Infinity spelling included) is null.
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
                // Live Neo4j 2026.10.1 raises CypherTypeException here —
                // 22003 under the TypeError class (ADR-0565) — unlike the
                // integer-argument range sites, whose 22N03 is the
                // ArgumentError class.
                return Err(Error::invalid_argument_value(format!(
                    "toInteger() expected 'input' to be of type INTEGER \
                     and in the range {} to {} but found {}",
                    i64::MIN,
                    i64::MAX,
                    string::trim_java(s)
                )));
            }
            IntegerString::NotANumber => Value::Null,
        },
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::invalid_argument_value("toInteger()")),
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
