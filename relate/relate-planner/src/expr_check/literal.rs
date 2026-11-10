//! The statically-known literal vocabulary: what an expression spells when
//! its value is known at plan time (a literal, a resolved plan-cache
//! placeholder, a folded eager value), and the ADR-0175 rejections the
//! integer-argument sites raise from it. Split from the expression checker
//! (`shadowing` sets the split precedent) so the checker file is the
//! traversal/predicate policy and this file is the literal classification.

use relate_core::integer_args::{self, IntegerArgSite};
use relate_core::{Error, Result, Value};
use relate_ir::{Expr, UnaryOp};

/// The literal an expression spells, bucketed by the kind each caller
/// tests for — `Other` carries every value that is neither null, boolean,
/// nor numeric.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LiteralKind {
    Null,
    Bool,
    Int,
    Float,
    /// Every other value: string, collection, temporal, entity, …
    Other,
}

/// The statically-known literal an expression spells: a plan-cache
/// placeholder is resolved to the literal it replaced (ADR-0503) and a
/// unary sign is unwrapped (the parser splits `-1.5` and `+1.5` into a
/// unary operator over the literal). A caller's own parameter and every
/// non-literal expression are `None`.
fn static_literal(expr: &Expr) -> Option<LiteralKind> {
    match expr {
        Expr::Null => Some(LiteralKind::Null),
        Expr::Bool(_) => Some(LiteralKind::Bool),
        Expr::Int(_) => Some(LiteralKind::Int),
        Expr::Float(_) => Some(LiteralKind::Float),
        Expr::String(_) | Expr::List(_) | Expr::Map(_) => Some(LiteralKind::Other),
        Expr::Parameter(name) => relate_core::auto_literal(name).as_ref().map(literal_kind),
        Expr::Unary {
            op: UnaryOp::Minus | UnaryOp::Plus,
            expr,
        } => static_literal(expr),
        // An executor-side eager value: a temporal constructor call the
        // front end folded (`date('2020-01-01')`).
        Expr::RuntimeValue(value) => Some(literal_kind(value)),
        _ => None,
    }
}

fn literal_kind(value: &Value) -> LiteralKind {
    match value {
        Value::Null => LiteralKind::Null,
        Value::Bool(_) => LiteralKind::Bool,
        Value::Int(_) => LiteralKind::Int,
        Value::Float(_) => LiteralKind::Float,
        // Every other value is a non-numeric literal; an unknown one is
        // rejected at an integer-argument site (it fails closed).
        Value::String(_)
        | Value::List(_)
        | Value::Map(_)
        | Value::Node(_)
        | Value::Relationship(_)
        | Value::Path(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_)
        | Value::Vector { .. }
        | Value::Bytes(_) => LiteralKind::Other,
    }
}

pub(super) fn is_static_non_bool(expr: &Expr) -> bool {
    // Bool and null stay legal boolean operands.
    matches!(expr, Expr::CountStar)
        || matches!(
            static_literal(expr),
            Some(kind) if !matches!(kind, LiteralKind::Bool | LiteralKind::Null)
        )
}

/// Whether `expr` is a float literal, including a plan-cache placeholder
/// for one.
fn is_float_literal(expr: &Expr) -> bool {
    static_literal(expr) == Some(LiteralKind::Float)
}

/// ADR-0175 (statically-known floats): the integer-argument site an
/// expression carries whose literal gate is a float — a list index or a
/// slice bound — paired with the site whose authored detail a rejection
/// names. The one list of those forms, precedence included (a slice's
/// start before its end): the checker rejects through it and the
/// plan-cache walk classifies through it.
///
/// A float *literal*, sign included: the parser splits `-1.5`/`+1.5`
/// into a unary sign over the literal, and both spellings are the mistake
/// Neo4j's static FLOAT gate rejects at compile time (22N27 / 22NB1).
/// Constant float *expressions* (`0.0/0.0`, `head([1.0, 'x'])`, …) are
/// not literals and are handled at runtime; the wording here is
/// plan-time-only, since those sites' runtime coercion truncates floats
/// (NaN → 0, saturating) and raises `NumberOutOfRange` instead.
pub(crate) fn float_integer_site(expr: &Expr) -> Option<&'static IntegerArgSite> {
    match expr {
        Expr::Subscript { index, .. } if is_float_literal(index) => Some(&integer_args::LIST_INDEX),
        Expr::Slice { start, .. } if start.as_deref().is_some_and(is_float_literal) => {
            Some(&integer_args::SLICE_START)
        }
        Expr::Slice { end, .. } if end.as_deref().is_some_and(is_float_literal) => {
            Some(&integer_args::SLICE_END)
        }
        _ => None,
    }
}

/// Whether `expr` is a statically-known literal of a type that is not
/// INTEGER. Live Neo4j 2026.10.1 reports the integer-argument sites'
/// wrong-typed literals as type mismatches (`substring('abcdef','x')`,
/// `substring('abcdef',1,'x')`, `left('abcdef','x')`,
/// `right('abcdef','x')`, `range('a',5)`, and a temporal constructor call
/// such as `substring('abcdef', date('2020-01-01'))`, which the front end
/// folds to a `RuntimeValue`). `null` is legal (it nulls the call), an
/// integer literal stays, and a constant expression whose type is only
/// known at runtime (`head(['x','1'])`) is handled by the executor's own
/// rejection.
pub(super) fn is_non_integer_literal(expr: &Expr) -> bool {
    matches!(
        static_literal(expr),
        Some(kind) if !matches!(kind, LiteralKind::Int | LiteralKind::Null)
    )
}

/// ADR-0175: reject a non-INTEGER literal at an integer-argument site
/// with the site's authored `InvalidArgumentType` detail — the plan-time
/// half of the type rejection whose runtime half is the executor's own
/// argument coercion.
pub(super) fn reject_non_integer_literal(expr: &Expr, detail: &str) -> Result<()> {
    if is_non_integer_literal(expr) {
        return Err(Error::invalid_argument_type(detail));
    }
    Ok(())
}
