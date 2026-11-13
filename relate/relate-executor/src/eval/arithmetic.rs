//! The arithmetic operator family (`+`, `||`, `-`, `*`, `/`, `%`, `^`):
//! the value-level semantics `binary_inner` dispatches to, beside the
//! `round`/unary helpers in `numeric.rs`.

use std::sync::Arc;

use relate_core::{temporal, ArithmeticFailure, Error, Result, Value};

use crate::overlay::ExecGraph;

/// `+`'s string-concatenation arms: a STRING operand renders the other
/// scalar through the one CIP-271 display policy (the same one `toString()`
/// uses — a duration renders as its ISO form, a point as
/// `point({x: …, crs: …})`) and joins, charging the result at the build
/// site once (ADR-0463). `string_first` distinguishes `'a' + 1` from
/// `1 + 'a'`.
fn concat_scalar_string(
    graph: &ExecGraph<'_>,
    string: &Arc<str>,
    scalar: &Value,
    string_first: bool,
) -> Result<Value> {
    let rendered = scalar.to_display_string();
    graph.charge_expression(relate_core::str_logical_bytes(string) + rendered.len() as u64)?;
    Ok(Value::String(
        if string_first {
            format!("{string}{rendered}")
        } else {
            format!("{rendered}{string}")
        }
        .into(),
    ))
}

/// The scalar types `+` concatenates with a STRING: every non-container
/// non-graph scalar that has a display form. `is_concat_scalar` is the one
/// spelling of that set — both mirror arms of [`add`] guard on it, so
/// `'a' + x` and `x + 'a'` can never disagree about what concatenates.
fn is_concat_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Temporal(_) | Value::Point(_)
    )
}

pub(crate) fn add(graph: &ExecGraph<'_>, l: Value, r: Value) -> Result<Value> {
    Ok(match (l, r) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::String(a), Value::String(b)) => {
            // ADR-0463: charge the result string's header plus its bytes
            // where the arm builds it, so the charge and the build share one
            // match and cannot drift.
            graph.charge_expression(relate_core::str_logical_bytes(&a) + b.len() as u64)?;
            Value::String(format!("{a}{b}").into())
        }
        (Value::String(a), r) if is_concat_scalar(&r) => concat_scalar_string(graph, &a, &r, true)?,
        (l, Value::String(b)) if is_concat_scalar(&l) => {
            concat_scalar_string(graph, &b, &l, false)?
        }
        (Value::List(a), Value::List(b)) => {
            graph.charge_value_buffer(a.len() + b.len())?;
            let mut o = Arc::unwrap_or_clone(a);
            o.extend(Arc::unwrap_or_clone(b));
            Value::checked_list(o)?
        }
        // CIP-273: MAP + MAP is a shallow merge with last-write-wins on
        // duplicate keys (extend overwrites), null values included.
        (Value::Map(a), Value::Map(b)) => {
            graph.charge_expression(crate::workspace::map_entries_bytes(a.len() + b.len()))?;
            let mut o = Arc::unwrap_or_clone(a);
            o.extend(Arc::unwrap_or_clone(b));
            Value::checked_map(o)?
        }
        (Value::List(a), r) => {
            // A mixed list+scalar operand pair builds a list (`other + 1`
            // slots), not a string — this arm owns that shape.
            graph.charge_value_buffer(a.len() + 1)?;
            let mut o = Arc::unwrap_or_clone(a);
            o.push(r);
            Value::checked_list(o)?
        }
        (l, Value::List(b)) => {
            graph.charge_value_buffer(b.len() + 1)?;
            let mut o = vec![l];
            o.extend(Arc::unwrap_or_clone(b));
            Value::checked_list(o)?
        }
        (Value::Temporal(a), Value::Temporal(b)) => {
            if matches!(b, temporal::Temporal::Duration { .. }) {
                Value::Temporal(temporal::add_duration(&a, &b)?)
            } else if matches!(a, temporal::Temporal::Duration { .. }) {
                Value::Temporal(temporal::add_duration(&b, &a)?)
            } else {
                return Err(Error::ty("+ type mismatch"));
            }
        }
        (Value::Int(a), Value::Int(b)) => Value::Int(
            // Checked like Sub/Mul/Div/Mod: integer overflow is an honest
            // range error (Neo4j errors too), never a silent saturation.
            a.checked_add(b)
                .ok_or_else(|| Error::ty("integer out of range"))?,
        ),
        (l, r) => {
            if let (Some(a), Some(b)) = (l.as_number(), r.as_number()) {
                // Both-integer addition is handled by the `(Int, Int)` arm
                // above, so this is the float path.
                Value::Float(a + b)
            } else {
                return Err(Error::ty("+ type mismatch"));
            }
        }
    })
}

pub(crate) fn concat(graph: &ExecGraph<'_>, l: Value, r: Value) -> Result<Value> {
    match (&l, &r) {
        // `+` on two strings or two lists IS `||`: delegate so the
        // charge-and-build policy has one spelling.
        (Value::String(_), Value::String(_)) | (Value::List(_), Value::List(_)) => add(graph, l, r),
        (Value::Null, Value::String(_) | Value::List(_))
        | (Value::String(_) | Value::List(_), Value::Null)
        | (Value::Null, Value::Null) => Ok(Value::Null),
        _ => Err(Error::invalid_argument_type(
            "|| requires two strings or two lists",
        )),
    }
}

pub(crate) fn sub(l: Value, r: Value) -> Result<Value> {
    match (&l, &r) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        // Neo4j's `-` accepts a temporal left operand only with a duration
        // right operand; `date - date` / `datetime - datetime` is the
        // 22N01 type mismatch ("expected Duration"), never a duration
        // between (`duration.between` is the function that answers that).
        // One arm covers both operands, like `add`'s, so the right operand is
        // read from the pattern rather than re-extracted.
        (Value::Temporal(a), Value::Temporal(b)) => {
            if matches!(b, temporal::Temporal::Duration { .. }) {
                Ok(Value::Temporal(temporal::sub_duration(a, b)?))
            } else {
                Err(Error::ty("- type mismatch"))
            }
        }
        _ => num_op(l, r, i64::checked_sub, |a, b| a - b),
    }
}

pub(crate) fn mul(l: Value, r: Value) -> Result<Value> {
    Ok(match (&l, &r) {
        // Duration * number is commutative, so one or-pattern arm covers
        // both operand orders; a non-number other operand (including
        // duration * duration) takes the same numeric fall-through as any
        // other operand pair.
        (Value::Temporal(d @ temporal::Temporal::Duration { .. }), b)
        | (b, Value::Temporal(d @ temporal::Temporal::Duration { .. })) => match b.as_number() {
            Some(n) => Value::Temporal(temporal::scale_duration(d, n)?),
            None => num_op(l, r, i64::checked_mul, |a, b| a * b)?,
        },
        _ => num_op(l, r, i64::checked_mul, |a, b| a * b)?,
    })
}

pub(crate) fn div(l: Value, r: Value) -> Result<Value> {
    Ok(match (&l, &r) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Temporal(d @ temporal::Temporal::Duration { .. }), b) => {
            let n = b.as_number().ok_or_else(|| Error::ty("/ needs numbers"))?;
            if n == 0.0 {
                return Err(Error::arithmetic(ArithmeticFailure::DivisionByZero));
            }
            Value::Temporal(temporal::scale_duration(d, 1.0 / n)?)
        }
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(Error::arithmetic(ArithmeticFailure::DivisionByZero));
            }
            // Checked like +, -, * and unary minus: live Neo4j 2026.10.1
            // raises 22003 for `i64::MIN / -1` ("long overflow"), the one
            // checked-division overflow, so do not wrap. (ADR-0175 had
            // recorded the plain-Java wrap; superseded by ADR-0569.)
            Value::Int(
                a.checked_div(*b)
                    .ok_or_else(|| Error::ty("integer out of range"))?,
            )
        }
        _ => {
            let a = l.as_number().ok_or_else(|| Error::ty("/ needs numbers"))?;
            let b = r.as_number().ok_or_else(|| Error::ty("/ needs numbers"))?;
            Value::Float(a / b)
        }
    })
}

pub(crate) fn mod_op(l: Value, r: Value) -> Result<Value> {
    Ok(match (&l, &r) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                return Err(Error::arithmetic(ArithmeticFailure::ModuloByZero));
            }
            // ADR-0175: i64::MIN % -1 is 0, matching Neo4j's plain Java
            // `%` (no overflow check on modulo either).
            Value::Int(a.wrapping_rem(*b))
        }
        _ => {
            let a = l.as_number().ok_or_else(|| Error::ty("% needs numbers"))?;
            let b = r.as_number().ok_or_else(|| Error::ty("% needs numbers"))?;
            Value::Float(a % b)
        }
    })
}

pub(crate) fn pow(l: Value, r: Value) -> Result<Value> {
    Ok(if l.is_null() || r.is_null() {
        Value::Null
    } else {
        let a = l.as_number().ok_or_else(|| Error::ty("^ needs numbers"))?;
        let b = r.as_number().ok_or_else(|| Error::ty("^ needs numbers"))?;
        // Java's Math.pow (Neo4j) answers NaN when the base's absolute
        // value is exactly 1 and the exponent is not finite (NaN or either
        // infinity), while IEEE pow (Rust's `powf`) answers 1 for a base of
        // exactly 1 whatever the exponent. Live Neo4j 2026.10.1: `1 ^
        // (0/0.0)` and `1 ^ (1/0.0)` are NaN, `(-1) ^ (±1/0.0)` is NaN,
        // `(0/0.0) ^ 0` is 1.0.
        if a.abs() == 1.0 && !b.is_finite() {
            Value::Float(f64::NAN)
        } else {
            Value::Float(a.powf(b))
        }
    })
}

/// Generic numeric binary op. The integer arm must be a checked op
/// (`None` on overflow) so extreme integers surface an honest range error
/// instead of unwinding into the host; floats never trap.
fn num_op(
    l: Value,
    r: Value,
    on_int: impl Fn(i64, i64) -> Option<i64>,
    on_float: impl Fn(f64, f64) -> f64,
) -> Result<Value> {
    Ok(match (&l, &r) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Int(a), Value::Int(b)) => {
            Value::Int(on_int(*a, *b).ok_or_else(|| Error::ty("integer out of range"))?)
        }
        _ => {
            let a = l.as_number().ok_or_else(|| Error::ty("numeric op"))?;
            let b = r.as_number().ok_or_else(|| Error::ty("numeric op"))?;
            Value::Float(on_float(a, b))
        }
    })
}
