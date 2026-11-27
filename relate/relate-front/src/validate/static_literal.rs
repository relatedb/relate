//! The literal-type and constant vocabulary the static validation rules
//! share: the statically-known kind and value of a literal expression (the
//! core [`LiteralValueKind`] table), the arithmetic-operator set, and the
//! constant fold. The one lower home both [`super::static_types`] (the
//! operator gate) and [`super::static_arithmetic`] (the quantifier/
//! `reduce()` arithmetic rules) build on, so neither module imports the
//! other's vocabulary.

use relate_core::{LiteralValueKind, Value};
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Expr, LiteralValue};

use crate::ast::tree::{is_boolean_operator, unparen};

/// The env-less default of [`static_type_in`]: every variable is
/// runtime-decided.
pub(super) fn static_literal_type(expr: &Expr) -> Option<LiteralValueKind> {
    static_type_in(expr, &|_| None)
}

/// The one expression-type resolver, with an optional variable
/// environment: a caller holding decided variable types — `reduce`'s
/// accumulator and element — supplies them here and the resolver answers
/// the body's type through the same operator table.
///
/// The static type of an expression, or `None` when the type cannot be
/// determined without evaluation (an unbound variable, a parameter,
/// property access, function calls, `CASE`, ...). `Paren` is transparent.
///
/// Literals are the base case; the operators extend it, so a constant
/// expression whose *value* a content check cannot materialize (mixed
/// Int/Float arithmetic, division, a comparison, `NOT`) still has a
/// decided type. Inference is deliberately one-sided: it only answers
/// where the operator's result type is certain for the operand types
/// (numeric arithmetic over numerics, `+`'s String concatenation over
/// non-container scalars, comparisons and boolean operators), and returns
/// `None` everywhere else (`+` with a container or null operand may be
/// append/propagation, so it stays runtime-decided).
pub(super) fn static_type_in(
    expr: &Expr,
    env: &impl Fn(&relate_names::SmallName) -> Option<LiteralValueKind>,
) -> Option<LiteralValueKind> {
    match unparen(expr) {
        // A literal, or a plan-cache placeholder for one (ADR-0503): the
        // value fold decides the type. A caller's own parameter is `None`
        // and is checked at runtime.
        Expr::Literal(_) | Expr::Parameter(_) => static_value_kind(expr),
        Expr::Variable(name) => env(name),
        Expr::List(_) => Some(LiteralValueKind::List),
        Expr::Map(_) => Some(LiteralValueKind::Map),
        Expr::Unary(unary) => match unary.op {
            "NOT" => Some(LiteralValueKind::Boolean),
            "-" | "+" => static_type_in(&unary.expr, env).filter(|ty| ty.is_numeric()),
            _ => None,
        },
        Expr::Binary(binary) => static_binary_type(binary.op, &binary.left, &binary.right, env),
        Expr::Comparison(_) => Some(LiteralValueKind::Boolean),
        _ => None,
    }
}

/// The literal kind of the statically-known *value* `expr` spells — the one
/// "folded value's kind" derivation, read by [`static_literal_type`]'s
/// literal/placeholder arm and by [`require_integer_argument`].
pub(super) fn static_value_kind(expr: &Expr) -> Option<LiteralValueKind> {
    static_value(expr).and_then(|value| LiteralValueKind::of_value(&value))
}

/// The element literal kinds of a list *literal*, or of the plan-cache
/// placeholder that replaced one (ADR-0503); `None` for any other spelling.
/// The one list-source derivation both list predicates read, so the
/// placeholder rule lives once.
pub(super) fn static_list_element_kinds(list: &Expr) -> Option<Vec<Option<LiteralValueKind>>> {
    match unparen(list) {
        Expr::List(items) => Some(items.iter().map(static_literal_type).collect()),
        Expr::Parameter(name) => match relate_core::auto_literal(name) {
            Some(Value::List(items)) => {
                Some(items.iter().map(LiteralValueKind::of_value).collect())
            }
            _ => None,
        },
        _ => None,
    }
}

/// The statically-known *value* an expression spells: a literal, a list
/// literal of them, a sign over one, and a plan-cache placeholder (ADR-0503).
/// A caller's own parameter and every non-literal expression are `None`.
/// [`constant_value`] layers its map marker and arithmetic fold on top.
pub(super) fn static_value(expr: &Expr) -> Option<Value> {
    match unparen(expr) {
        Expr::Literal(LiteralValue::Null) => Some(Value::Null),
        Expr::Literal(LiteralValue::Bool(value)) => Some(Value::Bool(*value)),
        Expr::Literal(LiteralValue::Integer(value)) => Some(Value::Int(*value)),
        Expr::Literal(LiteralValue::Float(value)) => Some(Value::Float(*value)),
        Expr::Literal(LiteralValue::String(value)) => Some(Value::String(value.as_str().into())),
        Expr::List(items) => {
            Value::checked_list(items.iter().map(static_value).collect::<Option<Vec<_>>>()?).ok()
        }
        Expr::Parameter(name) => relate_core::auto_literal(name),
        Expr::Unary(unary) if unary.op == "+" => static_value(&unary.expr),
        Expr::Unary(unary) if unary.op == "-" => match static_value(&unary.expr)? {
            Value::Int(value) => value.checked_neg().map(Value::Int),
            Value::Float(value) => Some(Value::Float(-value)),
            _ => None,
        },
        _ => None,
    }
}

/// The statically-known integer value of `expr` ([`static_value`] projected
/// to `i64`); `None` for anything else.
pub(super) fn static_int_value(expr: &Expr) -> Option<i64> {
    match static_value(expr)? {
        Value::Int(value) => Some(value),
        _ => None,
    }
}

/// The statically-known string value of `expr` ([`static_value`] projected
/// to a string); `None` for anything else.
pub(super) fn static_string_value(expr: &Expr) -> Option<String> {
    match static_value(expr)? {
        Value::String(value) => Some(value.to_string()),
        _ => None,
    }
}

/// The one membership predicate: `Some(ty)` when `ty` is known and outside
/// `allowed`; an undecided type is deferred to the runtime.
fn outside(ty: Option<LiteralValueKind>, allowed: &[LiteralValueKind]) -> Option<LiteralValueKind> {
    ty.filter(|ty| !allowed.contains(ty))
}

/// The statically-known type of `expr` when it is outside `allowed` — the one
/// view of [`outside`] the operator checks read: an undecided type is
/// deferred to the runtime, and so is any type the site admits (`Null` is
/// listed by each site that accepts it). The caller authors the rejection.
pub(super) fn static_type_outside(
    expr: &Expr,
    allowed: &[LiteralValueKind],
) -> Option<LiteralValueKind> {
    outside(static_literal_type(expr), allowed)
}

/// The one static-argument rejection decision every gate below spells: an
/// undecided type is deferred to the runtime, a known type inside `allowed`
/// passes, and anything else is the caller's authored sentence under the
/// caller's error class.
fn reject_static_type(
    ty: Option<LiteralValueKind>,
    allowed: &[LiteralValueKind],
    kind: ErrorKind,
    detail: impl FnOnce(&LiteralValueKind) -> String,
) -> Result<(), EngineError> {
    match outside(ty, allowed) {
        Some(ty) => Err(EngineError::semantic(kind, detail(&ty))),
        None => Ok(()),
    }
}

/// Reject `expr` when its static type is outside `allowed`, with the
/// caller's authored sentence, under the `InvalidArgumentType` class. An
/// undecided type is deferred to the runtime. The one gate most static
/// checks spell.
pub(super) fn require_static_type(
    expr: &Expr,
    allowed: &[LiteralValueKind],
    detail: impl FnOnce(&LiteralValueKind) -> String,
) -> Result<(), EngineError> {
    require_static_type_kind(expr, allowed, ErrorKind::InvalidArgumentType, detail)
}

/// [`require_static_type`] under a caller-chosen error class — the
/// operand-mismatch sites Neo4j reports as `UnexpectedSyntax` or
/// `InvalidConcatenationOperand` rather than `InvalidArgumentType`.
pub(super) fn require_static_type_kind(
    expr: &Expr,
    allowed: &[LiteralValueKind],
    kind: ErrorKind,
    detail: impl FnOnce(&LiteralValueKind) -> String,
) -> Result<(), EngineError> {
    reject_static_type(static_literal_type(expr), allowed, kind, detail)
}

/// The class a list element must not have for quantifier arithmetic: a
/// statically known literal type that is neither numeric nor null (STRING,
/// BOOLEAN, a nested LIST, a MAP). Shared by the AST arm and the plan-cache
/// `Value` arm of [`list_literal_all_non_numeric`], and by
/// `static_calls`' numeric-list gate.
pub(super) fn non_numeric_leaf_class(ty: LiteralValueKind) -> bool {
    !ty.is_numeric() && ty != LiteralValueKind::Null
}

/// The types an integer argument site admits as a statically-known literal:
/// the integer itself.
pub(super) const INTEGER_ONLY: &[LiteralValueKind] = &[LiteralValueKind::Integer];

/// [`INTEGER_ONLY`] plus a literal NULL, which the runtime owns per row.
pub(super) const INTEGER_OR_NULL: &[LiteralValueKind] =
    &[LiteralValueKind::Integer, LiteralValueKind::Null];

/// A STRING operand, plus NULL (which propagates).
pub(super) const STRING_OR_NULL: &[LiteralValueKind] =
    &[LiteralValueKind::String, LiteralValueKind::Null];

/// A LIST operand, plus NULL (which propagates).
pub(super) const LIST_OR_NULL: &[LiteralValueKind] =
    &[LiteralValueKind::List, LiteralValueKind::Null];

/// A MAP operand, plus NULL (which propagates).
pub(super) const MAP_OR_NULL: &[LiteralValueKind] =
    &[LiteralValueKind::Map, LiteralValueKind::Null];

/// `||`'s operand set: a STRING or a LIST, plus NULL.
pub(super) const STRING_OR_LIST_OR_NULL: &[LiteralValueKind] = &[
    LiteralValueKind::String,
    LiteralValueKind::List,
    LiteralValueKind::Null,
];

/// The one spelling of "this argument must be an integer": reject a
/// statically-known *literal* (or plan-cache placeholder) whose type is
/// outside `allowed` ([`INTEGER_ONLY`] or [`INTEGER_OR_NULL`] — a literal
/// NULL is either deferred to the runtime, which owns the per-row decision,
/// or a compile-time type error, CIP-34's concurrency value). An expression
/// whose value the fold declines — a variable, a parameter, a constant
/// *expression* — keeps its runtime decision (ADR-0175's recorded deviation
/// for `SKIP 0.0/0.0`).
pub(super) fn require_integer_argument(
    expr: &Expr,
    what: &str,
    allowed: &[LiteralValueKind],
) -> Result<(), EngineError> {
    reject_static_type(
        static_value_kind(expr),
        allowed,
        ErrorKind::InvalidArgumentType,
        |ty| format!("{what} requires an integer value, got {}", ty.name()),
    )
}

/// The arithmetic-operator vocabulary: the six binary operators whose result
/// type is decided by their numeric operands ([`static_binary_type`]) and that
/// the static rejection rules search a body for. The one table;
/// The quantifier rule's failing-operator set is this one minus `+`, which is also string
/// concatenation and list append and so is not a guaranteed type error.
const ARITHMETIC_OPS: [&str; 6] = ["+", "-", "*", "/", "%", "^"];

/// Whether `op` is one of the [`ARITHMETIC_OPS`].
pub(super) fn is_arithmetic_op(op: &str) -> bool {
    ARITHMETIC_OPS.contains(&op)
}

/// The numeric promotion of a binary arithmetic over two *numeric* kinds —
/// the one Int/Float decision, with the numeric precondition in the same
/// home: `None` unless both operands are numeric.
fn numeric_result(left: LiteralValueKind, right: LiteralValueKind) -> Option<LiteralValueKind> {
    if !left.is_numeric() || !right.is_numeric() {
        return None;
    }
    Some(
        if left == LiteralValueKind::Float || right == LiteralValueKind::Float {
            LiteralValueKind::Float
        } else {
            LiteralValueKind::Integer
        },
    )
}

/// The one `+` operand-pair classification (Neo4j's result rule): a
/// container or null operand stays runtime-decided (append/propagation);
/// a String operand makes it a String concatenation; two numerics take
/// the numeric promotion; every other pair is runtime-decided. The
/// operator table's `+` arm is its one consumer — a `reduce` body's `+`
/// reaches it through `static_type_in`/`static_binary_type` like every
/// other operator.
fn plus_result(left: LiteralValueKind, right: LiteralValueKind) -> Option<LiteralValueKind> {
    use LiteralValueKind::*;
    match (left, right) {
        // `+` with null propagates null at runtime — the pair stays
        // runtime-decided, the module's convention for every null.
        (Null, _) | (_, Null) => None,
        // A container operand makes the sum an append/merge the static
        // rule cannot rank.
        (List, _) | (_, List) | (Map, _) | (_, Map) => None,
        (String, _) | (_, String) => Some(String),
        _ => numeric_result(left, right),
    }
}

/// The one arithmetic-operator result policy: `+` takes the
/// concatenation/append classification, `^` always Float, every other
/// operator the numeric promotion — with the numeric precondition
/// [`numeric_result`] owns.
fn arithmetic_result(
    op: &str,
    left: LiteralValueKind,
    right: LiteralValueKind,
) -> Option<LiteralValueKind> {
    if op == "+" {
        return plus_result(left, right);
    }
    if op == "^" {
        return numeric_result(left, right).map(|_| LiteralValueKind::Float);
    }
    numeric_result(left, right)
}

/// The result type of a binary operator over statically-typed operands, or
/// `None` when it is not decided (a non-numeric operand of an arithmetic
/// operator, an unknown operator, an operand of unknown type).
fn static_binary_type(
    op: &str,
    left: &Expr,
    right: &Expr,
    env: &impl Fn(&relate_names::SmallName) -> Option<LiteralValueKind>,
) -> Option<LiteralValueKind> {
    match op {
        _ if is_boolean_operator(op) => Some(LiteralValueKind::Boolean),
        _ if is_arithmetic_op(op) => {
            let left = static_type_in(left, env)?;
            let right = static_type_in(right, env)?;
            arithmetic_result(op, left, right)
        }
        _ => None,
    }
}

/// A constant operand's fold outcome. `Map` is a map literal: its *type* is
/// decided by syntax alone (and rejected by the type rule at the operand or
/// as a list member), but the fold never materializes it — neither its
/// entries nor an empty-map placeholder stand in for the real value.
#[derive(Debug)]
pub(super) enum FoldedConstant {
    Value(Value),
    /// A map literal, at the operand itself or nested in a constant list.
    /// `{a: $p}` is as constant-typed as `{a: 1}`: the entries are never
    /// read, so their foldability is irrelevant.
    Map,
}

/// The constant subset the compile-time fold evaluates: [`static_value`]'s
/// literal vocabulary plus the map marker and checked arithmetic. `None` is a
/// fold decline, never an error: the expression keeps its runtime semantics,
/// and the executor's shared rule checks it on each row. A map literal
/// anywhere in a constant list is reported as [`FoldedConstant::Map`], so it
/// cannot slip past both the fold and the type check.
pub(super) fn constant_value(expr: &Expr) -> Option<FoldedConstant> {
    match unparen(expr) {
        Expr::Map(_) => Some(FoldedConstant::Map),
        Expr::List(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                match constant_value(item)? {
                    FoldedConstant::Value(value) => values.push(value),
                    FoldedConstant::Map => return Some(FoldedConstant::Map),
                }
            }
            Value::checked_list(values).ok().map(FoldedConstant::Value)
        }
        Expr::Binary(binary) => match (
            constant_value(&binary.left)?,
            constant_value(&binary.right)?,
        ) {
            (FoldedConstant::Value(left), FoldedConstant::Value(right)) => {
                constant_binary(binary.op, left, right).map(FoldedConstant::Value)
            }
            // A map literal under an operator is not a statically-typed
            // operand of this module's contract; the runtime decides it.
            _ => None,
        },
        other => static_value(other).map(FoldedConstant::Value),
    }
}

/// Checked constant arithmetic and string concatenation. Anything whose
/// result is not decided here (mixed numeric types, division, overflow)
/// declines the fold; its *type* is still decided by
/// [`static_literal_type`], so the operand cannot escape validation.
pub(super) fn constant_binary(op: &str, left: Value, right: Value) -> Option<Value> {
    match (op, left, right) {
        ("+" | "||", Value::String(a), Value::String(b)) => {
            Some(Value::String(format!("{a}{b}").into()))
        }
        ("+", Value::Int(a), Value::Int(b)) => a.checked_add(b).map(Value::Int),
        ("-", Value::Int(a), Value::Int(b)) => a.checked_sub(b).map(Value::Int),
        ("*", Value::Int(a), Value::Int(b)) => a.checked_mul(b).map(Value::Int),
        ("+", Value::Float(a), Value::Float(b)) => Some(Value::Float(a + b)),
        ("-", Value::Float(a), Value::Float(b)) => Some(Value::Float(a - b)),
        ("*", Value::Float(a), Value::Float(b)) => Some(Value::Float(a * b)),
        _ => None,
    }
}
