//! The page-bound (`LIMIT`/`SKIP`/`OFFSET`) compile-time rules, in two named
//! tiers: the constant-ness tier (a bound that references a variable or
//! contains a graph pattern is not a constant, 42N28) and the value-domain
//! tier (a statically negative or non-integer literal is a rejection,
//! ADR-0566). The clause shapes apply them differently, and only the
//! projection shapes apply both:
//!
//! - `RETURN` and `WITH` own both tiers ([`check_page_expr`]).
//! - CIP-168's standalone page statement owns the negative rule alone
//!   ([`check_page_negative`]): its corpus contract pins the runtime class
//!   for a non-integer or non-constant bound, so it deliberately skips the
//!   other two checks.

use relate_core::Value;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, PageStage};

use crate::ast::collect::expr_child_reaches_scope;
use crate::ast::tree::try_expr_contains;

use super::static_literal::{require_integer_argument, static_value};

/// Whether an expression references any variable (a parameter is not a
/// reference). Rides the shared scope-reach walk, so a variable inside a
/// `CASE`, `IN`, `IS NULL`, `HAS LABEL`, or map literal counts exactly as it
/// does for every other scope-reach check.
fn expr_references_variables(expr: &Expr) -> Result<bool, EngineError> {
    try_expr_contains(expr, expr_child_reaches_scope, |candidate| {
        matches!(candidate, Expr::Variable(_))
    })
}

/// Whether an expression contains a graph pattern (a pattern comprehension,
/// a pattern predicate, or `shortestPath()`), which Neo4j rejects in
/// LIMIT/SKIP as "not statically inferrable" (42N28).
fn expr_contains_graph_pattern(expr: &Expr) -> Result<bool, EngineError> {
    try_expr_contains(
        expr,
        |_, _| true,
        |candidate| {
            matches!(
                candidate,
                Expr::PatternComprehension(_) | Expr::PatternPredicate(_) | Expr::ShortestPath(_)
            )
        },
    )
}

/// Validate LIMIT/SKIP expressions: reject expressions that reference
/// variables or contain graph patterns (42N28), and statically negative or
/// non-integer literals (ADR-0566).
pub fn validate_limit_skip(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        match clause {
            Clause::Return(rc) => {
                check_page_expr_opt(rc.skip.as_ref(), "SKIP")?;
                check_page_expr_opt(rc.limit.as_ref(), "LIMIT")?;
            }
            Clause::With(wc) => {
                check_page_expr_opt(wc.skip.as_ref(), "SKIP")?;
                check_page_expr_opt(wc.limit.as_ref(), "LIMIT")?;
            }
            // CIP-168: the standalone page statement carries SKIP/OFFSET and
            // LIMIT in their written order (e.g. `MATCH … LIMIT -1 RETURN …`,
            // where the page clause precedes RETURN). Only the negative rule
            // applies at compile time here; the rest keep this clause
            // shape's runtime rejection (see the module doc).
            Clause::StandaloneOrderBy(sob) => {
                for stage in &sob.stages {
                    match stage {
                        PageStage::Skip(e) | PageStage::Limit(e) => check_page_negative(e)?,
                        PageStage::OrderBy(_) => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// One SKIP/LIMIT expression's full rule set — both tiers — for the
/// `RETURN`/`WITH` projection shapes.
fn check_page_expr_opt(expr: Option<&Expr>, what: &str) -> Result<(), EngineError> {
    match expr {
        Some(expr) => check_page_expr(expr, what),
        None => Ok(()),
    }
}

/// The `RETURN`/`WITH` rule set: the constant-ness tier, then the
/// value-domain tier. The standalone page statement applies neither in full
/// (see the module doc).
fn check_page_expr(expr: &Expr, what: &str) -> Result<(), EngineError> {
    check_page_constant(expr, what)?;
    check_page_value_domain(expr, what)
}

/// The constant-ness tier: a bound that references a variable or contains a
/// graph pattern is not a constant (42N28).
fn check_page_constant(expr: &Expr, what: &str) -> Result<(), EngineError> {
    if expr_references_variables(expr)? {
        return Err(EngineError::semantic(
            ErrorKind::NonConstantExpression,
            format!("{what} must be a non-negative integer constant"),
        ));
    }
    if expr_contains_graph_pattern(expr)? {
        return Err(EngineError::semantic(
            ErrorKind::NonConstantExpression,
            format!("Only statically inferrable patterns and variables are allowed in '{what}'"),
        ));
    }
    Ok(())
}

/// The value-domain tier: a statically negative or non-integer literal
/// (direct, parenthesised, or a plan-cache placeholder for one) is a value
/// rejection. The standalone page statement applies only
/// [`check_page_negative`], the negative half.
fn check_page_value_domain(expr: &Expr, what: &str) -> Result<(), EngineError> {
    check_page_negative(expr)?;
    require_integer_argument(expr, what, true)
}

/// The negative-bound rejection alone (a direct sign or a plan-cache
/// placeholder for one) — the standalone page statement's compile-time rule,
/// and half of the `RETURN`/`WITH` value-domain tier. The wording is
/// `relate-core`'s one detail; this layer decides only the class.
fn check_page_negative(expr: &Expr) -> Result<(), EngineError> {
    if matches!(static_value(expr), Some(Value::Int(value)) if value < 0) {
        return Err(EngineError::semantic(
            ErrorKind::NegativeIntegerArgument,
            relate_core::NEGATIVE_PAGE_BOUND_DETAIL,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(query: &str) -> Result<(), EngineError> {
        let stmt = relate_parser::parse(query).expect("parse");
        validate_limit_skip(&stmt.clauses)
    }

    #[test]
    fn page_bounds_reject_negative_literals() {
        for query in [
            // CIP-168's standalone page statement.
            "MATCH (p) LIMIT -1 RETURN p.name",
            "MATCH (p) SKIP -1 RETURN p.name",
            "MATCH (p) OFFSET -1 RETURN p.name",
            "MATCH (n) ORDER BY n.name DESC SKIP 1 LIMIT -1 RETURN n.name AS name",
            // The RETURN/WITH clause shapes share the one rule.
            "MATCH (p) RETURN p.name LIMIT -1",
            "MATCH (p) WITH p SKIP -1 RETURN p.name",
        ] {
            assert!(
                check(query).is_err(),
                "`{query}` must be rejected at compile time"
            );
        }
        for query in [
            "MATCH (p) LIMIT 1 RETURN p.name",
            "MATCH (p) RETURN p.name LIMIT 1",
            "MATCH (p) WITH p SKIP 1 RETURN p.name",
        ] {
            assert!(check(query).is_ok(), "`{query}` must be accepted");
        }
    }

    #[test]
    fn page_bounds_keep_their_expression_and_type_rules() {
        // A non-constant bound and a non-integer literal are the other two
        // rejections the shared expression rule owns.
        let err = check("MATCH (p) RETURN p.name LIMIT p.x").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NonConstantExpression);
        // Every literal spelling outside the integer domain, not just FLOAT.
        for query in [
            "MATCH (p) RETURN p.name LIMIT 1.5",
            "MATCH (p) RETURN p.name LIMIT 'one'",
            "MATCH (p) WITH p SKIP true RETURN p.name",
        ] {
            let err = check(query).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::InvalidArgumentType, "{query}");
        }
        // A variable inside a construct the hand-rolled walker missed still
        // counts as a non-constant bound (the canonical scope-reach walk).
        let err =
            check("MATCH (p) RETURN p.name LIMIT CASE WHEN true THEN 1 ELSE p.x END").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NonConstantExpression);
        // A non-integer literal keeps the standalone page statement's runtime
        // rejection (its corpus contract pins the runtime class).
        assert!(check("MATCH (p) LIMIT 1.5 RETURN p.name").is_ok());
    }
}
