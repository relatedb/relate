//! The type gate: which property values the per-key statistics read.
//!
//! One predicate, applied to each value before any hashing, so a value the
//! statistics decline costs this match and a counter increment. The gate is
//! the cost control: a long string or a vector is never hashed, and a key
//! that is mostly declined takes no sketch.

use relate_core::Value;

/// Longest string, in bytes, the statistics admit. Short strings are the
/// structured text (ids, codes, enum labels, names); a longer one is prose
/// nobody compares for equality, and hashing it costs by length.
pub(super) const MAX_TEXT_BYTES: usize = 64;

/// How the statistics treat one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admission {
    /// A number, boolean, temporal, point, uuid or reference: it outranks
    /// text when the byte budget is full.
    Structured,
    /// A string of at most [`MAX_TEXT_BYTES`]: admitted, and the first
    /// sketch to go when a structured key needs the room.
    Text,
    /// Not summarised: a longer string, a list, a map, a vector, bytes, a
    /// path, a NaN, or a null.
    Declined,
}

/// A key's budget priority, from the values it admitted: a key whose every
/// admitted value is structured outranks one that admitted any text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyClass {
    Structured,
    Text,
}

/// The gate. An exhaustive match: a new `Value` variant must pick a side
/// here.
pub(super) fn admission(value: &Value) -> Admission {
    match value {
        Value::Bool(_)
        | Value::Int(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_)
        | Value::Node(_)
        | Value::Relationship(_) => Admission::Structured,
        Value::Float(f) if !f.is_nan() => Admission::Structured,
        Value::String(s) if s.len() <= MAX_TEXT_BYTES => Admission::Text,
        Value::Float(_)
        | Value::String(_)
        | Value::Null
        | Value::List(_)
        | Value::Map(_)
        | Value::Path(_)
        | Value::Vector { .. }
        | Value::Bytes(_) => Admission::Declined,
    }
}

/// Whether the statistics read `value` at all.
pub(crate) fn is_admitted(value: &Value) -> bool {
    admission(value) != Admission::Declined
}
