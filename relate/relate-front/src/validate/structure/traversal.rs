//! The traversal-depth admission limit (LABS-610, ADR-0466): the clause-level
//! pattern lists and the expression-embedded patterns, split from
//! `structure/mod.rs` so each file stays under the code-shape size cap.

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, PatternElement, PatternPath};

/// The traversal-depth admission limit (LABS-610, ADR-0466): an explicit
/// variable-length hop bound (`*1..N`, `*..N`, `*N`, and a CIP-28
/// quantifier `{1,N}` on a quantified path pattern) may not exceed
/// `max_traversal_depth`. The executor walks those bounds recursively, and
/// the native stack aborts before 10,000 hops in a debug build (see
/// `relate_core::traversal`). Rejecting the statement at lowering keeps
/// it away from the walker entirely and gives every surface one typed error
/// (Bolt SYNTAX_ERROR, HTTP 400/42N06).
///
/// An unbounded upper bound (`max_hops == None`, `quantifier.max == None`)
/// is exempt: the executor already caps a classic walk at
/// `DEFAULT_MAX_HOPS` (32). The compact and non-compact pattern spellings,
/// and the CIP-28 Form-5 bare-quantified relationship, all arrive here as
/// `PatternElement::Relationship`/`PatternElement::Quantified` entries in
/// `PatternPath::elements`, so one recursive scan covers every form —
/// including quantified groups nested inside quantified groups.
///
/// Wired through [`crate::validate::validate_statement_tree`]'s per-statement
/// battery, so a bound hidden in a `CALL { }` body, a `WHEN`/`ELSE` body, a
/// FOREACH body, a `COUNT`/`COLLECT`/`EXISTS` subquery, or a UNION/NEXT arm
/// is rejected too.
pub fn validate_traversal_bounds(
    clauses: &[Clause],
    max_traversal_depth: u32,
) -> Result<(), EngineError> {
    for clause in clauses {
        let Some(patterns) = crate::ast::collect::clause_patterns(clause) else {
            continue;
        };
        for path in patterns {
            check_path_traversal_bounds(path, max_traversal_depth)?;
        }
    }
    Ok(())
}

/// The expression-embedded half of [`validate_traversal_bounds`]: a pattern
/// predicate (`WHERE (a)-[*1..N]->(b)`), a pattern comprehension, and the
/// `shortestPath()`/`allShortestPaths()` expression each carry a
/// [`PatternPath`] outside any clause pattern list. The whole-tree walk's
/// expression visitor calls this for every expression, at every nesting
/// depth.
pub fn validate_expr_traversal_bounds(
    expr: &Expr,
    max_traversal_depth: u32,
) -> Result<(), EngineError> {
    let Some(path) = crate::ast::collect::pattern_of_expr(expr) else {
        return Ok(());
    };
    check_path_traversal_bounds(path, max_traversal_depth)
}

fn check_path_traversal_bounds(
    path: &PatternPath,
    max_traversal_depth: u32,
) -> Result<(), EngineError> {
    for element in &path.elements {
        check_element_traversal_bounds(element, max_traversal_depth)?;
    }
    Ok(())
}

fn check_element_traversal_bounds(
    element: &PatternElement,
    max_traversal_depth: u32,
) -> Result<(), EngineError> {
    match element {
        PatternElement::Node(_) => {}
        PatternElement::Relationship(rp) => {
            if let Some(max_hops) = rp.max_hops {
                check_hop_bound(max_hops, max_traversal_depth)?;
            }
        }
        PatternElement::Quantified(qpp) => {
            if let Some(max) = qpp.quantifier.max {
                check_hop_bound(max, max_traversal_depth)?;
            }
            // A quantified group's own elements can nest further groups.
            for inner in &qpp.elements {
                check_element_traversal_bounds(inner, max_traversal_depth)?;
            }
        }
    }
    Ok(())
}

fn check_hop_bound(bound: u32, max_traversal_depth: u32) -> Result<(), EngineError> {
    if bound > max_traversal_depth {
        return Err(EngineError::semantic(
            ErrorKind::TraversalDepthExceeded,
            relate_core::traversal_depth_exceeded_message(
                bound,
                relate_core::MAX_TRAVERSAL_DEPTH_CEILING,
            ),
        ));
    }
    Ok(())
}
