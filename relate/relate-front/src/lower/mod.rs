//! Lower Relate `Statement` into the planner-IR `Query` (TCK subset + CIP waves).

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir as mdb;
use relate_parser as cl;

use crate::validate::StatementPolicy;

/// Lower a statement with the default policy (no safe mode, the default
/// traversal-depth admission limit [`relate_core::MAX_TRAVERSAL_DEPTH`]).
pub fn lower_statement(stmt: &cl::Statement) -> Result<mdb::Query> {
    lower_statement_with_options_and_policy(
        stmt,
        crate::validate::ValidateOptions::default(),
        StatementPolicy::default(),
        &BTreeMap::new(),
    )
}

/// [`lower_statement`] with the store's resolved traversal-depth admission
/// limit (LABS-610): an explicit hop bound above `max_traversal_depth` is a
/// typed `TraversalDepthExceeded` syntax error before anything lowers.
pub fn lower_statement_with_limits(
    stmt: &cl::Statement,
    max_traversal_depth: u32,
) -> Result<mdb::Query> {
    lower_statement_with_options_and_policy(
        stmt,
        crate::validate::ValidateOptions::default(),
        StatementPolicy::unsafened(max_traversal_depth),
        &BTreeMap::new(),
    )
}

/// The one implementation behind the entry points: the ADR-0371 validation
/// gates, the LABS-610 traversal-depth admission limit, the LABS-680
/// safe-mode shape rules, and the parameter-aware dynamic-operand rule all
/// ride the same validation battery. `params` are the statement's bound
/// parameter values: Neo4j treats them as compile-time constants, so a
/// parameter-typed dynamic operand of the wrong type fails here even when
/// the driving `MATCH` matches no rows.
pub(crate) fn lower_statement_with_options_and_policy(
    stmt: &cl::Statement,
    options: crate::validate::ValidateOptions,
    policy: StatementPolicy,
    params: &BTreeMap<String, Value>,
) -> Result<mdb::Query> {
    validate_statement(stmt, options, policy, params)?;
    // The CIP-240 static-predicate (inner-WHERE) filter shape check lives
    // in exactly one place: `search_filter`, the lowering boundary's own
    // grammar walk over the inner `WHERE` of every index specification of
    // every (possibly nested, CIP-278) source tree (the CIP-224 SEARCH
    // rules themselves run in the battery above, via
    // `validate_search_clauses`).
    lower_statement_impl(stmt)
}

/// One front-validation rejection as the engine's typed [`Error`]: the
/// class the validator's [`ErrorKind`](relate_error::ErrorKind) carries is
/// preserved for the type/argument classes (so a statically-known type
/// mismatch surfaces as `TypeError`, matching its runtime twin), while every
/// other kind keeps the historical `Syntax` rendering.
fn front_validation_error(error: relate_error::EngineError) -> Error {
    let text = error.to_string();
    // The concatenation operand mismatch is the one front check whose class
    // differs from the historical `SyntaxError` flattening (CIP-144 expects
    // `TypeError`); every other kind keeps its rendered syntax error.
    if error.kind() == relate_error::ErrorKind::InvalidConcatenationOperand {
        Error::ty(text)
    } else {
        Error::syntax(text)
    }
}

/// The validation sequence the boundary runs before lowering.
fn validate_statement(
    stmt: &cl::Statement,
    options: crate::validate::ValidateOptions,
    policy: StatementPolicy,
    params: &BTreeMap<String, Value>,
) -> Result<()> {
    // Validation runs once, up front, as a single walk over the whole
    // statement tree (LABS-70/LABS-81):
    //
    // 1. `validate_statement_tree_with_options_and_policy` heads its
    //    per-statement battery with the ADR-0257 nesting-depth guard
    //    (statement/structural nesting is capped at the lowering boundary;
    //    the parser caps expression nesting), so an over-nested query is a
    //    typed error here, never a stack-overflow abort, and applies every
    //    scope-free validator to the top-level statement *and* every nested
    //    body (UNION arms, NEXT stages, `CALL { }` / `WHEN` / `ELSE` bodies,
    //    FOREACH bodies, and `EXISTS`/`COUNT`/`COLLECT` subquery bodies), so
    //    a rule violation cannot hide inside nesting. The traversal-depth
    //    admission limit (LABS-610) and the safe-mode shape rules (LABS-680)
    //    ride the same battery.
    crate::validate::validate_statement_tree_with_options_and_policy(stmt, options, policy)
        .map_err(front_validation_error)?;
    // 2. The scope-sensitive validators -- they track bindings across the
    //    clauses of one linear statement body, which a nested body cannot do
    //    soundly without its enclosing (correlated) scope -- and the
    //    self-recursing CIP-150 dialect gate and ADR-0497 function-name
    //    admission gate run on the top-level statement; each covers nested
    //    bodies in its own way (FOREACH bodies share the enclosing scope and
    //    are reached by `validate_variable_types`'s own recursion).
    crate::validate::validate_show_column_names(stmt).map_err(front_validation_error)?;
    for validation in [
        crate::call_scope::validate_call_scope_clauses(stmt),
        crate::validate::validate_show_composability(stmt),
        crate::validate::validate_show_system_graph_reads(stmt),
        crate::validate::validate_show_requires_yield_before_return(stmt),
        crate::validate::validate_variable_types(stmt),
        crate::validate::validate_aggregation_semantics(stmt),
        crate::validate::validate_expression_constraints(stmt),
        // ADR-0497 §4: the function-name admission gate.
        crate::validate::validate_function_admission(stmt, options.preview),
        // LABS-709: dynamic key/label operands whose value is statically
        // known from a binding (a `WITH`-projected variable) or from a bound
        // parameter. Neo4j folds both before execution, so a bad one fails
        // on a zero-row `MATCH` too. Runs last, after the historical
        // statement battery, so first-error precedence is unchanged.
        crate::validate::validate_scoped_dynamic_operands(stmt, params),
    ] {
        if let Err(error) = validation {
            return Err(front_validation_error(error));
        }
    }
    Ok(())
}

/// The lowering boundary's validation sequence with the default options and
/// policy (no safe mode, the default traversal-depth limit, no bound
/// parameters), so a test drives exactly what [`lower_statement`] drives
/// instead of a hand-picked slice of the battery.
#[cfg(test)]
pub(crate) fn validate_statement_for_test(stmt: &cl::Statement) -> Result<()> {
    validate_statement(
        stmt,
        crate::validate::ValidateOptions::default(),
        StatementPolicy::default(),
        &BTreeMap::new(),
    )
}

fn lower_statement_impl(stmt: &cl::Statement) -> Result<mdb::Query> {
    // The `EXPLAIN`/`PROFILE` preamble (`stmt.preamble`) is not lowered:
    // it rides out of band on `LoweredStatement`
    // (`crate::parse_and_lower_statement`) and is honored by the connection
    // layer, which renders the plan without executing (EXPLAIN) or with
    // statistics (PROFILE). Lowering always produces the wrapped statement's
    // real query.
    let mut parts = vec![mdb::SingleQuery {
        clauses: lower_clauses(&stmt.clauses)?,
    }];
    let mut union_all = Vec::new();
    for (arm, all) in crate::ast::collect::union_arms(stmt.union.as_deref()) {
        union_all.push(all);
        parts.push(mdb::SingleQuery {
            clauses: lower_clauses(&arm.clauses)?,
        });
    }
    Ok(mdb::Query {
        parts,
        union_all,
        next: lower_boxed(stmt.next.as_deref(), lower_statement_impl)?,
    })
}

/// Lower an optional boxed value, keeping the optional-boxed shape —
/// one spelling of the `.as_ref().map(f).transpose()?.map(Box::new)`
/// chain the lower module's optional-boxed lowering sites share.
fn lower_boxed<'a, T, U>(
    opt: Option<&'a T>,
    lower: impl FnOnce(&'a T) -> Result<U>,
) -> Result<Option<Box<U>>> {
    opt.map(|inner| lower(inner).map(Box::new)).transpose()
}

fn nyi(what: &str) -> Error {
    Error::semantic(format!("not implemented: {what}"))
}

fn lower_clauses(clauses: &[cl::Clause]) -> Result<Vec<mdb::Clause>> {
    let mut out = Vec::with_capacity(clauses.len());
    for c in clauses {
        if let cl::Clause::StandaloneOrderBy(page) = c {
            for stage in &page.stages {
                let (order, skip, limit) = match stage {
                    cl::PageStage::OrderBy(items) => (lower_order(items)?, None, None),
                    cl::PageStage::Skip(expr) => (Vec::new(), Some(lower_expr(expr)?), None),
                    cl::PageStage::Limit(expr) => (Vec::new(), None, Some(lower_expr(expr)?)),
                };
                out.push(mdb::Clause::With {
                    distinct: false,
                    group_by: None,
                    items: vec![mdb::Projection {
                        expr: mdb::Expr::Star,
                        alias: None,
                        source_text: None,
                    }],
                    order,
                    skip,
                    limit,
                    where_clause: None,
                    origin: mdb::WithOrigin::Explicit,
                });
            }
            continue;
        }
        out.push(lower_clause(c)?);
    }
    Ok(out)
}

/// Lower a CIP-236 `<group by clause>`: explicit keys replace the
/// inference from non-aggregating items (that is the *only* semantic
/// difference). A grouping element that is a bare variable matching a
/// projection alias resolves to that item's expression (Rule 4.2); `()`
/// lowers to the empty key set (global aggregation) and `ALL` to the
/// non-aggregating items (which equals inference).
fn lower_group_by(
    spec: &Option<cl::GroupBySpec>,
    items: &[cl::ReturnItem],
) -> Result<Option<Vec<mdb::Expr>>> {
    let Some(spec) = spec else { return Ok(None) };
    Ok(Some(match spec {
        cl::GroupBySpec::Empty => Vec::new(),
        cl::GroupBySpec::All => items
            .iter()
            .filter(|i| !crate::agg::contains_aggregation(&i.expr))
            .map(|i| lower_expr(&i.expr))
            .collect::<Result<Vec<_>>>()?,
        cl::GroupBySpec::Keys(keys) => {
            let mut out = Vec::with_capacity(keys.len());
            for k in keys {
                out.push(match k {
                    // Alias reference: resolve to the aliased item's
                    // expression (Rule 4.2) — through the one shared alias
                    // predicate (`crate::validate::is_aliased`).
                    cl::Expr::Variable(name) => items
                        .iter()
                        .find(|i| crate::validate::is_aliased(i, name))
                        .map(|i| lower_expr(&i.expr))
                        .transpose()?
                        .unwrap_or_else(|| mdb::Expr::Variable(name.clone())),
                    // GBN1.5: a property of a self-shadowing alias
                    // (`a.p AS a` + `GROUP BY a.p`) resolves the base to the
                    // aliased expression, so the key is `(a.p).p`
                    // (GroupByAcceptance [46]: evaluated, not rejected).
                    // One scan: the first item aliased `base`, kept when it
                    // self-shadows.
                    cl::Expr::Property(property) => match &property.expr {
                        cl::Expr::Variable(base) => {
                            let aliased = items
                                .iter()
                                .find(|i| crate::validate::is_aliased(i, base))
                                .filter(|i| {
                                    crate::validate::alias_shadows_its_own_base(base, &i.expr)
                                });
                            match aliased {
                                Some(item) => mdb::Expr::property(
                                    lower_expr(&item.expr)?,
                                    property.property.clone(),
                                ),
                                None => lower_expr(k)?,
                            }
                        }
                        _ => lower_expr(k)?,
                    },
                    other => lower_expr(other)?,
                });
            }
            out
        }
    }))
}

mod clause;
mod expr;
mod pattern;
mod search_filter;

use clause::*;
pub use expr::{lower_expr, lower_type_expr};
use pattern::*;

#[cfg(test)]
mod tests;
