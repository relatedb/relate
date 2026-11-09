//! Scalar expression operators: unary/binary ops, property access,
//! subscript/slice, and CASE.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{temporal, ArithmeticFailure, Error, Result, Value};
use relate_ir::{BinaryOp, Expr, UnaryOp};
use relate_names::{NamedToken, SmallName, Token};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::string::char_range;
use super::{bool_operand, eval};

pub(crate) fn reject_deleted_access(graph: &ExecGraph<'_>, v: &Value) -> Result<()> {
    match v {
        Value::Node(n) if graph.is_deleted_node(n.id) => Err(Error::execution(
            "DeletedEntityAccess: cannot access properties of a deleted node",
        )),
        Value::Relationship(r) if graph.is_deleted_rel(r.id) => Err(Error::execution(
            "DeletedEntityAccess: cannot access properties of a deleted relationship",
        )),
        _ => Ok(()),
    }
}

/// `key` is the property name beside its plan-resolved token;
/// `Token::Unresolved` for a key the plan never saw (a dynamic or
/// projected key).
pub(crate) fn prop(graph: &ExecGraph<'_>, v: &Value, key: &NamedToken) -> Result<Value> {
    Ok(match v {
        Value::Node(n) => graph
            .node_prop_resolved(n.id, key)
            .or_else(|| key.0.with_str(|text| n.properties.get(text).cloned()))
            .unwrap_or(Value::Null),
        Value::Relationship(r) => graph
            .rel_prop_resolved(r.id, key)
            .or_else(|| key.0.with_str(|text| r.properties.get(text).cloned()))
            .unwrap_or(Value::Null),
        Value::Map(m) => key
            .0
            .with_str(|text| m.get(text).cloned())
            .unwrap_or(Value::Null),
        Value::Temporal(t) => key.0.with_str(|text| t.component(text))?,
        // The point accessor vocabulary is the data model's, beside the
        // temporal one, so the executor only dispatches — and both answer
        // the same typed rejection for a component their value lacks.
        Value::Point(p) => key
            .0
            .with_str(|text| relate_core::point_component(p, text))?,
        Value::Null => Value::Null,
        _ => {
            return Err(Error::invalid_argument_type(
                "property access requires a map, node, relationship, temporal, or point value",
            ))
        }
    })
}

pub(crate) fn unary(op: UnaryOp, v: Value) -> Result<Value> {
    Ok(match op {
        UnaryOp::Not => match bool_operand(&v, "NOT")? {
            Some(b) => Value::Bool(!b),
            None => Value::Null,
        },
        UnaryOp::Plus => match v {
            Value::Int(i) => Value::Int(i),
            Value::Float(f) => Value::Float(f),
            Value::Temporal(t @ temporal::Temporal::Duration { .. }) => Value::Temporal(t),
            Value::Null => Value::Null,
            _ => return Err(Error::ty("unary + needs a number")),
        },
        UnaryOp::Minus => match v {
            // i64::MIN is reachable (e.g. duration.between(...).months on
            // far-apart civil dates); negate honestly instead of panicking.
            Value::Int(i) => Value::Int(
                i.checked_neg()
                    .ok_or_else(|| Error::ty("integer out of range"))?,
            ),
            Value::Float(f) => Value::Float(-f),
            Value::Temporal(t @ temporal::Temporal::Duration { .. }) => {
                Value::Temporal(temporal::negate_duration(&t)?)
            }
            Value::Null => Value::Null,
            _ => return Err(Error::ty("unary - needs a number")),
        },
        UnaryOp::IsNull => Value::Bool(v.is_null()),
        UnaryOp::IsNotNull => Value::Bool(!v.is_null()),
    })
}

pub(crate) fn binary(graph: &ExecGraph<'_>, op: BinaryOp, l: Value, r: Value) -> Result<Value> {
    binary_inner(Some(graph), op, l, r)
}

/// The comparison-only entry: the batch/kernel/predicate-spec sites compare
/// already-evaluated values and have no graph in scope, so they cannot charge
/// an allocating `+`/`||`. Those operators are refused here rather than
/// silently bypassing the ADR-0463 seam; every comparison op dispatches
/// exactly as [`binary`] does.
pub(crate) fn binary_pure(op: BinaryOp, l: Value, r: Value) -> Result<Value> {
    binary_inner(None, op, l, r)
}

fn binary_inner(charge: Option<&ExecGraph<'_>>, op: BinaryOp, l: Value, r: Value) -> Result<Value> {
    Ok(match op {
        BinaryOp::Eq => l.cypher_eq(&r),
        BinaryOp::Neq => match l.cypher_eq(&r) {
            Value::Bool(b) => Value::Bool(!b),
            other => other,
        },
        // The four ordering operators, from the one (equality-tolerant,
        // predicate) table the index range filter also reads. Vectors have no
        // ordering arm in `Value::compare`, so `<=`/`>=` keep working for
        // equal vectors through the equality fallback.
        BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Lte | BinaryOp::Gte => {
            let (equality_allowed, pred) =
                ordering_operator(op).expect("`op` matched one of the four ordering operators");
            cmp_bool(&l, &r, equality_allowed, pred)
        }
        // The boolean connectives name their operand contexts with the
        // operator's own token — the one spelling `BinaryOp::as_str` owns,
        // the same one the planner's boolean-operand rejection reads.
        BinaryOp::And => match (
            bool_operand(&l, op.as_str())?,
            bool_operand(&r, op.as_str())?,
        ) {
            (Some(false), _) | (_, Some(false)) => Value::Bool(false),
            (Some(true), Some(true)) => Value::Bool(true),
            _ => Value::Null,
        },
        BinaryOp::Or => match (
            bool_operand(&l, op.as_str())?,
            bool_operand(&r, op.as_str())?,
        ) {
            (Some(true), _) | (_, Some(true)) => Value::Bool(true),
            (Some(false), Some(false)) => Value::Bool(false),
            _ => Value::Null,
        },
        BinaryOp::Xor => match (
            bool_operand(&l, op.as_str())?,
            bool_operand(&r, op.as_str())?,
        ) {
            (Some(a), Some(b)) => Value::Bool(a ^ b),
            _ => Value::Null,
        },
        BinaryOp::Add => match charge {
            Some(graph) => add(graph, l, r)?,
            None => {
                return Err(Error::internal(
                    "allocating '+' reached the comparison-only binary path",
                ))
            }
        },
        BinaryOp::Sub => sub(l, r)?,
        BinaryOp::Mul => mul(l, r)?,
        BinaryOp::Div => div(l, r)?,
        BinaryOp::Mod => mod_op(l, r)?,
        BinaryOp::Pow => pow(l, r)?,
        BinaryOp::Concat => match charge {
            Some(graph) => concat(graph, l, r)?,
            None => {
                return Err(Error::internal(
                    "allocating '||' reached the comparison-only binary path",
                ))
            }
        },
        BinaryOp::In => in_op(l, r)?,
        BinaryOp::StartsWith => str_pred(l, r, |a, b| a.starts_with(b)),
        BinaryOp::EndsWith => str_pred(l, r, |a, b| a.ends_with(b)),
        BinaryOp::Contains => str_pred(l, r, |a, b| a.contains(b)),
        BinaryOp::Regex => match (&l, &r) {
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            (Value::String(a), Value::String(b)) => match regex_is_match(a, b) {
                Ok(m) => Value::Bool(m),
                // 22N11 is an invalid-argument (data exception) rejection.
                Err(_) => return Err(Error::argument_error("invalid regular expression")),
            },
            // A non-null non-string operand is 22NB1, a type mismatch
            // (Neo4j), not a null result.
            _ => {
                return Err(Error::invalid_argument_type(
                    "the regex operator requires string operands",
                ))
            }
        },
    })
}

pub(crate) fn regex_is_match(text: &str, pattern: &str) -> std::result::Result<bool, regex::Error> {
    // `=~` is a whole-string match (Neo4j's `Pattern.matches`), not a
    // substring search: `'21_val' =~ '1_.*'` is false. The non-capturing
    // group keeps an alternation (`a|b`) inside the anchors.
    regex::Regex::new(&format!("^(?:{pattern})$")).map(|re| re.is_match(text))
}

/// One ordering operator: whether an incomparable-but-equal pair satisfies it
/// (the `<=`/`>=` disjunct `a = b`), and the predicate over `Value::compare`.
pub(crate) type OrderingOperator = (bool, fn(Ordering) -> bool);

/// The four ordering operators' [`OrderingOperator`]s: `<=`/`>=` also hold
/// when the operands are equal, which is what makes them true for values
/// `Value::compare` cannot order (durations, UUIDs, points) where `<`/`>` are
/// null. The shared table keeps `binary`'s evaluation and the index range
/// filter (`graph::declared::order::cmp_holds`) on one definition; `None` for
/// every other operator.
pub(crate) fn ordering_operator(op: BinaryOp) -> Option<OrderingOperator> {
    Some(match op {
        BinaryOp::Lt => (false, Ordering::is_lt),
        BinaryOp::Gt => (false, Ordering::is_gt),
        BinaryOp::Lte => (true, Ordering::is_le),
        BinaryOp::Gte => (true, Ordering::is_ge),
        _ => return None,
    })
}

/// Borrows both operands: the indexed range filter calls this per posting
/// (`cmp_holds`), where a by-value signature would heap-clone a non-Arc
/// `Value::Temporal` on every check.
pub(crate) fn cmp_bool(
    l: &Value,
    r: &Value,
    equality_allowed: bool,
    pred: impl Fn(std::cmp::Ordering) -> bool,
) -> Value {
    let ln = l.as_number();
    let rn = r.as_number();
    if ln.is_some_and(|f| f.is_nan()) || rn.is_some_and(|f| f.is_nan()) {
        return if ln.is_some() && rn.is_some() {
            Value::Bool(false)
        } else {
            Value::Null
        };
    }
    match l.compare(r) {
        Some(o) => Value::Bool(pred(o)),
        // Incomparable: null, unless the equality-tolerant operator's other
        // disjunct (`a = b`) holds.
        None if equality_allowed => match l.cypher_eq(r) {
            Value::Bool(true) => Value::Bool(true),
            _ => Value::Null,
        },
        None => Value::Null,
    }
}

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
            // ADR-0175: i64::MIN / -1 wraps to i64::MIN, matching
            // Neo4j's plain Java `/`, which carries no overflow check
            // (unlike +, -, * and unary minus, which keep raising
            // `integer out of range`).
            Value::Int(a.wrapping_div(*b))
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
pub(crate) fn num_op(
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

/// The one `IN`-operand policy: `None` for a null operand (`x IN null` is
/// null) and the member list otherwise; a non-list is the typed shape
/// error. Every `IN` consumer -- the membership operator, the CIP-240
/// property-in-set filter, and the entity-set filter -- reads its operand
/// through here, so the three cannot disagree about null.
pub(crate) fn in_rhs(value: &Value) -> Result<Option<&[Value]>> {
    if value.is_null() {
        return Ok(None);
    }
    match value {
        Value::List(xs) => Ok(Some(xs)),
        _ => Err(Error::invalid_argument_type(
            "IN requires a list on the right-hand side",
        )),
    }
}

pub(crate) fn in_op(l: Value, r: Value) -> Result<Value> {
    Ok(match in_rhs(&r)? {
        Some(members) => super::membership(&l, members),
        None => Value::Null,
    })
}

pub(crate) fn str_pred(l: Value, r: Value, f: impl Fn(&str, &str) -> bool) -> Value {
    match (&l, &r) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::String(a), Value::String(b)) => Value::Bool(f(a, b)),
        _ => Value::Null,
    }
}

pub(crate) fn subscript(graph: &ExecGraph<'_>, v: &Value, index: Value) -> Result<Value> {
    Ok(match (v, index) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::List(xs), idx) => {
            // ADR-0175: any FLOAT index — including 1.0 and NaN — is a type
            // error (Neo4j 22N01); no truncation.
            let i = match idx {
                Value::Int(i) => i,
                _ => {
                    return Err(Error::invalid_argument_type(
                        "list index must be an integer",
                    ))
                }
            };
            let n = xs.len() as i64;
            let i = if i < 0 { n + i } else { i };
            if i < 0 || i >= n {
                Value::Null
            } else {
                xs[i as usize].clone()
            }
        }
        // A string key rides the same dispatch as `n.k` (property access),
        // so the two paths cannot drift (the Map/Node/Relationship arms used
        // to be a hand-copied, subtly divergent subset of prop).
        (_, Value::String(k)) => prop(graph, v, &(SmallName::from_name(&k), Token::Unresolved))?,
        (Value::Vector { .. }, _) => {
            return Err(Error::invalid_argument_type(
                "vector indexing is not supported",
            ))
        }
        (Value::Map(_), _) | (Value::Node(_), _) | (Value::Relationship(_), _) => {
            return Err(Error::ty(
                "MapElementAccessByNonString: map index must be a string",
            ))
        }
        _ => {
            return Err(Error::invalid_argument_type(
                "subscript requires a list or map",
            ))
        }
    })
}

pub(crate) fn slice(v: &Value, start: Option<Value>, end: Option<Value>) -> Result<Value> {
    let (start, end) = (start.as_ref(), end.as_ref());
    Ok(match v {
        Value::Null => Value::Null,
        Value::List(xs) => match slice_bounds(xs.len(), start, end)? {
            Some(range) => Value::List(xs[range].to_vec().into()),
            None => Value::Null,
        },
        // A string slices by character; the substring is copied once.
        Value::String(s) => match slice_bounds(s.chars().count(), start, end)? {
            Some(range) => Value::String(char_range(s, range).into()),
            None => Value::Null,
        },
        _ => return Err(Error::ty("slice needs a list")),
    })
}

/// The index range `[start..end]` selects from a sequence of `len` elements,
/// or `None` when either bound is null (the slice is null, even if the other
/// bound is not an integer). A missing bound is the sequence's start or end;
/// a negative bound counts from the end; both clamp to `0..=len`, and a
/// start at or past the end selects nothing.
///
/// ADR-0175: an INTEGER bound is a long — it clamps to the sequence across
/// the whole i64 range (the harness pins 2147483648, Long.MAX and Long.MIN;
/// only a literal that does not fit an i64 is rejected, at parse time).
/// A FLOAT bound converts like Neo4j's `asIntExact`: truncate toward zero
/// (NaN → 0) and then require the int32 range — beyond it (±Infinity
/// included) raises NumberOutOfRange. A non-numeric bound keeps the
/// historical bare `what` type error.
fn slice_bounds(
    len: usize,
    start: Option<&Value>,
    end: Option<&Value>,
) -> Result<Option<std::ops::Range<usize>>> {
    if matches!(start, Some(Value::Null)) || matches!(end, Some(Value::Null)) {
        return Ok(None);
    }
    let n = len as i64;
    let bound = |b: Option<&Value>, missing: i64, what: &str| -> Result<usize> {
        let i = match b {
            None => missing,
            Some(v) => {
                let i = super::numeric::int_arg(Some(v)).ok_or_else(|| Error::ty(what))?;
                // Only the FLOAT coercion is int32-exact; an INTEGER bound
                // is a long that clamps.
                if matches!(v, Value::Float(_))
                    && !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&i)
                {
                    return Err(Error::ty(format!("NumberOutOfRange: {what}")));
                }
                i
            }
        };
        Ok((if i < 0 { i + n } else { i }).clamp(0, n) as usize)
    };
    let s = bound(start, 0, "slice start")?;
    let e = bound(end, n, "slice end")?;
    Ok(Some(s..e.max(s)))
}

pub(crate) fn eval_case(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    when_then: &[(Expr, Expr)],
    else_expr: &Option<Box<Expr>>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    // Every arm's condition is a predicate: the parser lowers a simple-CASE
    // test operand into the arm's equality comparison, so first-true wins and
    // an arm that is not true falls through to the next.
    for (when, then) in when_then {
        if eval(graph, row, when, params, cypher_version)?.truthy() == Some(true) {
            return eval(graph, row, then, params, cypher_version);
        }
    }
    match else_expr {
        Some(e) => eval(graph, row, e, params, cypher_version),
        None => Ok(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Value {
        Value::String(x.into())
    }

    fn int(x: i64) -> Value {
        Value::Int(x)
    }

    fn ty_err(msg: &str) -> String {
        Error::ty(msg).to_string()
    }

    /// Five characters of 1, 2, 4, 1 and 2 bytes: slice bounds count
    /// characters, so a byte-indexed slice would split a code point.
    const MB: &str = "añ😀bç";

    fn mb_slice(start: Option<Value>, end: Option<Value>) -> Value {
        slice(&s(MB), start, end).unwrap()
    }

    #[test]
    fn string_slice_indexes_by_character() {
        assert_eq!(mb_slice(Some(int(1)), Some(int(3))), s("ñ😀"));
        assert_eq!(mb_slice(Some(int(2)), None), s("😀bç"));
        assert_eq!(mb_slice(None, Some(int(2))), s("añ"));
        assert_eq!(mb_slice(None, None), s(MB));
        assert_eq!(mb_slice(Some(int(4)), Some(int(5))), s("ç"));
        // Integral floats are integer bounds.
        assert_eq!(
            mb_slice(Some(Value::Float(1.0)), Some(Value::Float(2.0))),
            s("ñ")
        );
    }

    #[test]
    fn string_slice_negative_bounds_count_from_the_end() {
        assert_eq!(mb_slice(Some(int(-2)), None), s("bç"));
        assert_eq!(mb_slice(Some(int(-4)), Some(int(-1))), s("ñ😀b"));
        assert_eq!(mb_slice(None, Some(int(-3))), s("añ"));
    }

    #[test]
    fn string_slice_out_of_range_bounds_clamp() {
        assert_eq!(mb_slice(Some(int(-10)), Some(int(10))), s(MB));
        assert_eq!(mb_slice(Some(int(3)), Some(int(99))), s("bç"));
        assert_eq!(mb_slice(Some(int(-99)), Some(int(2))), s("añ"));
    }

    #[test]
    fn string_slice_empty_range_is_the_empty_string() {
        assert_eq!(mb_slice(Some(int(2)), Some(int(2))), s(""));
        assert_eq!(mb_slice(Some(int(4)), Some(int(1))), s(""));
        assert_eq!(mb_slice(Some(int(7)), None), s(""));
        assert_eq!(mb_slice(None, Some(int(-9))), s(""));
        assert_eq!(slice(&s(""), Some(int(0)), Some(int(1))).unwrap(), s(""));
    }

    #[test]
    fn string_slice_null_and_non_integer_bounds() {
        assert_eq!(mb_slice(Some(Value::Null), Some(int(2))), Value::Null);
        assert_eq!(mb_slice(Some(int(1)), Some(Value::Null)), Value::Null);
        assert_eq!(mb_slice(Some(Value::Null), None), Value::Null);
        // A null bound wins over a non-integer one.
        assert_eq!(mb_slice(Some(s("x")), Some(Value::Null)), Value::Null);
        let err = |start, end| slice(&s(MB), start, end).unwrap_err().to_string();
        assert_eq!(err(Some(s("x")), None), ty_err("slice start"));
        // ADR-0175: a float bound truncates instead of erroring; a
        // non-numeric bound keeps the bare type error.
        assert_eq!(err(None, Some(Value::Bool(true))), ty_err("slice end"));
        // The start bound is checked first.
        assert_eq!(err(Some(s("x")), Some(s("y"))), ty_err("slice start"));
    }

    #[test]
    fn list_slice_follows_the_same_bounds_rules() {
        let list = |xs: &[i64]| Value::List(xs.iter().map(|&x| int(x)).collect::<Vec<_>>().into());
        let xs = list(&[0, 1, 2, 3]);
        let sl = |start, end| slice(&xs, start, end);
        assert_eq!(sl(Some(int(1)), Some(int(3))).unwrap(), list(&[1, 2]));
        assert_eq!(sl(Some(int(-3)), Some(int(-1))).unwrap(), list(&[1, 2]));
        assert_eq!(
            sl(Some(int(-9)), Some(int(9))).unwrap(),
            list(&[0, 1, 2, 3])
        );
        assert_eq!(sl(Some(int(3)), Some(int(1))).unwrap(), list(&[]));
        assert_eq!(sl(None, Some(Value::Null)).unwrap(), Value::Null);
        assert_eq!(
            sl(Some(Value::Bool(true)), None).unwrap_err().to_string(),
            ty_err("slice start")
        );
        assert_eq!(
            sl(None, Some(s("x"))).unwrap_err().to_string(),
            ty_err("slice end")
        );
    }

    /// The harness pins slice bounds across the whole i64 range: a bound
    /// beyond Integer.MAX_VALUE clamps instead of raising NumberOutOfRange
    /// (only a literal that does not fit an i64 is a parse-time error).
    #[test]
    fn list_slice_bounds_clamp_at_and_beyond_the_int32_range() {
        let list = |xs: &[i64]| Value::List(xs.iter().map(|&x| int(x)).collect::<Vec<_>>().into());
        let xs = list(&[1, 2, 3, 4, 5]);
        let sl = |start, end| slice(&xs, start, end).unwrap();
        let i32_max_plus_1 = i64::from(i32::MAX) + 1;
        assert_eq!(sl(None, Some(int(i32_max_plus_1))), xs);
        assert_eq!(
            sl(Some(int(0)), Some(int(i32_max_plus_1))),
            list(&[1, 2, 3, 4, 5])
        );
        assert_eq!(
            sl(Some(int(i32_max_plus_1)), Some(int(i32_max_plus_1 + 1))),
            list(&[])
        );
        assert_eq!(sl(None, Some(int(i64::from(i32::MIN) - 1))), list(&[]));
        assert_eq!(sl(Some(int(i64::from(i32::MIN) - 1)), None), xs);
        assert_eq!(sl(None, Some(int(i64::MAX))), xs);
        assert_eq!(sl(Some(int(i64::MAX)), None), list(&[]));
        assert_eq!(sl(None, Some(int(i64::MIN))), list(&[]));
        assert_eq!(sl(Some(int(i64::MIN)), None), xs);
    }

    #[test]
    fn add_concatenates_a_string_with_temporal_and_point_scalars() {
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let point = Value::Point(relate_core::PointValue::cartesian_2d(1.0, 0.0));
        let point_text = "point({x: 1.0, y: 0.0, crs: 'cartesian'})";
        let cases: Vec<(Value, Value, String)> = vec![
            (s("hi"), duration(0, 1), "hiP1D".into()),
            (duration(0, 1), s("hi"), "P1Dhi".into()),
            (s("hi"), point.clone(), format!("hi{point_text}")),
            (point, s("hi"), format!("{point_text}hi")),
            // The scalar arms keep their old rendering.
            (s("hi"), int(2), "hi2".to_string()),
            (int(2), s("hi"), "2hi".to_string()),
        ];
        for (l, r, expected) in cases {
            assert_eq!(add(&graph, l, r).unwrap(), s(&expected), "{expected}");
        }
    }

    #[test]
    fn slice_of_null_or_a_non_sequence() {
        assert_eq!(
            slice(&Value::Null, Some(s("x")), None).unwrap(),
            Value::Null
        );
        assert_eq!(
            slice(&int(5), Some(s("x")), None).unwrap_err().to_string(),
            ty_err("slice needs a list")
        );
    }

    /// A non-null non-string regex operand is a type error (22NB1), and an
    /// invalid pattern is an argument error (22N11) — neither is a null
    /// result.
    #[test]
    fn regex_rejections_carry_the_neo4j_classes() {
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let run = |l: Value, r: Value| binary(&graph, BinaryOp::Regex, l, r);
        // 22NB1: a non-string operand is a type mismatch, not null.
        assert!(run(s("a"), int(42)).is_err());
        // 22N11: an invalid pattern is an argument error, not a syntax error.
        assert_eq!(
            run(s("a"), s("[")).unwrap_err(),
            Error::argument_error("invalid regular expression")
        );
        // Null still propagates.
        assert_eq!(run(Value::Null, int(42)).unwrap(), Value::Null);
    }

    // `=~` is a whole-string match, not a substring search.
    #[test]
    fn regex_anchors_at_both_ends() {
        assert!(!regex_is_match("21_val", "1_.*").unwrap());
        assert!(regex_is_match("1_val", "1_.*").unwrap());
        // An alternation stays inside the anchors.
        assert!(!regex_is_match("ab", "a|b").unwrap());
        assert!(regex_is_match("a", "a|b").unwrap());
        assert!(regex_is_match("", "x*").unwrap());
        assert!(regex_is_match("x1_val", ".*1_.*").unwrap());
        assert!(regex_is_match("abc", "abc").unwrap());
        assert!(regex_is_match("abc", "b").is_ok_and(|m| !m));
        assert!(regex_is_match("abc", "[").is_err());
    }

    fn duration(months: i64, days: i64) -> Value {
        Value::Temporal(relate_core::Temporal::Duration {
            months,
            days,
            nanos: 0,
        })
    }

    /// ADR-0463 regression (rust review round 1): a mixed list+string
    /// operand pair builds a **list** (`other + 1` slots), not a string, so
    /// its charge must be the list buffer. The former estimator's wildcard
    /// string arms matched first and charged ~len+32 bytes, letting
    /// `bigList + 'x'` bypass the budget.
    #[test]
    fn mixed_list_and_string_charges_the_list_buffer() {
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let list = Value::List(vec![int(1), int(2), int(3)].into());
        let out = add(&graph, list, s("x")).unwrap();
        assert!(matches!(out, Value::List(_)), "{out:?}");
        assert_eq!(
            graph.expression_built_bytes(),
            crate::workspace::buffer_bytes(4, std::mem::size_of::<Value>() as u64,)
        );
    }

    /// The same pair under a one-byte cap: the list buffer allocation (one
    /// slot per element plus the header) is refused, where the old
    /// string-shaped estimate would have slipped through.
    #[test]
    fn mixed_list_and_string_is_refused_when_the_buffer_exceeds_the_cap() {
        let mut g = crate::graph::Graph::default();
        g.configure_expression_memory_limit(Some(1));
        let graph = ExecGraph::read(&g);
        let list = Value::List(vec![int(1), int(2), int(3)].into());
        let error = add(&graph, list, s("x")).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Capacity {
                    limit: 1,
                    kind: relate_core::CapacityKind::Expression,
                    ..
                }
            ),
            "{error}"
        );
    }

    fn cmp(op: BinaryOp, l: Value, r: Value) -> Value {
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        binary(&graph, op, l, r).unwrap()
    }

    // Durations have `=` but no ordering: `<`/`>` are null, `<=`/`>=` still
    // hold for equal values (TCK TernaryComparisonAcceptance).
    #[test]
    fn equal_durations_satisfy_the_equality_tolerant_operators() {
        let d = duration(0, 1);
        assert_eq!(cmp(BinaryOp::Lt, d.clone(), d.clone()), Value::Null);
        assert_eq!(cmp(BinaryOp::Gt, d.clone(), d.clone()), Value::Null);
        assert_eq!(cmp(BinaryOp::Lte, d.clone(), d.clone()), Value::Bool(true));
        assert_eq!(cmp(BinaryOp::Gte, d.clone(), d.clone()), Value::Bool(true));
        // Different durations are incomparable in every direction.
        let other = duration(0, 2);
        for op in [BinaryOp::Lt, BinaryOp::Gt, BinaryOp::Lte, BinaryOp::Gte] {
            assert_eq!(cmp(op, d.clone(), other.clone()), Value::Null);
        }
    }

    /// Neo4j's `-` takes a duration right operand only: subtracting one
    /// temporal from another is the 22N01 type mismatch, not a duration
    /// between (`duration.between` answers that).
    #[test]
    fn subtracting_two_temporals_is_a_type_mismatch() {
        // One 2020-01-01 fixture serves both assertions.
        let date = Value::Temporal(temporal::parse_iso("date", "2020-01-01").unwrap());
        let error = sub(date.clone(), date.clone()).unwrap_err();
        assert_eq!(error.to_string(), ty_err("- type mismatch"));

        // A temporal minus a duration is still arithmetic.
        assert_eq!(
            sub(date, duration(0, 1)).unwrap(),
            Value::Temporal(temporal::parse_iso("date", "2019-12-31").unwrap())
        );
    }

    // UUIDs have `=` but no ordering either.
    #[test]
    fn equal_uuids_satisfy_the_equality_tolerant_operators() {
        let a = Value::Uuid(1);
        let b = Value::Uuid(2);
        assert_eq!(cmp(BinaryOp::Gt, a.clone(), b.clone()), Value::Null);
        assert_eq!(cmp(BinaryOp::Lt, b.clone(), a.clone()), Value::Null);
        assert_eq!(cmp(BinaryOp::Gte, a.clone(), a.clone()), Value::Bool(true));
        assert_eq!(cmp(BinaryOp::Lte, a.clone(), a.clone()), Value::Bool(true));
    }
}
