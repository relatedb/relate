//! Shared [`Value`] fixtures: the one constructor per value kind the
//! engine's test suites repeat.

use relate_core::{PointValue, Value};

/// A 2D `Value::Point` (no height) on the given SRID.
pub fn point(srid: i32, x: f64, y: f64) -> Value {
    Value::Point(PointValue {
        srid,
        x,
        y,
        z: None,
    })
}

/// `base` wrapped in `depth` nested lists: the one spelling of the
/// wrap-in-N-lists loop the nesting-limit tests share.
pub fn wrapped_in_lists(base: Value, depth: usize) -> Value {
    let mut value = base;
    for _ in 0..depth {
        value = Value::List(vec![value].into());
    }
    value
}
