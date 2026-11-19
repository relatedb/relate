//! The pattern-position policy: where a pattern expression is legal —
//! the predicate positions the front end reads (a `WHERE`, the operand
//! of `NOT`, either side of `AND`/`OR`/`XOR`, a `CASE WHEN` condition, a
//! comprehension's WHERE filter) and the legacy `exists(<pattern>)`
//! argument — and where it is a value and rejected (a bare projection
//! item, a SET value, a list element, another function's argument).
//! Neo4j accepts a pattern expression wherever a predicate is read and
//! rejects it wherever a value is; one walk ([`check_no_pattern_in_value_position`])
//! serves every value position, and one WHERE-side walk
//! ([`validate_expr_in_where`]) validates the predicate positions.

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Expr, PatternPath};

use crate::ast::collect::{check_name_defined, check_pattern_vars_allowed, has_relationship};
use crate::ast::tree::{is_boolean_operator, unparen, walk_expr, walk_expr_with};

/// Check that an expression used in WHERE doesn't violate pattern
/// predicate rules: `size(<pattern>)` is illegal anywhere in it, and the
/// pattern predicates in its predicate positions must be validated.
pub(super) fn validate_expr_in_where(
    expr: &Expr,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    check_size_over_pattern(expr)?;
    // Visit exactly the homes [`pattern_home`] names (plus a
    // parenthesized group, which it makes transparent): a pattern
    // predicate is legal there and must be validated (relationship-bearing,
    // bound variables) — including one under a `CASE WHEN` condition, which
    // the former hand-rolled recursion skipped — and a parenthesized single
    // variable used as a predicate is the single-node rejection.
    walk_expr_with(
        expr,
        &mut |e| match e {
            Expr::PatternPredicate(pp) => validate_pattern_predicate(&pp.pattern, scope),
            Expr::Paren(inner) => match inner.as_ref() {
                Expr::Variable(v) => {
                    check_name_defined(v, scope)?;
                    Err(single_node_predicate_error())
                }
                _ => Ok(()),
            },
            _ => Ok(()),
        },
        // The WHERE walk descends every predicate position — for ANY child
        // shape, not only pattern-shaped ones, so its visitor keeps the
        // single-node rejection for a parenthesized variable used as a
        // predicate (`WHERE (n) AND (m)--()`) — plus the legacy
        // `exists(<pattern>)` argument (its variables are enclosing-scope)
        // and parenthesized groups (a group is transparent: its body is a
        // predicate exactly when the group is). A comprehension's WHERE
        // filter is NOT descended: it binds its own variable, so
        // validating its patterns against this clause's scope would
        // false-reject them (`WHERE NOT [n IN [a] WHERE (n)-[:R]->(a) |
        // 1]` — the regression the shape-agnostic descent round
        // introduced; the value-position walk still exempts that slot
        // through pattern_home, where exemption is the right reading).
        |parent, child| {
            predicate_position(parent, child)
                || is_exists_pattern_arg(parent, child)
                || matches!(parent, Expr::Paren(_))
        },
    )
}

/// The `size()` constraint this module owns: `size()` applied directly to
/// a pattern predicate is invalid (UnexpectedSyntax), anywhere in the
/// expression. (size() on a path *variable* is rejected on the lowered IR
/// by the planner's `size()` argument check.) A full-descent pass of its
/// own beside the home-gated walk above: the home-gated walk never
/// reaches value subexpressions, and `WHERE x = size((n)--())` must fail
/// here as it always has.
fn check_size_over_pattern(expr: &Expr) -> Result<(), EngineError> {
    walk_expr(expr, &mut |e| {
        if let Expr::FunctionCall(fc) = e {
            if fc.name.with_str(str::to_lowercase) == "size"
                && fc.args.len() == 1
                && matches!(&fc.args[0], Expr::PatternPredicate(_))
            {
                return Err(EngineError::semantic(
                    ErrorKind::UnexpectedSyntax,
                    "size() cannot be applied to a pattern predicate".to_string(),
                ));
            }
        }
        Ok(())
    })
}

/// Validate a pattern predicate used in WHERE.
/// - Must have at least one relationship (not just a single node).
/// - Must not introduce new unbound variables.
fn validate_pattern_predicate(
    pattern: &PatternPath,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Check: single-node pattern predicate like `(n)` is invalid — the
    // canonical relationship-bearing test, the same depth model the
    // variable walk below uses.
    if !has_relationship(pattern) {
        return Err(single_node_predicate_error());
    }

    // Check for unbound variables introduced by the pattern predicate: the
    // canonical pattern-variable walk gated by the canonical
    // every-name-must-be-allowed fold.
    check_pattern_vars_allowed(std::slice::from_ref(pattern), |v| scope.contains(v))
}

/// The one "a single node is not a valid pattern predicate" rejection: the
/// paren-variable arm and the no-relationship pattern arm report the same
/// shape.
fn single_node_predicate_error() -> EngineError {
    EngineError::semantic(
        ErrorKind::InvalidArgumentType,
        "a single node is not a valid pattern predicate".to_string(),
    )
}

/// The one value-position policy every clause-specific pattern check rides
/// (a `WITH`/`RETURN` projection item, a SET value): reject a pattern
/// predicate everywhere except the predicate homes — a comprehension's
/// WHERE filter, a pattern predicate that *is itself* the sole argument of
/// `exists`, and a (possibly parenthesized) pattern predicate in a
/// predicate position (`NOT`, `AND`/`OR`/`XOR`, a `CASE WHEN` condition).
/// `position` names the value position in the rejection text (`WITH
/// projection`, `RETURN projection`, `SET values`), and the error kind and
/// construction stay owned here. The child must BE the pattern: pruning on
/// the *slot* alone let a pattern buried in a value subexpression of a
/// boolean operand (`(n)--() OR size((m)--())`) escape the rejection.
pub(super) fn check_no_pattern_in_value_position(
    expr: &Expr,
    position: &str,
) -> Result<(), EngineError> {
    walk_expr_with(
        expr,
        &mut |e| {
            if matches!(e, Expr::PatternPredicate(_)) {
                Err(EngineError::semantic(
                    ErrorKind::UnexpectedSyntax,
                    format!("pattern expressions are not allowed in {position}"),
                ))
            } else {
                Ok(())
            }
        },
        |parent, child| !pattern_home(parent, child),
    )
}

/// The one pattern-predicate home vocabulary: a comprehension's WHERE
/// filter, the sole argument of the legacy `exists(<pattern>)` call, and
/// a (possibly parenthesized) pattern predicate in a predicate position
/// (`NOT`, `AND`/`OR`/`XOR`, a `CASE WHEN` condition). The value-position
/// walk exempts exactly these slots. The WHERE walk descends the homes
/// too, but its predicate-position descent is shape-agnostic (any child
/// of a predicate position is visited, so its visitor keeps the
/// single-node rejection for a parenthesized variable). The paren
/// transparency is [`unparen`]'s: a group is transparent for the shape
/// (`NOT ((n)--())` reads the pattern), never for the position (a bare
/// `RETURN ((n)--())` is still a value position).
fn pattern_home(parent: &Expr, child: &Expr) -> bool {
    is_comprehension_where(parent, child)
        || is_exists_pattern_arg(parent, child)
        || (matches!(unparen(child), Expr::PatternPredicate(_))
            && predicate_position(parent, child))
}

/// Whether `child` is the sole argument of the legacy `exists(<pattern>)`
/// call `parent` and is (possibly behind transparent parentheses) the
/// pattern itself: only that exact slot is the legal home for a pattern
/// in a value position. An `exists` over any other expression is
/// descended into — a pattern buried below it (`exists(1 + (n)--())`)
/// is not the legacy form and stays rejected by the walks above.
fn is_exists_pattern_arg(parent: &Expr, child: &Expr) -> bool {
    match parent {
        Expr::FunctionCall(fc)
            if fc.name.with_str(|n| n.eq_ignore_ascii_case("exists")) && fc.args.len() == 1 =>
        {
            std::ptr::eq(&fc.args[0], child) && matches!(unparen(child), Expr::PatternPredicate(_))
        }
        _ => false,
    }
}

/// Whether `child` is the WHERE filter of the comprehension `parent` (by
/// identity, so the filter rides the exact node the walk would otherwise
/// descend into).
fn is_comprehension_where(parent: &Expr, child: &Expr) -> bool {
    let where_expr = match parent {
        Expr::ListComprehension(lc) => lc.where_expr.as_ref(),
        Expr::MapComprehension(mc) => mc.where_expr.as_ref(),
        Expr::PatternComprehension(pc) => pc.where_expr.as_ref(),
        _ => return false,
    };
    where_expr.is_some_and(|w| std::ptr::eq(w, child))
}

/// Whether `child` sits in a position the front end reads as a predicate:
/// the operand of `NOT`, either side of an `AND`/`OR`/`XOR`, or a `CASE
/// WHEN` condition — one arm of the home vocabulary [`pattern_home`]
/// names, which both walks ([`validate_expr_in_where`],
/// [`check_no_pattern_in_value_position`]) share. A pattern expression is
/// legal exactly in the homes and illegal in every value position (a bare
/// projection item, a list element, another function's argument); a
/// parenthesized group is handled at each walk's edge — transparent for
/// the shape through [`unparen`], never for the position.
fn predicate_position(parent: &Expr, child: &Expr) -> bool {
    match parent {
        Expr::Unary(u) if is_boolean_operator(u.op) => std::ptr::eq(&u.expr, child),
        Expr::Binary(b) if is_boolean_operator(b.op) => {
            std::ptr::eq(&b.left, child) || std::ptr::eq(&b.right, child)
        }
        Expr::Case(c) => c
            .whens
            .iter()
            .any(|when| std::ptr::eq(&when.condition, child)),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
