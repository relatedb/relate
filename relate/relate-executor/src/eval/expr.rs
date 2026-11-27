//! Scalar expression operators: unary/binary ops, property access,
//! subscript/slice, and CASE.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{integer_args, temporal, Error, Result, Value};
use relate_ir::{BinaryOp, Expr, UnaryOp};
use relate_names::{NamedToken, SmallName};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::arithmetic::{add, concat, div, mod_op, mul, pow, sub};
use super::function::entity::virtual_own_prop;
use super::regex_cache::regex_is_match;
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
///
/// The one entry for every property read — including a runtime key,
/// which arrives as [`relate_names::unresolved`] — so the deleted-entity
/// policy below cannot be missed by a caller that only picks a subset of
/// the dispatch (the subscript string arm used to answer `null` for a
/// deleted node).
pub(crate) fn prop(graph: &ExecGraph<'_>, v: &Value, key: &NamedToken) -> Result<Value> {
    reject_deleted_access(graph, v)?;
    key.0.with_str(|text| {
        Ok(match v {
            // The own-map fallback is the virtual-entity half of the
            // `properties()`/`keys()` policy (`backing_properties`), read
            // through its single-key sibling — the map is never materialized.
            Value::Node(n) => graph
                .node_prop_resolved(n.id, key)
                .or_else(|| virtual_own_prop(n.id, &n.properties, text))
                .unwrap_or(Value::Null),
            Value::Relationship(r) => graph
                .rel_prop_resolved(r.id, key)
                .or_else(|| virtual_own_prop(r.id, &r.properties, text))
                .unwrap_or(Value::Null),
            Value::Map(m) => m.get(text).cloned().unwrap_or(Value::Null),
            Value::Temporal(t) => t.component(text)?,
            // The point accessor vocabulary is the data model's, beside the
            // temporal one, so the executor only dispatches — and both answer
            // the same typed rejection for a component their value lacks.
            Value::Point(p) => relate_core::point_component(p, text)?,
            Value::Null => Value::Null,
            _ => {
                return Err(Error::invalid_argument_type(
                    "property access requires a map, node, relationship, temporal, or point value",
                ))
            }
        })
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
            let ordering =
                ordering_operator(op).expect("`op` matched one of the four ordering operators");
            cmp_bool(&l, &r, ordering.equality_allowed, ordering.pred)
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
        BinaryOp::In => in_op(&l, &r)?,
        BinaryOp::StartsWith => str_pred(l, r, |a, b| a.starts_with(b)),
        BinaryOp::EndsWith => str_pred(l, r, |a, b| a.ends_with(b)),
        BinaryOp::Contains => str_pred(l, r, |a, b| a.contains(b)),
        BinaryOp::Regex => match (&l, &r) {
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            // 22N11 is an invalid-argument (data exception) rejection,
            // mapped where the cache module's seam ends.
            (Value::String(a), Value::String(b)) => Value::Bool(regex_is_match(a, b)?),
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

/// The side a comparison operator orders its operands on — the direction
/// the expression evaluator and the index range seek both read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrderingSide {
    Greater,
    Less,
}

impl OrderingSide {
    /// Whether this side is the greater-than direction.
    pub(crate) fn is_greater(self) -> bool {
        self == OrderingSide::Greater
    }
}

/// One ordering operator: the direction it orders on, whether an
/// incomparable-but-equal pair satisfies it (the `<=`/`>=` disjunct
/// `a = b`), and the predicate over `Value::compare` that pair determines.
pub(crate) struct OrderingOperator {
    pub(crate) side: OrderingSide,
    pub(crate) equality_allowed: bool,
    pub(crate) pred: fn(Ordering) -> bool,
}

impl OrderingOperator {
    /// The one construction: the predicate follows from the side and the
    /// equality tolerance, so a row cannot name a combination the two
    /// fields do not determine.
    const fn new(side: OrderingSide, equality_allowed: bool) -> Self {
        let pred: fn(Ordering) -> bool = match (side, equality_allowed) {
            (OrderingSide::Less, false) => Ordering::is_lt,
            (OrderingSide::Less, true) => Ordering::is_le,
            (OrderingSide::Greater, false) => Ordering::is_gt,
            (OrderingSide::Greater, true) => Ordering::is_ge,
        };
        Self {
            side,
            equality_allowed,
            pred,
        }
    }
}

/// The four ordering operators' [`OrderingOperator`]s: `<=`/`>=` also hold
/// when the operands are equal, which is what makes them true for values
/// `Value::compare` cannot order (durations, UUIDs, points) where `<`/`>` are
/// null. One row per operator keeps `binary`'s evaluation and the index
/// range seek (`graph::declared::order`) on one definition; `None` for every
/// other operator.
pub(crate) fn ordering_operator(op: BinaryOp) -> Option<OrderingOperator> {
    Some(match op {
        BinaryOp::Lt => OrderingOperator::new(OrderingSide::Less, false),
        BinaryOp::Gt => OrderingOperator::new(OrderingSide::Greater, false),
        BinaryOp::Lte => OrderingOperator::new(OrderingSide::Less, true),
        BinaryOp::Gte => OrderingOperator::new(OrderingSide::Greater, true),
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

/// The one `IN`-operand classification. Every `IN` consumer — the
/// membership operator, the CIP-240 property-in-set filter, and the
/// entity-set filter — reads its policy off the named variant, so the
/// three cannot disagree about null: the operator answers a scalar as
/// the one-element list holding it (ADR-0711) and rejects a container,
/// while the filter forms reject both, each stated at its own match.
pub(crate) enum InOperand<'a> {
    /// A null operand: `x IN null` is null.
    Null,
    /// The member list — the list's shared allocation, which the
    /// membership index is keyed on.
    List(&'a Arc<Vec<Value>>),
    /// A membership scalar ([`is_membership_scalar`]'s one kind-set):
    /// answers as the one-element list holding it.
    Scalar,
    /// Any other value — a map, a vector, an entity, a path, a point:
    /// the typed rejection the list rule always was.
    Container,
}

impl<'a> InOperand<'a> {
    /// The filter forms' half of the operand policy, stated once on the
    /// classification itself: a null operand is the empty member set
    /// (`x IN null` matches nothing, so an empty set carries the same
    /// outcome), a list is its members, and a scalar or container is no
    /// member set at all — the caller's own typed rejection.
    pub(crate) fn filter_members(self) -> Option<&'a [Value]> {
        match self {
            InOperand::Null => Some(&[]),
            InOperand::List(members) => Some(members),
            InOperand::Scalar | InOperand::Container => None,
        }
    }
}

/// The one membership-scalar vocabulary: the value kinds that answer
/// membership questions as themselves — booleans, numbers, strings,
/// temporals. The `IN` right-operand policy (a scalar coerces to the
/// one-element list, ADR-0711) and the search filters' member policy
/// (an allowed scalar member never matches) read the same kind-set
/// here, so the two cannot drift.
pub(crate) fn is_membership_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::String(_) | Value::Temporal(_)
    )
}

/// Classify an `IN` right operand — the one entry point every consumer
/// shares ([`InOperand`] is the policy vocabulary).
pub(crate) fn in_rhs(value: &Value) -> InOperand<'_> {
    match value {
        Value::Null => InOperand::Null,
        Value::List(xs) => InOperand::List(xs),
        value if is_membership_scalar(value) => InOperand::Scalar,
        _ => InOperand::Container,
    }
}

pub(crate) fn in_op(l: &Value, r: &Value) -> Result<Value> {
    // The operand policy is [`InOperand`]'s; the one fact this fold adds
    // is that a one-member list's membership is exactly equality — both
    // null rules included — so the scalar arm is `cypher_eq` itself:
    // nothing allocates, and the pure-operator contract the compiled
    // predicate gate evaluates this under stays intact.
    Ok(match in_rhs(r) {
        // The one production membership entry: a long list bound once per
        // statement answers an exact same-typed probe from its once-built
        // index; everything the index cannot decide exactly takes the scan.
        InOperand::List(list) => super::member_of(l, list),
        // A scalar — and a null operand, whose `x IN null is null` rides
        // the same equality fold (`cypher_eq` answers null whenever
        // either side is null).
        InOperand::Null | InOperand::Scalar => l.cypher_eq(r),
        // A container that is not a list — a map, a vector, an entity, a
        // path — is the typed rejection the list rule always was; the
        // corpus pins the vector and map forms.
        InOperand::Container => {
            return Err(Error::invalid_argument_type(
                "IN requires a list on the right-hand side",
            ))
        }
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
        (_, Value::String(k)) => prop(
            graph,
            v,
            &relate_names::unresolved(SmallName::from_name(&k)),
        )?,
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
/// A FLOAT bound converts like Neo4j dev's dynamic slice bounds
/// (ADR-0631, superseding ADR-0175's `asIntExact` image): the shared
/// `(long)` cast, then saturation into the int32 range — a bound beyond
/// it (±Infinity, `2147483648.0`, `9.3e18`) reads as past-the-end or
/// before-the-start, never a range fault, so a dynamic slice bound no
/// longer raises NumberOutOfRange (the float *literal* still fails at
/// plan time, and `substring()` / `left()` / `right()` and list indexing
/// keep the rejection). A non-numeric bound keeps the historical bare
/// `what` type error.
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
            // A FLOAT bound saturates into the int32 range (ADR-0631):
            // the shared Java `(long)` cast, then the clamp — never a
            // range fault. An INTEGER bound is a long that clamps at the
            // end. The variants are matched, never re-derived from a
            // coerced value.
            Some(Value::Float(f)) => {
                super::numeric::float_to_i64(*f).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            }
            Some(Value::Int(i)) => *i,
            Some(_) => return Err(Error::ty(what)),
        };
        Ok((if i < 0 { i + n } else { i }).clamp(0, n) as usize)
    };
    let s = bound(start, 0, "slice start")?;
    let e = bound(end, n, "slice end")?;
    Ok(Some(s..e.max(s)))
}

/// The row-scalar CASE evaluation: the shared [`eval_case_with`] policy
/// over `eval` against one row.
pub(crate) fn eval_case(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    when_then: &[(Expr, Expr)],
    else_expr: &Option<Box<Expr>>,
) -> Result<Value> {
    eval_case_with(when_then, else_expr, |expr| eval(graph, row, expr, params))
}

/// The one CASE selection policy, read by both CASE evaluations — the
/// row-scalar [`eval_case`] and the aggregating projection's CASE arm —
/// so the two cannot drift on laziness or predicate policy. Every arm's
/// condition is a predicate — the one canonical helper the other
/// predicate positions read ([`predicate_value_holds`]): the parser
/// lowers a simple-CASE test operand into the arm's equality comparison,
/// so first-true wins and an arm that is not true falls through to the
/// next; a runtime non-boolean condition is the typed rejection, never a
/// silent fall-through to ELSE. Only the selected branch evaluates
/// through `eval_expr`: an unselected THEN or ELSE never runs.
pub(crate) fn eval_case_with<'e>(
    when_then: &'e [(Expr, Expr)],
    else_expr: &'e Option<Box<Expr>>,
    mut eval_expr: impl FnMut(&'e Expr) -> Result<Value>,
) -> Result<Value> {
    for (when, then) in when_then {
        if predicate_value_holds(&eval_expr(when)?)? {
            return eval_expr(then);
        }
    }
    match else_expr {
        Some(e) => eval_expr(e),
        None => Ok(Value::Null),
    }
}

#[cfg(test)]
mod tests;
