//! The quantifier-variable arithmetic rule's building blocks: which
//! arithmetic operators are guaranteed type errors on non-numeric operands,
//! which list literals are statically non-numeric, and the binder-aware
//! search for an operator applied directly to a bare variable (shared with
//! the `reduce()` rule in [`super::static_reduce`]). The operator-type pass
//! that consumes them lives in [`super::static_types`].

use relate_core::Value;
use relate_error::EngineError;
use relate_names::SmallName;
use relate_parser::{Expr, Statement};

use crate::ast::collect::{walk_expr_scoped, ScopedExprVisitor};

use super::static_types::{
    is_arithmetic_op, non_numeric_leaf_class, static_literal_type, unparen, StaticLitType,
};

/// The arithmetic operators that raise a type error for *every* non-numeric,
/// non-null operand. `+` is deliberately excluded from the
/// [`ARITHMETIC_OPS`] set here: it is also string concatenation (`'a' + 1` →
/// `'a1'`) and list append (`[1] + 2`), both valid for STRING/LIST operands,
/// so `+` is never a *guaranteed* compile-time type error and is left for the
/// runtime evaluator.
pub(super) fn is_failing_arithmetic_op(op: &str) -> bool {
    op != "+" && is_arithmetic_op(op)
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
pub(super) fn list_literal_all_non_numeric(list: &Expr) -> bool {
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

/// True if `expr` applies an operator `is_op` accepts *directly* to the bare
/// variable `var` somewhere in its tree (`x % 2`, `2 - x`, `x ^ 2`, …).
///
/// Rides [`walk_expr_scoped`] — the one table of which construct binds which
/// name and where each scope begins and ends (ADR-0513's collector is the
/// precedent) — so the shadowing rule cannot drift from the resolver's: a
/// nested scope that rebinds `var` (`all(x IN ['a'] WHERE any(x IN [1]
/// WHERE x % 2 = 0))`) shadows the search inside it, because the inner `x`
/// is a different binding. Both static rules that search a body for the
/// shape use this finder; they differ only in the operator policy they
/// pass: the quantifier rule excludes `+` ([`is_failing_arithmetic_op`]),
/// the `reduce`/temporal rule includes it ([`is_arithmetic_op`]).
pub(super) fn applies_arithmetic_to_var(expr: &Expr, var: &str, is_op: fn(&str) -> bool) -> bool {
    /// Records the `var <op> …` shape wherever the binder stack open at
    /// that point does not shadow `var`.
    struct ArithVarSearch<'a> {
        var: &'a str,
        is_op: fn(&str) -> bool,
        binders: Vec<SmallName>,
        found: bool,
    }
    impl ScopedExprVisitor for ArithVarSearch<'_> {
        fn enter_expr(&mut self, expr: &Expr) -> Result<(), EngineError> {
            if let Expr::Binary(b) = expr {
                if (self.is_op)(b.op)
                    && !self.binders.iter().any(|bound| bound == self.var)
                    && (expr_is_bare_var(&b.left, self.var) || expr_is_bare_var(&b.right, self.var))
                {
                    self.found = true;
                }
            }
            Ok(())
        }
        // A read outside expression position — a pattern name, a projection
        // selector — is never the `x <op> …` shape this search records.
        fn read_name(&mut self, _name: &SmallName) -> Result<(), EngineError> {
            Ok(())
        }
        fn bind_name(&mut self, name: &SmallName) -> Result<(), EngineError> {
            self.binders.push(name.clone());
            Ok(())
        }
        // The event stream is well-nested (the trait's contract), so the
        // pop is always the youngest open binder.
        fn unbind_name(&mut self) -> Result<(), EngineError> {
            self.binders.pop();
            Ok(())
        }
        // A subquery body is an opaque leaf to this rule, as it was to the
        // hand-rolled recursion it replaces.
        fn subquery_body(&mut self, _statement: &Statement) -> Result<(), EngineError> {
            Ok(())
        }
    }
    let mut search = ArithVarSearch {
        var,
        is_op,
        binders: Vec::new(),
        found: false,
    };
    match walk_expr_scoped(expr, &mut search) {
        // An over-deep body caps out and defers to the runtime type check,
        // exactly as the self-capping walker this replaces did.
        Err(_) => false,
        Ok(()) => search.found,
    }
}

/// True if `expr` applies an [`is_failing_arithmetic_op`] operator *directly*
/// to the bare quantifier variable `var` — the quantifier policy over
/// [`applies_arithmetic_to_var`].
pub(super) fn predicate_applies_arithmetic_to_var(expr: &Expr, var: &str) -> bool {
    applies_arithmetic_to_var(expr, var, is_failing_arithmetic_op)
}
