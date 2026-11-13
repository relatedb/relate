//! The literal-type and constant vocabulary the static validation rules
//! share: [`StaticLitType`], the statically-known type and value of a
//! literal expression, the arithmetic-operator set, and the constant fold.
//! The one lower home both [`super::static_types`] (the operator gate) and
//! [`super::static_arithmetic`] (the quantifier/`reduce()` arithmetic
//! rules) build on, so neither module imports the other's vocabulary.

use relate_core::Value;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Expr, LiteralValue};

/// The statically-known type of an expression Relate can decide without
/// evaluating it: a literal (list and map literals included), arithmetic and
/// unary operators over such operands, comparisons, and boolean operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StaticLitType {
    Null,
    Boolean,
    Integer,
    Float,
    String,
    List,
    Map,
}

impl StaticLitType {
    pub(super) fn name(self) -> &'static str {
        match self {
            StaticLitType::Null => "NULL",
            StaticLitType::Boolean => "BOOLEAN",
            StaticLitType::Integer => "INTEGER",
            StaticLitType::Float => "FLOAT",
            StaticLitType::String => "STRING",
            StaticLitType::List => "LIST",
            StaticLitType::Map => "MAP",
        }
    }

    pub(super) fn is_numeric(self) -> bool {
        matches!(self, StaticLitType::Integer | StaticLitType::Float)
    }

    /// The static type of a statically-known [`Value`], or `None` for a value
    /// no literal spelling produces (an entity, a path, a temporal). The one
    /// value→type table; the parameter/placeholder arm of
    /// [`static_literal_type`] reads it.
    pub(super) fn of_value(value: &Value) -> Option<StaticLitType> {
        Some(match value {
            Value::Null => StaticLitType::Null,
            Value::Bool(_) => StaticLitType::Boolean,
            Value::Int(_) => StaticLitType::Integer,
            Value::Float(_) => StaticLitType::Float,
            Value::String(_) => StaticLitType::String,
            Value::List(_) => StaticLitType::List,
            Value::Map(_) => StaticLitType::Map,
            _ => return None,
        })
    }
}

/// The static type of an expression, or `None` when the type cannot be
/// determined without evaluation (variables, parameters, property access,
/// function calls, `CASE`, ...). `Paren` is transparent.
///
/// Literals are the base case; the numeric/boolean operators extend it, so a
/// constant expression whose *value* a content check cannot materialize
/// (mixed Int/Float arithmetic, division, a comparison, `NOT`) still has a
/// decided type. Inference is deliberately one-sided: it only answers where
/// the operator's result type is certain for the operand types (numeric
/// arithmetic over numerics, comparisons and boolean operators), and returns
/// `None` everywhere else (`+` on a string/list mix may be concatenation or
/// append, so it stays runtime-decided).
pub(super) fn static_literal_type(expr: &Expr) -> Option<StaticLitType> {
    match unparen(expr) {
        // A literal, or a plan-cache placeholder for one (ADR-0503): the
        // value fold decides the type. A caller's own parameter is `None`
        // and is checked at runtime.
        Expr::Literal(_) | Expr::Parameter(_) => {
            static_value(expr).and_then(|value| StaticLitType::of_value(&value))
        }
        Expr::List(_) => Some(StaticLitType::List),
        Expr::Map(_) => Some(StaticLitType::Map),
        Expr::Unary(unary) => match unary.op {
            "NOT" => Some(StaticLitType::Boolean),
            "-" | "+" => static_literal_type(&unary.expr).filter(|ty| ty.is_numeric()),
            _ => None,
        },
        Expr::Binary(binary) => static_binary_type(binary.op, &binary.left, &binary.right),
        Expr::Comparison(_) => Some(StaticLitType::Boolean),
        _ => None,
    }
}

/// Strip wrapping parentheses.
pub(super) fn unparen(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unparen(inner),
        other => other,
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

/// The statically-known type of `expr` when it is outside `allowed` — the one
/// gate every static argument-type check spells: an undecided type is
/// deferred to the runtime, and so is any type the site admits (`Null` is
/// listed by each site that accepts it). The caller authors the rejection.
pub(super) fn static_type_outside(expr: &Expr, allowed: &[StaticLitType]) -> Option<StaticLitType> {
    match static_literal_type(expr) {
        Some(ty) if !allowed.contains(&ty) => Some(ty),
        _ => None,
    }
}

/// Reject `expr` when its static type is outside `allowed`, with the
/// caller's authored sentence. An undecided type is deferred to the runtime.
/// The one gate every static argument-type check spells.
pub(super) fn require_static_type(
    expr: &Expr,
    allowed: &[StaticLitType],
    detail: impl FnOnce(&StaticLitType) -> String,
) -> Result<(), EngineError> {
    match static_type_outside(expr, allowed) {
        Some(ty) => Err(EngineError::semantic(
            ErrorKind::InvalidArgumentType,
            detail(&ty),
        )),
        None => Ok(()),
    }
}

/// The class a list element must not have for quantifier arithmetic: a
/// statically known literal type that is neither numeric nor null (STRING,
/// BOOLEAN, a nested LIST, a MAP). Shared by the AST arm and the plan-cache
/// `Value` arm of [`list_literal_all_non_numeric`], and by
/// `static_calls`' numeric-list gate.
pub(super) fn non_numeric_leaf_class(ty: StaticLitType) -> bool {
    !ty.is_numeric() && ty != StaticLitType::Null
}

/// The one spelling of "this argument must be an integer": reject a
/// statically-known *literal* (or plan-cache placeholder) of any other type.
/// `allow_null` says whether a literal NULL is a legitimate value at the site
/// (deferred to the runtime, which owns the per-row decision) or a
/// compile-time type error (CIP-34's concurrency value). An expression whose
/// value the fold declines — a variable, a parameter, a constant
/// *expression* — keeps its runtime decision (ADR-0175's recorded deviation
/// for `SKIP 0.0/0.0`).
pub(super) fn require_integer_argument(
    expr: &Expr,
    what: &str,
    allow_null: bool,
) -> Result<(), EngineError> {
    let Some(ty) = static_value(expr).and_then(|value| StaticLitType::of_value(&value)) else {
        return Ok(());
    };
    if ty == StaticLitType::Integer || (allow_null && ty == StaticLitType::Null) {
        return Ok(());
    }
    Err(EngineError::semantic(
        ErrorKind::InvalidArgumentType,
        format!("{what} requires an integer value, got {}", ty.name()),
    ))
}

/// The *class* of an expression whose type is statically known, for CIP-271's
/// list gate: INTEGER and FLOAT unify into one numeric class (`[1, 2.0]` is a
/// homogeneous `LIST<NUMBER>`), every other type keeps its own class. `None`
/// means the type is not statically known (a variable, parameter, property
/// read, function result) or is null — both make the list `LIST<ANY>`.
fn static_literal_class(expr: &Expr) -> Option<StaticLitType> {
    match static_literal_type(expr)? {
        StaticLitType::Integer | StaticLitType::Float => Some(StaticLitType::Integer),
        StaticLitType::Null => None,
        other => Some(other),
    }
}

/// The class of a list *literal* whose elements are all statically known and
/// share one class — the `LIST<T>` CIP-271 rejects for `toString()`. `None`
/// for anything else: an empty list, a null or dynamically typed element,
/// elements of more than one class, or a non-literal expression. All of those
/// are `LIST<ANY>` (or not a list at all) and are left to the runtime.
pub(super) fn homogeneous_list_literal_class(list: &Expr) -> Option<StaticLitType> {
    match unparen(list) {
        Expr::List(items) if !items.is_empty() => {
            let mut classes = items.iter().map(static_literal_class);
            let first = classes.next()??;
            classes.all(|class| class == Some(first)).then_some(first)
        }
        _ => None,
    }
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

/// The result type of a binary operator over statically-typed operands, or
/// `None` when it is not decided (a non-numeric operand of an arithmetic
/// operator, an unknown operator, an operand of unknown type).
fn static_binary_type(op: &str, left: &Expr, right: &Expr) -> Option<StaticLitType> {
    match op {
        "AND" | "OR" | "XOR" => Some(StaticLitType::Boolean),
        _ if is_arithmetic_op(op) => {
            let left = static_literal_type(left)?;
            let right = static_literal_type(right)?;
            if !left.is_numeric() || !right.is_numeric() {
                return None;
            }
            // Mixed Int/Float arithmetic yields FLOAT; `^` always does.
            Some(
                if op == "^" || left == StaticLitType::Float || right == StaticLitType::Float {
                    StaticLitType::Float
                } else {
                    StaticLitType::Integer
                },
            )
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
