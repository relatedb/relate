//! Generic scalar builtins: `coalesce`, `nullif`, `exists`, and `valueType`.

use relate_core::{value_type_name, Result, Value};

use crate::overlay::ExecGraph;

pub(super) fn bi_exists(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Bool(b)) => Value::Bool(*b),
        Some(Value::Null) | None => Value::Bool(false),
        Some(_) => Value::Bool(true),
    })
}

pub(super) fn bi_coalesce(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(vals
        .into_iter()
        .find(|v| !v.is_null())
        .unwrap_or(Value::Null))
}

pub(super) fn bi_nullif(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // The two-argument count is the registry row's contract, enforced before
    // any argument is evaluated.
    Ok(if vals[0].is_null() || vals[1].is_null() {
        vals[0].clone()
    } else if vals[0].eq_non_null(&vals[1]) {
        Value::Null
    } else {
        vals[0].clone()
    })
}

pub(super) fn bi_valuetype(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(Value::String(
        value_type_name(
            vals.first()
                .expect("the valuetype row binds exactly one argument"),
        )
        .into(),
    ))
}
