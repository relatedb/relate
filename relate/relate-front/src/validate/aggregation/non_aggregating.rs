//! Aggregation in the expression positions that cannot aggregate.
//!
//! Neo4j rejects an aggregate in a list/map/pattern comprehension
//! projection or filter, a quantifier predicate, a `reduce()` step
//! expression, an `UNWIND` source, or a `WHEN` predicate at compile
//! time (42I24 under top-
//! level 42001 — an `InvalidAggregation`/`SyntaxError` here). Relate used
//! to let several of them reach the executor's scalar-eval backstop, which
//! both mis-classified the error and (before that backstop ran) could matter
//! for error precedence; the SemanticErrorPart2 "aggregation inside looping
//! expressions" battery pins the class.
//!
//! The iteration *source* of a comprehension/quantifier/`reduce()` is
//! aggregation-transparent (`all(x IN collect(l) WHERE x > 0)` is legal,
//! live-verified); only the per-element expression/predicate is checked.
//!
//! Two entry points, one policy, each covering a distinct position class:
//!
//! - [`validate_expression_slots`] judges ONE expression node; the
//!   whole-tree walk's expression visitor calls it for every node of the
//!   statement at every depth, so every clause's expressions (CREATE/MERGE
//!   property maps, SET values, DELETE/REMOVE targets, CALL arguments, …) are
//!   covered without a second clause enumeration here.
//! - [`validate_clause_slots`] owns the two *clause-level*
//!   slots — an `UNWIND` source and a CIP-135 `WHEN` predicate — which are
//!   not expression-node slots, so the expression visitor cannot see them.
//!
//! The reject test is [`contains_aggregation_anywhere`] (the full reach), so
//! an aggregate nested in an `IN` operand, a literal, a `CASE` arm or a
//! dynamic key is found too.

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr};

use crate::agg::contains_aggregation_anywhere;

/// The clause-level half: the `UNWIND` source and the CIP-135 `WHEN`
/// predicate. Everything else is an expression-node slot judged by
/// [`validate_expression_slots`].
pub fn validate_clause_slots(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        match clause {
            Clause::Unwind(unwind) => {
                reject_aggregate_in(&unwind.expr, "an UNWIND source")?;
            }
            Clause::Conditional(conditional) => {
                for branch in &conditional.branches {
                    reject_aggregate_in(&branch.predicate, "a WHEN predicate")?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The expression-node half: judge one node's known non-aggregating slots.
/// Called once per expression node by the whole-tree walk.
pub fn validate_expression_slots(expr: &Expr) -> Result<(), EngineError> {
    match expr {
        Expr::ListComprehension(comprehension) => {
            let slot = "a list comprehension";
            reject_optional_aggregate_in(comprehension.map_expr.as_ref(), slot)?;
            reject_optional_aggregate_in(comprehension.where_expr.as_ref(), slot)
        }
        Expr::MapComprehension(comprehension) => {
            let slot = "a map comprehension";
            reject_aggregate_in(&comprehension.key_expr, slot)?;
            reject_aggregate_in(&comprehension.value_expr, slot)?;
            reject_optional_aggregate_in(comprehension.where_expr.as_ref(), slot)
        }
        Expr::PatternComprehension(comprehension) => {
            // A pattern comprehension is the comprehension family's
            // pattern form: its projection and WHERE evaluate per match,
            // so an aggregate there is the same 42I24 the list and map
            // forms reject (ADR-0685 lowers the form to its collect
            // subquery, whose RETURN would otherwise fold the aggregate
            // into one element instead of rejecting the spelling).
            let slot = "a pattern comprehension";
            reject_aggregate_in(&comprehension.map_expr, slot)?;
            reject_optional_aggregate_in(comprehension.where_expr.as_ref(), slot)
        }
        Expr::Quantifier(quantifier) => {
            reject_optional_aggregate_in(quantifier.predicate.as_ref(), "a quantifier predicate")
        }
        Expr::Reduce(reduce) => reject_aggregate_in(&reduce.expression, "a reduce() expression"),
        Expr::AllReduce(reduce) => {
            let slot = "a reduce() expression";
            reject_aggregate_in(&reduce.expression, slot)?;
            reject_aggregate_in(&reduce.predicate, slot)
        }
        _ => Ok(()),
    }
}

/// The one rejection of an aggregation in a non-aggregating position.
fn reject_aggregation(context: &str) -> EngineError {
    EngineError::semantic(
        ErrorKind::InvalidAggregation,
        format!(
            "invalid use of aggregate function: aggregate expression is not allowed in {context}"
        ),
    )
}

/// Reject an aggregate anywhere in one mandatory non-aggregating slot.
fn reject_aggregate_in(expr: &Expr, context: &str) -> Result<(), EngineError> {
    if contains_aggregation_anywhere(expr) {
        return Err(reject_aggregation(context));
    }
    Ok(())
}

/// [`reject_aggregate_in`] for a slot the grammar makes optional.
fn reject_optional_aggregate_in(expr: Option<&Expr>, context: &str) -> Result<(), EngineError> {
    expr.map_or(Ok(()), |expr| reject_aggregate_in(expr, context))
}

#[cfg(test)]
mod tests {

    /// Drive the production validation entry, so the test exercises the
    /// real battery wiring rather than re-spelling it.
    fn validate(query: &str) -> Result<(), String> {
        crate::validate::parse_and_validate_tree(query)
    }

    #[test]
    fn aggregates_are_rejected_in_the_non_aggregating_contexts() {
        for query in [
            "WITH 1 AS a MATCH (n) RETURN all(x in [1,2,3] WHERE collect(a) = 0)",
            "WITH 1 AS a MATCH (n) RETURN any(x in [1,2,3] WHERE count(a) = 0)",
            "WITH 1 AS a MATCH (n) RETURN none(x in [1,2,3] WHERE min(a) = 0)",
            "WITH 1 AS a MATCH (n) RETURN single(x in [1,2,3] WHERE max(a) = 0)",
            "WITH 1 AS a MATCH (n) RETURN reduce(acc = 0, x in [1,2,3] | acc + collect(a))",
            "WITH 1 AS a UNWIND collect(a) AS x RETURN x",
            "WITH 1 AS a MATCH (n) RETURN [x in [1,2,3] | collect(a) = 0]",
            "WITH 1 AS a MATCH (n) RETURN [x in [1,2,3] WHERE collect(a) = 0]",
            "WITH 1 AS a MATCH (n) RETURN {k: x in [1,2] | k : collect(a)}",
            "WHEN count(*) = 1 THEN RETURN 1 AS x",
            "WHEN count($para) = 1 THEN RETURN 1 AS x",
            // The historical reach stopped at these shapes; the full reach
            // finds them.
            "WITH 1 AS a RETURN all(x IN [1] WHERE collect(a) IN [0])",
            "WITH 1 AS a RETURN [x IN [1] | [collect(a)]]",
            "WITH 1 AS a RETURN [x IN [1] | CASE WHEN true THEN collect(a) END]",
            "WITH 1 AS a RETURN [x IN [1] | {k: collect(a)}]",
            "WITH 1 AS a RETURN reduce(acc = 0, x IN [1] | head([collect(a)]))",
            // Clause positions other than RETURN/WHERE are covered too.
            "MATCH (n) SET n.x = [y IN [1] | collect(n)]",
            "MATCH (n) WITH n CALL { WITH n RETURN [y IN [1] | collect(n)] AS c } RETURN c",
        ] {
            match validate(query) {
                Err(err) => assert!(err.contains("InvalidAggregation"), "{query}: {err}"),
                Ok(()) => panic!("{query}: expected an InvalidAggregation rejection"),
            }
        }
    }

    /// The iteration *source* is aggregation-transparent: Neo4j returns a
    /// value for these (live-verified against neo4j:2026.09-enterprise).
    #[test]
    fn aggregation_in_the_iteration_source_is_allowed() {
        for query in [
            "WITH [1] AS l RETURN all(x IN collect(l) WHERE x > 0)",
            "WITH [1] AS l RETURN any(x IN collect(l) WHERE x > 0)",
            "WITH [1] AS l RETURN [x IN collect(l) | x]",
            "WITH [1] AS l RETURN reduce(acc = 0, x IN collect(l) | acc + x)",
            "RETURN size(collect([1]))",
        ] {
            assert!(validate(query).is_ok(), "{query}: {:?}", validate(query));
        }
    }
}
