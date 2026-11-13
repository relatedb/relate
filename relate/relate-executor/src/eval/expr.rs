//! Scalar expression operators: unary/binary ops, property access,
//! subscript/slice, and CASE.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use relate_core::{integer_args, temporal, Error, ErrorKind, Result, Value};
use relate_ir::{BinaryOp, Expr, UnaryOp};
use relate_names::{NamedToken, SmallName, Token};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::arithmetic::{add, concat, div, mod_op, mul, pow, sub};
use super::string::char_range;
use super::{bool_operand, eval, predicate_value_holds};

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
                        integer_args::LIST_INDEX.type_error,
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
                let i = super::numeric::int_arg(v).ok_or_else(|| Error::ty(what))?;
                // Only the FLOAT coercion is int32-exact; an INTEGER bound
                // is a long that clamps.
                if matches!(v, Value::Float(_))
                    && !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&i)
                {
                    // ADR-0175's 22N03 mapping: NumberOutOfRange is the
                    // ArgumentError class (live 2026.10.1:
                    // Neo.ClientError.Statement.ArgumentError).
                    return Err(Error::argument_value_kind(
                        ErrorKind::NumberOutOfRange,
                        what,
                    ));
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
) -> Result<Value> {
    // Every arm's condition is a predicate — the one canonical helper the
    // other predicate positions read ([`predicate_value_holds`]): the
    // parser lowers a simple-CASE test operand into the arm's equality
    // comparison, so first-true wins and an arm that is not true falls
    // through to the next; a runtime non-boolean condition is the typed
    // rejection, never a silent fall-through to ELSE.
    for (when, then) in when_then {
        if predicate_value_holds(&eval(graph, row, when, params)?)? {
            return eval(graph, row, then, params);
        }
    }
    match else_expr {
        Some(e) => eval(graph, row, e, params),
        None => Ok(Value::Null),
    }
}

#[cfg(test)]
mod tests;
