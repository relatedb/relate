//! The quantifier-variable arithmetic rule: its decision
//! ([`check_quantifier_arithmetic`]), the arithmetic operators guaranteed to
//! be type errors on non-numeric operands, which list literals are statically
//! non-numeric, and the binder-aware search for an operator applied directly
//! to a bare variable (shared with the `reduce()` rule in
//! [`super::static_reduce`]). The operator dispatcher in
//! [`super::static_types`] calls the decision.

use relate_core::LiteralValueKind;
use relate_error::{EngineError, ErrorKind};
use relate_names::SmallName;
use relate_parser::{Expr, QuantifierExpr};

use crate::ast::collect::{walk_expr_scoped, BinderStack, ScopedExprVisitor};
use crate::ast::tree::unparen;

use super::static_literal::{is_arithmetic_op, non_numeric_leaf_class, static_list_element_kinds};

/// The arithmetic operators that raise a type error for *every* non-numeric,
/// non-null operand. `+` is deliberately excluded from the
/// arithmetic-operator set ([`is_arithmetic_op`]): it is also string concatenation (`'a' + 1` →
/// `'a1'`) and list append (`[1] + 2`), both valid for STRING/LIST operands,
/// so `+` is never a *guaranteed* compile-time type error and is left for the
/// runtime evaluator.
fn is_failing_arithmetic_op(op: &str) -> bool {
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
    static_list_element_kinds(list).is_some_and(non_empty_all_non_numeric)
}

/// The quantifier arithmetic rule's decision, beside every piece it reads: a
/// quantifier over a list of statically non-numeric literal elements whose
/// predicate applies a failing arithmetic operator directly to the loop
/// variable is a compile-time type error — e.g.
/// `all(x IN ['Clara'] WHERE x % 2 = 0)`
/// (expressions/quantifier/Quantifier1-4 scenario "[15]/[16] Fail …
/// quantifier on type mismatch between list elements and predicate",
/// `SyntaxError … : InvalidArgumentType` at compile time). Relate otherwise
/// catches it at runtime in `eval::binary`; this narrow static pass moves the
/// statically-decidable case to compile time. Only literal lists with
/// fully-known non-numeric element types qualify, so every query whose element
/// type depends on runtime data is untouched.
pub(super) fn check_quantifier_arithmetic(q: &QuantifierExpr) -> Result<(), EngineError> {
    let Some(pred) = &q.predicate else {
        return Ok(());
    };
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
    Ok(())
}

/// The empty-list rule and the all-non-numeric rule share this one iterator
/// predicate: a non-empty list whose every element has a known non-numeric
/// literal kind.
fn non_empty_all_non_numeric(classes: impl IntoIterator<Item = Option<LiteralValueKind>>) -> bool {
    let mut classes = classes.into_iter();
    classes
        .next()
        .is_some_and(|first| first.is_some_and(non_numeric_leaf_class))
        && classes.all(|class| class.is_some_and(non_numeric_leaf_class))
}

/// True if `e`, ignoring wrapping parentheses, is exactly the bare variable
/// `var`.
pub(super) fn expr_is_bare_var(e: &Expr, var: &SmallName) -> bool {
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
        var: &'a SmallName,
        is_op: fn(&str) -> bool,
        binders: BinderStack,
        found: bool,
    }
    impl ScopedExprVisitor for ArithVarSearch<'_> {
        fn enter_expr(&mut self, expr: &Expr) -> Result<(), EngineError> {
            if let Expr::Binary(b) = expr {
                if (self.is_op)(b.op)
                    && !self.binders.shadows(self.var)
                    && (expr_is_bare_var(&b.left, self.var) || expr_is_bare_var(&b.right, self.var))
                {
                    self.found = true;
                }
            }
            Ok(())
        }
        fn bind_name(&mut self, name: &SmallName) -> Result<(), EngineError> {
            self.binders.bind(name);
            Ok(())
        }
        fn unbind_name(&mut self) -> Result<(), EngineError> {
            self.binders.unbind();
            Ok(())
        }
    }
    let var = SmallName::from_name(var);
    let mut search = ArithVarSearch {
        var: &var,
        is_op,
        binders: BinderStack::default(),
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
