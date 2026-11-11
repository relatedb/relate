//! Compile-time checks decidable from *literal* operand types, and the
//! literal-type vocabulary they share: boolean-operator operands, `IN` list
//! operands, `properties()` argument types, CIP-271's `toString()`
//! argument-type rejection, and quantifier-variable arithmetic over a
//! statically non-numeric list literal. Page bounds live in
//! [`super::page_bounds`]; the builtin-call families live in
//! [`super::static_calls`]. Everything whose type depends on runtime data is
//! deliberately left to the runtime evaluator.

use relate_core::Value;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, LiteralValue};

use crate::ast::tree::{expr_contains, is_boolean_operator, walk_expr};

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

/// The boolean-operand rule's predicate, shared by the operator gate here and
/// `static_calls`' `allReduce()` predicate check: a statically-known type
/// that is neither BOOLEAN nor NULL.
pub(super) fn is_statically_non_boolean(expr: &Expr) -> bool {
    static_type_outside(expr, BOOLEAN_OPERAND_TYPES).is_some()
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
fn homogeneous_list_literal_class(list: &Expr) -> Option<StaticLitType> {
    match unparen(list) {
        Expr::List(items) if !items.is_empty() => {
            let mut classes = items.iter().map(static_literal_class);
            let first = classes.next()??;
            classes.all(|class| class == Some(first)).then_some(first)
        }
        _ => None,
    }
}

/// The result type of a binary operator over statically-typed operands, or
/// `None` when it is not decided (a non-numeric operand of an arithmetic
/// operator, an unknown operator, an operand of unknown type).
fn static_binary_type(op: &str, left: &Expr, right: &Expr) -> Option<StaticLitType> {
    match op {
        "AND" | "OR" | "XOR" => Some(StaticLitType::Boolean),
        "+" | "-" | "*" | "/" | "%" | "^" => {
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

/// The types a boolean operand admits: BOOLEAN, plus NULL (three-valued
/// logic). One list, consulted by the operator gate and
/// [`is_statically_non_boolean`].
const BOOLEAN_OPERAND_TYPES: &[StaticLitType] = &[StaticLitType::Boolean, StaticLitType::Null];

/// Reject a boolean-operator operand whose *literal* type is neither Boolean nor
/// Null. A `null` operand is always allowed (three-valued logic); a non-literal
/// operand is left for runtime. A list *literal* stays a compile-time error
/// (openCypher TCK Boolean1-3 [8]); only a runtime list (a parameter, a
/// function result) takes the deprecated list-to-boolean conversion.
fn require_boolean_literal_operand(operand: &Expr) -> Result<(), EngineError> {
    require_static_type(operand, BOOLEAN_OPERAND_TYPES, |ty| {
        format!("Type mismatch: expected Boolean but was {}", ty.name())
    })
}

/// Compile-time type checking for operators/functions whose operand type is
/// statically knowable from a *literal*. Neo4j/openCypher rejects these during
/// semantic analysis (a compile-time `SyntaxError: InvalidArgumentType`), not at
/// runtime — so `RETURN NOT [1]`, `RETURN 1 IN 2`, and
/// `RETURN properties(1)` are compile errors. This validator reproduces exactly
/// that boundary and *only* for literal operands; every non-literal operand
/// (variable, parameter, function result, …) is left to the runtime evaluator,
/// so no valid query is affected.
///
/// `cypher_version` is the statement's CIP-150 preamble: CIP-271's container
/// acceptance is Cypher-25-only, so the `toString()` gate stands down under an
/// explicit `CYPHER 5` (where the runtime rejects every container, and
/// `toStringOrNull()` maps that rejection to null).
///
/// Covered (all `expressions/…` TCK features):
/// - `NOT` / `AND` / `OR` / `XOR` require boolean operands (Boolean1-4).
/// - `x IN <list>` requires a list right operand (List5 scenario 42).
/// - `properties(x)` requires a node/relationship/map (Graph9 scenarios 5-7).
/// - `toString()`/`toStringOrNull()` reject a `LIST<T>` literal (CIP-271).
pub fn validate_static_operator_types(
    clauses: &[Clause],
    cypher_version: Option<u32>,
) -> Result<(), EngineError> {
    let mut check = |expr: &Expr| check_static_operator_types(expr, cypher_version);
    for clause in clauses {
        match clause {
            Clause::Return(rc) => {
                for item in &rc.items {
                    if !item.all {
                        walk_expr(&item.expr, &mut check)?;
                    }
                }
                for ob in &rc.order_by {
                    walk_expr(&ob.expr, &mut check)?;
                }
            }
            Clause::With(wc) => {
                for item in &wc.items {
                    if !item.all {
                        walk_expr(&item.expr, &mut check)?;
                    }
                }
                if let Some(w) = &wc.where_expr {
                    walk_expr(w, &mut check)?;
                }
                for ob in &wc.order_by {
                    walk_expr(&ob.expr, &mut check)?;
                }
            }
            Clause::Match(mc) => {
                if let Some(w) = &mc.where_expr {
                    walk_expr(w, &mut check)?;
                }
            }
            Clause::Unwind(uc) => {
                walk_expr(&uc.expr, &mut check)?;
            }
            Clause::Set(sc) => {
                for item in &sc.items {
                    if let Some(val) = &item.value {
                        walk_expr(val, &mut check)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The per-node visitor for [`validate_static_operator_types`].
fn check_static_operator_types(e: &Expr, cypher_version: Option<u32>) -> Result<(), EngineError> {
    match e {
        Expr::Unary(u) if is_boolean_operator(u.op) => require_boolean_literal_operand(&u.expr)?,
        Expr::Binary(b) if is_boolean_operator(b.op) => {
            require_boolean_literal_operand(&b.left)?;
            require_boolean_literal_operand(&b.right)?;
        }
        Expr::In(ie) => {
            require_static_type(
                &ie.list,
                &[StaticLitType::List, StaticLitType::Null],
                |ty| format!("IN requires a list, got {}", ty.name()),
            )?;
        }
        Expr::FunctionCall(fc)
            if fc.name.with_str(|n| n.eq_ignore_ascii_case("properties")) && fc.args.len() == 1 =>
        {
            require_static_type(
                &fc.args[0],
                &[StaticLitType::Map, StaticLitType::Null],
                |ty| {
                    format!(
                        "properties() requires a node, relationship, or map, got {}",
                        ty.name()
                    )
                },
            )?;
        }
        // CIP-271's static half: Neo4j's type checker rejects the `toString`
        // family on a `LIST<T>` with a concrete `T` — a list literal whose
        // elements all share one known class — while a dynamically typed list
        // (a property/parameter read, statically `LIST<ANY>`) is accepted and
        // renders in display form. The value-level converter cannot see the
        // argument expression, so the shape decision lives here, beside the
        // other static-literal gates; `EXPLAIN` and a zero-row CASE arm fail
        // with the rest of semantic analysis.
        Expr::FunctionCall(fc)
            if fc.args.len() == 1
                && fc.name.with_str(|n| {
                    n.eq_ignore_ascii_case("tostring") || n.eq_ignore_ascii_case("tostringornull")
                }) =>
        {
            if !relate_core::is_cypher5(cypher_version)
                && homogeneous_list_literal_class(&fc.args[0]).is_some()
            {
                return Err(EngineError::semantic(
                    ErrorKind::InvalidArgumentValue,
                    "toString()",
                ));
            }
        }
        // A list quantifier (`all`/`any`/`none`/`single`) whose loop variable
        // ranges over a list *literal* of statically non-numeric elements, and
        // whose predicate applies an arithmetic operator directly to that loop
        // variable, is a compile-time type error in openCypher — e.g.
        // `all(x IN ['Clara'] WHERE x % 2 = 0)`
        // (expressions/quantifier/Quantifier1-4 scenario "[15]/[16] Fail …
        // quantifier on type mismatch between list elements and predicate",
        // `SyntaxError … : InvalidArgumentType` at compile time). Relate
        // otherwise catches it at runtime in `eval::binary`; this narrow static
        // pass moves the statically-decidable case to compile time. Only literal
        // lists with fully-known non-numeric element types qualify, so every
        // query whose element type depends on runtime data is untouched.
        Expr::Quantifier(q) => {
            if let Some(pred) = &q.predicate {
                if list_literal_all_non_numeric(&q.list)
                    && q.variable
                        .with_str(|text| predicate_applies_arithmetic_to_var(pred, text))
                {
                    return Err(EngineError::semantic(
                        ErrorKind::InvalidArgumentType,
                        format!(
                            "Type mismatch: the quantifier variable '{}' ranges over a list of \
                             non-numeric elements and cannot be used in arithmetic",
                            q.variable
                        ),
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The arithmetic operators that raise a type error for *every* non-numeric,
/// non-null operand. `+` is deliberately excluded: it is also string
/// concatenation (`'a' + 1` → `'a1'`) and list append (`[1] + 2`), both valid
/// for STRING/LIST operands, so `+` is never a *guaranteed* compile-time type
/// error and is left for the runtime evaluator.
fn is_failing_arithmetic_op(op: &str) -> bool {
    matches!(op, "-" | "*" | "/" | "%" | "^")
}

/// True if `list`, ignoring wrapping parentheses, is a non-empty list *literal*
/// (`Expr::List`) whose every element is a statically-typed, non-null,
/// NON-NUMERIC literal (STRING / BOOLEAN / nested LIST / MAP). When this holds,
/// a quantifier binding a variable to the list's elements gives that variable a
/// value on which the [`is_failing_arithmetic_op`] operators must raise, and
/// that is knowable at compile time.
///
/// Any numeric element, `null` element, or element whose type is not statically
/// a literal (a variable, a caller's parameter, a function result, …) makes
/// the outcome runtime-dependent — arithmetic on a number succeeds, and on
/// `null` yields `null` — so the list does not qualify and the query is left
/// untouched. A plan-cache placeholder is the list it replaced.
fn list_literal_all_non_numeric(list: &Expr) -> bool {
    match unparen(list) {
        Expr::List(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|e| static_literal_type(e).is_some_and(non_numeric_leaf_class))
        }
        // The list a plan-cache placeholder replaced. Its elements are the
        // literals, so `all(x IN ['Clara'] WHERE x % 2 = 0)` still fails
        // here instead of at runtime.
        Expr::Parameter(name) => match relate_core::auto_literal(name) {
            Some(Value::List(items)) if !items.is_empty() => items
                .iter()
                .all(|item| StaticLitType::of_value(item).is_some_and(non_numeric_leaf_class)),
            _ => false,
        },
        _ => false,
    }
}

/// True if `e`, ignoring wrapping parentheses, is exactly the bare variable
/// `var`.
pub(super) fn expr_is_bare_var(e: &Expr, var: &str) -> bool {
    matches!(unparen(e), Expr::Variable(v) if v == var)
}

/// True if `expr` applies an [`is_failing_arithmetic_op`] operator *directly* to
/// the bare quantifier variable `var` somewhere in its tree (`x % 2`, `2 - x`,
/// `x ^ 2`, …).
///
/// Rides the shared self-capping walker ([`expr_contains`]) with a visit
/// predicate for the `x <op> …` shape and an edge filter reproducing the
/// historical hand-rolled recursion's reach — in particular the
/// binder-shadowing rule: the walk stops at any nested scope-binding
/// construct (quantifier / list comprehension / reduce / allReduce) that
/// *rebinds* the same variable name, so shadowing (`all(x IN ['a'] WHERE
/// any(x IN [1] WHERE x % 2 = 0))`) never yields a false positive: the
/// inner `x` is a different, numeric binding. A construct the filter does
/// not enter merely defers to the runtime type check, which is safe; only
/// a false positive (rejecting a valid query) would be a regression, and
/// requiring the operand to be the *bare* loop variable prevents that.
fn predicate_applies_arithmetic_to_var(expr: &Expr, var: &str) -> bool {
    // The binder edges: from a scope-binding construct, the walk enters
    // the bound body only while the construct does not rebind `var` (the
    // iterated list and the reduce init expression bind nothing, so they
    // are always entered).
    let enter_child = |parent: &Expr, child: &Expr| match parent {
        Expr::Quantifier(q) => {
            std::ptr::eq(child, &q.list)
                || (q.variable != var
                    && q.predicate.as_ref().is_some_and(|p| std::ptr::eq(child, p)))
        }
        Expr::ListComprehension(lc) => {
            std::ptr::eq(child, &lc.list_expr)
                || (lc.variable != var
                    && (lc
                        .where_expr
                        .as_ref()
                        .is_some_and(|w| std::ptr::eq(child, w))
                        || lc.map_expr.as_ref().is_some_and(|m| std::ptr::eq(child, m))))
        }
        Expr::Reduce(r) => {
            std::ptr::eq(child, &r.init_expr)
                || std::ptr::eq(child, &r.list_expr)
                || (r.variable != var && r.accumulator != var && std::ptr::eq(child, &r.expression))
        }
        Expr::AllReduce(r) => {
            std::ptr::eq(child, &r.init_expr)
                || std::ptr::eq(child, &r.list_expr)
                || (r.variable != var
                    && r.accumulator != var
                    && (std::ptr::eq(child, &r.expression) || std::ptr::eq(child, &r.predicate)))
        }
        // Leaves the hand-rolled recursion never opened: map and pattern
        // comprehensions and map projections evaluate their contents
        // against their own bindings, and the preview-gated chronological
        // comparisons postdate this rule's reach. (Subquery bodies are
        // opaque leaves to the walker itself.)
        Expr::MapComprehension(_)
        | Expr::PatternComprehension(_)
        | Expr::MapProjection(_)
        | Expr::ChronologicalCompare(_) => false,
        // Every other parent's expression children — operands, arguments,
        // list/map items, CASE arms, property keys, subscript parts — were
        // all visited by the hand-rolled recursion.
        _ => true,
    };
    expr_contains(expr, enter_child, |node| {
        matches!(node,
            Expr::Binary(b)
                if is_failing_arithmetic_op(b.op)
                    && (expr_is_bare_var(&b.left, var) || expr_is_bare_var(&b.right, var)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 22N27/22NB1 detail text renders a folded operand's type through
    /// core's `bare_type_name` and a declined-but-typed one through
    /// [`StaticLitType::name`]: one vocabulary, two call sites. A spelling
    /// change on either side must fail here. `List` sits outside the overlap
    /// by design — the front renders the *kind* (`LIST`), core renders a
    /// list value's element union (`LIST<INTEGER>`), and a folded list always
    /// takes the core path.
    #[test]
    fn static_type_names_match_the_core_vocabulary() {
        use relate_core::bare_type_name;

        for (front, core) in [
            (StaticLitType::Null, Value::Null),
            (StaticLitType::Boolean, Value::Bool(true)),
            (StaticLitType::Integer, Value::Int(0)),
            (StaticLitType::Float, Value::Float(0.0)),
            (StaticLitType::String, Value::String("".into())),
            (StaticLitType::Map, Value::Map(Default::default())),
        ] {
            assert_eq!(front.name(), bare_type_name(&core), "{front:?}");
        }
    }

    fn expr_of(query: &str) -> Expr {
        let stmt = relate_parser::parse(query).unwrap();
        match &stmt.clauses[0] {
            Clause::Return(rc) => rc.items[0].expr.clone(),
            _ => panic!("expected a RETURN clause"),
        }
    }
    // ---- Quantifier over a non-numeric list literal: compile-time TypeError ----
    //
    // openCypher rejects `all/any/none/single(x IN <non-numeric list literal>
    // WHERE <arith on x>)` during semantic analysis, as a `SyntaxError … :
    // InvalidArgumentType` at compile time (expressions/quantifier/Quantifier1-4
    // "[15]/[16] Fail … quantifier on type mismatch between list elements and
    // predicate"). Relate's runtime evaluator would otherwise raise the same
    // TypeError one phase too late.

    /// Parse `query`, run only the narrow static operator/type pass, and return
    /// its verdict.
    fn parse_and_check_static_ops(query: &str) -> Result<(), EngineError> {
        let stmt = relate_parser::parse(query).expect("parse");
        validate_static_operator_types(&stmt.clauses, stmt.cypher_version)
    }

    #[test]
    fn quantifier_over_string_literal_arith_predicate_is_compile_time_type_error() {
        // Every quantifier keyword, over both a STRING and a BOOLEAN list literal
        // (the exact Quantifier1-4 [15]/[16] examples). Each must be a *compile*
        // (Semantic) error whose kind is InvalidArgumentType — the detail the
        // TCK step pins.
        for q in ["all", "any", "none", "single"] {
            for list in [
                "['Clara']",
                "['Clara', 'Bob', 'Dave', 'Alice']",
                "[false, true]",
            ] {
                let query = format!("RETURN {q}(x IN {list} WHERE x % 2 = 0) AS result");
                let err = parse_and_check_static_ops(&query)
                    .expect_err(&format!("expected a compile-time type error for `{query}`"));
                assert!(
                    matches!(err, EngineError::Semantic { .. }),
                    "`{query}` must fail at compile time (Semantic), got {err:?}"
                );
                assert_eq!(
                    err.kind(),
                    ErrorKind::InvalidArgumentType,
                    "`{query}` must carry the InvalidArgumentType detail"
                );
                assert_eq!(err.tck_class(), relate_error::TckErrorClass::TypeError);
            }
        }
    }

    #[test]
    fn quantifier_over_numeric_literal_is_untouched() {
        // The numeric-list quantifier the tested type-mismatch scenarios are the
        // negative of: `x % 2 = 0` over integers/floats is perfectly valid and
        // must pass the static pass unchanged (no false positive).
        for query in [
            "RETURN all(x IN [1, 2] WHERE x % 2 = 0) AS result",
            "RETURN any(x IN [1.5, 2.5] WHERE x / 2 > 0) AS result",
            "RETURN none(x IN [1, 2, 3] WHERE x - 1 = 0) AS result",
            "RETURN single(x IN [10] WHERE x * 2 = 20) AS result",
            // A list mixing numbers and null is not statically all-non-numeric.
            "RETURN all(x IN [1, null, 3] WHERE x % 2 = 0) AS result",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "`{query}` must not be rejected by the static pass"
            );
        }
    }

    #[test]
    fn quantifier_non_arithmetic_predicate_over_strings_is_untouched() {
        // The *valid* Quantifier[5]/[6]/[7]/[11] scenarios use `size(x)`, `x.a`,
        // `x IS NULL`, `x = …` — never arithmetic on the loop variable — and must
        // not be mistaken for the type-mismatch case.
        for query in [
            "RETURN all(x IN ['abc', 'ef'] WHERE size(x) = 3) AS result",
            "RETURN any(x IN [{a: 2}, {a: 4}] WHERE x.a = 2) AS result",
            "RETURN none(x IN ['a', 'b'] WHERE x IS NULL) AS result",
            "RETURN single(x IN [[1, 2, 3]] WHERE size(x) = 3) AS result",
            // `+` on strings is concatenation, not arithmetic — never a
            // compile-time error.
            "RETURN all(x IN ['a', 'b'] WHERE x + 'y' = 'ay') AS result",
            // Arithmetic applies to a *derived* value, not the bare loop var.
            "RETURN all(x IN ['1', '2'] WHERE toInteger(x) % 2 = 0) AS result",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "`{query}` must not be rejected by the static pass"
            );
        }
    }

    #[test]
    fn quantifier_shadowing_inner_numeric_var_is_not_a_false_positive() {
        // The inner `any` rebinds `x` to a numeric list; the outer `all`'s
        // string binding never reaches the inner `x % 2`. The shadow guard in
        // `predicate_applies_arithmetic_to_var` must keep this valid.
        let query = "RETURN all(x IN ['a'] WHERE any(x IN [1, 2] WHERE x % 2 = 0)) AS result";
        assert!(
            parse_and_check_static_ops(query).is_ok(),
            "shadowed inner numeric quantifier must not trip the outer static check"
        );
    }

    #[test]
    fn quantifier_static_helpers_are_narrow() {
        // list_literal_all_non_numeric: only fully-known non-numeric literals.
        let all_non_numeric =
            |q: &str| list_literal_all_non_numeric(&expr_of(&format!("RETURN {q}")));
        assert!(all_non_numeric("['a', 'b']"));
        assert!(all_non_numeric("[false, true]"));
        assert!(all_non_numeric("[[1], ['a']]"));
        assert!(all_non_numeric("[{a: 1}]"));
        assert!(!all_non_numeric("[]")); // empty ⇒ nothing to constrain
        assert!(!all_non_numeric("[1, 2]")); // numeric
        assert!(!all_non_numeric("['a', 1]")); // heterogeneous ⇒ runtime-dependent
        assert!(!all_non_numeric("['a', null]")); // null ⇒ arithmetic yields null
        assert!(!all_non_numeric("$p")); // not a list literal

        // predicate_applies_arithmetic_to_var: bare-var operand under a failing op.
        let hits = |q: &str| {
            let e = expr_of(&format!("RETURN {q}"));
            predicate_applies_arithmetic_to_var(&e, "x")
        };
        assert!(hits("x % 2 = 0"));
        assert!(hits("2 - x > 0"));
        assert!(hits("x ^ 2 = 4"));
        assert!(hits("x = 1 OR x / 2 = 0"));
        assert!(!hits("x + 'y' = 'z'")); // `+` excluded (concat/append)
        assert!(!hits("size(x) = 3")); // no arithmetic on x
        assert!(!hits("toInteger(x) % 2 = 0")); // operand is derived, not bare x
        assert!(!hits("y % 2 = 0")); // different variable
    }

    #[test]
    fn quantifier_type_mismatch_surfaces_through_full_engine_at_compile_time() {
        // End-to-end through the engine: the query must error, as a compile
        // (Semantic) InvalidArgumentType, before any row evaluation.
        // Compile-time validation: a static list of strings under `%` is InvalidArgumentType.
        let err =
            parse_and_check_static_ops("RETURN all(x IN ['Clara'] WHERE x % 2 = 0) AS result")
                .expect_err("quantifier type mismatch must error");
        assert_eq!(err.kind(), ErrorKind::InvalidArgumentType);
        assert!(
            matches!(err, EngineError::Semantic { .. }),
            "must be a compile-time (Semantic) error, got {err:?}"
        );
        assert_eq!(err.kind(), ErrorKind::InvalidArgumentType);
    }

    // ---- CIP-271: toString()/toStringOrNull() reject a homogeneous LIST<T> ----

    #[test]
    fn tostring_rejects_a_homogeneous_list_literal_at_compile_time() {
        // The four shapes Neo4j's checker rejects: homogeneous INTEGER,
        // STRING and nested-LIST literals, plus the mixed number list (INTEGER
        // and FLOAT are one class). Both spellings of the function share the
        // rule; the error is the InvalidArgumentValue detail CIP-271 pins.
        for query in [
            "RETURN toString([1, 2]) AS result",
            "RETURN toString(['a', 'b']) AS result",
            "RETURN toString([1, 2.0]) AS result",
            "RETURN toString([[1]]) AS result",
            "RETURN toString([1 + 1, 2]) AS result",
            "RETURN toStringOrNull([1, 2]) AS result",
            "RETURN toStringOrNull(['a', 'b']) AS result",
        ] {
            let err = parse_and_check_static_ops(query)
                .expect_err(&format!("`{query}` must be rejected statically"));
            assert!(
                matches!(err, EngineError::Semantic { .. }),
                "`{query}` must fail at compile time (Semantic), got {err:?}"
            );
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidArgumentValue,
                "`{query}` must carry the InvalidArgumentValue detail"
            );
            assert_eq!(err.tck_class(), relate_error::TckErrorClass::TypeError);
        }
    }

    #[test]
    fn tostring_accepts_a_dynamic_or_mixed_list() {
        // `LIST<ANY>` (or a non-list): empty, mixed classes, a null element, a
        // variable/property read, and a list whose elements the static pass
        // cannot classify all fall through to the runtime display renderer.
        for query in [
            "RETURN toString([]) AS result",
            "RETURN toString([1, 'a']) AS result",
            "RETURN toString([1, null, 'a']) AS result",
            "RETURN toString([true, null]) AS result",
            "RETURN toString($p) AS result",
            "UNWIND [[1, 2]] AS xs RETURN toString(xs) AS result",
            "MATCH (n) RETURN toString(n.ids) AS result",
            "RETURN toString(1) AS result",
            "RETURN toString({a: 1}) AS result",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "`{query}` must not be rejected by the static pass"
            );
        }
    }

    #[test]
    fn tostring_static_gate_stands_down_under_cypher_5() {
        // CIP-271 is Cypher-25-only; under an explicit `CYPHER 5` preamble
        // the runtime owns the container contract (toString raises,
        // toStringOrNull returns null), so the static gate must not fire.
        for query in [
            "CYPHER 5 RETURN toString([1, 2]) AS result",
            "CYPHER 5 RETURN toStringOrNull([1, 2]) AS result",
        ] {
            assert!(
                parse_and_check_static_ops(query).is_ok(),
                "`{query}` must be left to the runtime under CYPHER 5"
            );
        }
    }

    #[test]
    fn genuinely_runtime_arithmetic_type_error_stays_runtime_typeerror() {
        // A non-static operand (bound at row time by UNWIND) is NOT caught by the
        // static pass — it must still raise at RUNTIME, now correctly classed as
        // a TypeError (InvalidArgumentType) rather than the old Uncategorized/
        // SemanticError.
        // A non-static operand is NOT caught by the static pass.
        assert!(parse_and_check_static_ops("UNWIND ['abc'] AS x RETURN x % 2").is_ok());
    }
}
