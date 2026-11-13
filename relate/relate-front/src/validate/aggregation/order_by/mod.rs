//! ORDER BY aggregation rules, split by concern:
//! - the aggregation-*shape* battery ([`validate_order_by_aggregation`]):
//!   ORDER BY must not use aggregation when the clause itself is not an
//!   aggregating one (no aggregate item, no explicit `GROUP BY`, not
//!   DISTINCT); in an aggregating clause (ADR-0453) an ORDER BY aggregation
//!   expression recomputes over the group — only its non-aggregate parts
//!   are restricted, to simple expressions.
//! - [`projection`]: the post-projection *variable visibility* rules
//!   (LABS-665) — which variables an ORDER BY item or a WITH
//!   `WHERE` may name after a DISTINCT or aggregating projection.

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr};

use super::{is_direct_aggregation, is_projected_item, projection_parts, ProjectionParts};
use crate::agg::{contains_aggregation, projection_aggregates};
use crate::ast::collect::interp_embeds;

mod projection;
pub(super) use self::projection::check_projection_subclauses;

/// The three-way ORDER BY mode of a projection (ADR-0453), classified once
/// from the same inputs every consumer reads: `DISTINCT` (its items are the
/// grouping keys, so ORDER BY is restricted to the orderable columns), an
/// aggregating projection (aggregate item or explicit `GROUP BY`), or a
/// plain projection (whose ORDER BY sees the full pre-projection scope).
enum ProjectionMode {
    Distinct,
    Aggregating,
    Plain,
}

/// Classify a projection's ORDER BY mode once — the single owner of the
/// three-way decision both [`validate_order_by_aggregation`] and
/// [`check_projection_subclauses`] route on.
fn projection_mode(parts: &ProjectionParts) -> ProjectionMode {
    if parts.distinct {
        ProjectionMode::Distinct
    } else if projection_aggregates(parts.items, parts.group_by) {
        ProjectionMode::Aggregating
    } else {
        ProjectionMode::Plain
    }
}

/// Validate that ORDER BY in WITH/RETURN doesn't contain aggregation functions
/// when the clause itself does not aggregate (no aggregate item, no explicit
/// `GROUP BY`, not DISTINCT). In an aggregating clause an ORDER BY
/// aggregation expression is valid wherever its non-aggregate parts are
/// simple expressions (ADR-0453).
pub fn validate_order_by_aggregation(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        let Some(parts) = projection_parts(clause) else {
            continue;
        };
        // One mode per clause, classified once (the per-item gate below is
        // the only per-ORDER-BY decision).
        let mode = projection_mode(&parts);
        for ob in parts.order_by {
            if !contains_aggregation(&ob.expr) {
                continue;
            }
            match mode {
                // No aggregate item, no GROUP BY, no DISTINCT — ORDER BY
                // must not use aggregation.
                ProjectionMode::Plain => {
                    return Err(EngineError::semantic(ErrorKind::InvalidAggregation, "aggregation functions are not allowed in ORDER BY when clause has no aggregation".to_string()));
                }
                // An aggregating or DISTINCT clause (ADR-0453): an ORDER BY
                // aggregation expression recomputes over the group — only
                // its non-aggregate parts are restricted.
                ProjectionMode::Aggregating | ProjectionMode::Distinct => {
                    check_ob_agg_sub_exprs(&ob.expr, parts.items)?;
                }
            }
        }
    }
    Ok(())
}

/// Recursively check an ORDER BY expression that contains aggregation:
/// every aggregation sub-expression is valid (it recomputes over the
/// group, ADR-0453); every non-aggregate part must be simple.
///
/// Entry invariant: `contains_aggregation(expr)`. Every call site — the
/// top-level guard in `validate_order_by_aggregation` and the per-side /
/// per-arg dispatch below — only reaches this function with an expression
/// that contains aggregation.
fn check_ob_agg_sub_exprs(
    expr: &Expr,
    items: &[relate_parser::ReturnItem],
) -> Result<(), EngineError> {
    debug_assert!(contains_aggregation(expr));

    // A direct aggregation call recomputes over the group: valid wherever
    // it appears (its arguments are the aggregation's own inputs).
    if is_direct_aggregation(expr) {
        return Ok(());
    }

    match expr {
        Expr::Binary(b) => check_ob_agg_mixed_sides(&b.left, &b.right, items),
        Expr::Comparison(c) => check_ob_agg_mixed_sides(&c.left, &c.right, items),
        Expr::Unary(u) => check_ob_agg_sub_exprs(&u.expr, items),
        Expr::Paren(inner) => check_ob_agg_sub_exprs(inner, items),
        // Match contains_aggregation's base-only reach, excluding keys/indices/bounds.
        Expr::Property(p) => check_ob_agg_sub_exprs(&p.expr, items),
        Expr::Subscript(s) => check_ob_agg_sub_exprs(&s.expr, items),
        Expr::FunctionCall(fc) => {
            // An aggregation function was already answered above; a
            // non-aggregating call's aggregating arguments recompute over
            // the group like any other aggregation sub-expression.
            for arg in &fc.args {
                if contains_aggregation(arg) {
                    check_ob_agg_sub_exprs(arg, items)?;
                }
            }
            Ok(())
        }
        Expr::StringInterp(parts) => {
            for e in interp_embeds(parts) {
                if contains_aggregation(e) {
                    check_ob_agg_sub_exprs(e, items)?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Two-sided dispatch shared by the Binary and Comparison arms: recurse
/// into an aggregating side, validate a non-aggregating one. The planner's
/// IR lowers comparisons to the same `Expr::Binary` node and checks them
/// identically, so the front end does too (stellar F-3-3 — the missing
/// Comparison arm let a mixed comparison escape the mixed-side validation
/// its Binary equivalent gets).
fn check_ob_agg_mixed_sides(
    left: &Expr,
    right: &Expr,
    items: &[relate_parser::ReturnItem],
) -> Result<(), EngineError> {
    for side in [left, right] {
        if contains_aggregation(side) {
            check_ob_agg_sub_exprs(side, items)?;
        } else {
            validate_ob_non_agg_part(side, items)?;
        }
    }
    Ok(())
}

/// Validate that a non-aggregate part of an ORDER BY expression in an aggregation context
/// is a simple expression.
///
/// Complex expressions (Binary, etc.) are NOT allowed even if they match a grouping key,
/// because Cypher requires referencing grouping keys by alias in ORDER BY when mixed
/// with aggregation. Everything else — variables, property accesses, literals,
/// parameters — is deliberately accepted here without a grouping-key or alias
/// match: the backstops that reject genuinely invalid references are the WITH
/// driver's ORDER BY non-aggregation-variable check in
/// [`validate_aggregation_semantics`](super::validate_aggregation_semantics)
/// (WithOrderBy4 [19]) and the planner's ORDER BY aggregation leaf check,
/// which see the full scope.
fn validate_ob_non_agg_part(
    expr: &Expr,
    items: &[relate_parser::ReturnItem],
) -> Result<(), EngineError> {
    // A non-aggregate side that is verbatim one of the clause's projection
    // items is that grouping key, however complex: `RETURN me.age + you.age,
    // count(*) AS cnt ORDER BY me.age + you.age + count(*)` (ReturnOrderBy6
    // [5]) decomposes the projected expression, which Neo4j allows.
    if is_projected_item(items, expr) {
        return Ok(());
    }
    match expr {
        Expr::Binary(_) => {
            // Complex expression: ambiguous even if it matches a grouping key
            Err(EngineError::semantic(
                ErrorKind::AmbiguousAggregationExpression,
                "complex non-aggregate expressions cannot be mixed with aggregation in ORDER BY"
                    .to_string(),
            ))
        }
        // Deliberate leniency (see the doc comment for the backstops).
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The battery-only level: [`validate_order_by_aggregation`] on a
    /// parsed statement (the driver level is `validate_semantics` in
    /// [`projection`]'s tests).
    fn validate_battery(query: &str) -> Result<(), EngineError> {
        let stmt = relate_parser::parse(query).unwrap();
        validate_order_by_aggregation(&stmt.clauses)
    }

    #[test]
    fn postfix_wrappers_validate_aggregate_bases_only() {
        for order_by in [
            "head(collect(n)).x",
            "collect(n)[0]",
            "collect(n)[..1]",
            // Aggregates in indices/bounds recompute over the group.
            "collect(n)[count(*)]",
            "collect(n)[count(*)..count(*) + 1]",
        ] {
            // An aggregating projection accepts every one of them: each
            // aggregate call, projected or not, recomputes over the group
            // (ADR-0453).
            for projection in ["collect(n) AS xs", "count(*) AS c"] {
                let ok = validate_battery(&format!(
                    "MATCH (n) RETURN {projection} ORDER BY {order_by}"
                ));
                assert!(ok.is_ok(), "{projection} ORDER BY {order_by}: {ok:?}");
            }

            // A non-aggregating, non-DISTINCT projection still rejects
            // aggregation in ORDER BY outright (ReturnOrderBy2 [14]).
            let err = validate_battery(&format!("MATCH (n) RETURN n.x AS c ORDER BY {order_by}"))
                .unwrap_err();
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidAggregation,
                "non-aggregating projection in {order_by}: {err}"
            );
        }
    }

    // ADR-0453: a DISTINCT projection with no aggregate items accepts an
    // aggregating ORDER BY key (the DISTINCT items are the grouping keys).
    #[test]
    fn distinct_projection_accepts_order_by_aggregation() {
        let ok = validate_battery("MATCH (n) RETURN DISTINCT n.x, n.y ORDER BY sum(n.num)");
        assert!(ok.is_ok(), "DISTINCT ORDER BY aggregation: {ok:?}");
        // The complex non-aggregate part restriction still applies.
        let err = validate_battery("MATCH (n) RETURN DISTINCT n.x ORDER BY sum(n.num) + n.a * n.b")
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AmbiguousAggregationExpression);
    }

    // An explicit GROUP BY aggregates the clause even without an aggregate
    // item, so its ORDER BY may aggregate — the same rule the planner's
    // `projection_aggregates` applies to the lowered projection.
    #[test]
    fn explicit_group_by_accepts_order_by_aggregation() {
        for query in [
            "MATCH (n) RETURN n.p GROUP BY n.p ORDER BY count(*)",
            "MATCH (n) WITH n.p AS p GROUP BY p ORDER BY count(*)",
            "MATCH (n) RETURN n.p GROUP BY ALL ORDER BY count(*)",
            "MATCH (n) RETURN n.p GROUP BY () ORDER BY count(*)",
        ] {
            let ok = validate_battery(query);
            assert!(ok.is_ok(), "{query}: {ok:?}");
        }
    }

    // Stellar F-3-3: a mixed comparison under ORDER BY is validated
    // identically to the equivalent Binary form.
    #[test]
    fn comparison_mixed_sides_match_binary_verdicts() {
        // Rejected: `n.x * 2` parses as a bare `Expr::Binary`, so the
        // non-aggregate side is a complex expression — the Binary form has
        // always rejected this shape as ambiguous, and the Comparison form
        // now rejects it with the same error kind.
        let binary = validate_battery("MATCH (n) RETURN count(*) AS c ORDER BY count(*) + n.x * 2");
        let comparison =
            validate_battery("MATCH (n) RETURN count(*) AS c ORDER BY count(*) > n.x * 2");
        for (shape, result) in [("Binary", binary), ("Comparison", comparison)] {
            let err = result.unwrap_err();
            assert_eq!(
                err.kind(),
                ErrorKind::AmbiguousAggregationExpression,
                "{shape} form must reject a complex non-aggregate side, got: {err}"
            );
        }

        // Accepted: `(n.x + 1)` parses as `Expr::Paren`, which the
        // non-aggregate-part check deliberately lets through (see
        // `validate_ob_non_agg_part`), and an aggregate on the other side
        // is fine — both forms accept.
        let binary =
            validate_battery("MATCH (n) RETURN count(*) AS c ORDER BY count(*) + (n.x + 1)");
        let comparison =
            validate_battery("MATCH (n) RETURN count(*) AS c ORDER BY count(*) > (n.x + 1)");
        assert!(binary.is_ok(), "Binary form: {binary:?}");
        assert!(comparison.is_ok(), "Comparison form: {comparison:?}");

        // Accepted (ADR-0453): a non-projected aggregate beside a simple
        // non-aggregate part recomputes over the group for both forms.
        let binary = validate_battery("MATCH (n) RETURN count(*) AS c ORDER BY sum(n.x) + c");
        let comparison = validate_battery("MATCH (n) RETURN count(*) AS c ORDER BY sum(n.x) > c");
        assert!(binary.is_ok(), "Binary form: {binary:?}");
        assert!(comparison.is_ok(), "Comparison form: {comparison:?}");
    }
}
