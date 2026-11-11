//! Expression checking: variable resolution against the binding
//! environment, predicate shape rules, and the per-variant semantic rules
//! (function arity/argument types, comprehension scopes, subquery
//! expressions, ...). The per-name builtin-call rule table lives in its own
//! [`function_meta`] module (`shadowing` sets the split precedent).

use relate_core::{Error, Result};
// The aggregate-name set is owned by `relate-ir` and re-exported by
// `relate-front::agg` (which adds the CIP-128 alias spellings); the
// known-function registry is owned by `relate-front`. The planner binds
// against these single definitions instead of keeping local copies.
use relate_front::agg::is_aggregate_name as is_agg_fn;
use relate_ir::{BinaryOp, Clause, Expr, MapProjectionItem, UnaryOp};
use relate_names::SmallName;

use crate::binding::{pattern_local_bindings, Binding, Env, EnvBridge};
use crate::expr_text::graph_call_text;
use crate::names::query_output_names_with_seed;
use crate::names::query_with_persistent_imports;
use crate::pattern::{check_path_exprs, check_pattern_pred_vars};
use crate::validate::{clause_is_write, validate_part_with_env};

mod function_meta;
pub(crate) mod literal;
pub(crate) mod shadowing;

use function_meta::apply_function_meta;
pub(crate) use literal::float_integer_site;
use literal::{is_non_integer_literal, is_static_non_bool};
use shadowing::{reject_binding_shadowing, reject_projection_shadowing};

pub(crate) fn check_expr(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    expr: &Expr,
) -> Result<()> {
    check_expr_ctx(env, is_write_procedure, expr, false)
}

pub(crate) fn check_pred(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    expr: &Expr,
) -> Result<()> {
    // Both of the checks below decide a *static* fact about the expression,
    // and live Neo4j reports a statically non-boolean predicate as a
    // compile-time `SyntaxError` (probed 2026-11-10: `WHERE 1` and
    // `MATCH (n) WHERE n` are both `Neo.ClientError.Statement.SyntaxError`).
    // A value whose type is only known per row keeps the runtime TypeError
    // (`bool_or_null`).
    if is_static_non_bool(expr) {
        return Err(Error::static_requires_boolean("predicate"));
    }
    if let Expr::Variable(v) = expr {
        if matches!(
            env.get_name(v),
            Some(Binding::Node | Binding::Rel | Binding::Path)
        ) {
            return Err(Error::invalid_argument_type(
                "an entity or path is not a boolean predicate",
            )
            .into_syntax());
        }
    }
    check_expr_ctx(env, is_write_procedure, expr, true)
}

pub(crate) fn is_delete_target(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Variable(_)
            | Expr::Property { .. }
            | Expr::Subscript { .. }
            | Expr::Function { .. }
            | Expr::Case { .. }
            // `DELETE null` is a legal no-op (Neo4j acceptance
            // `Finish.feature`); the executor ignores `Value::Null`.
            | Expr::Null
    )
}

/// The one property-access-on-non-map rejection: a variable bound to a
/// path, scalar, or list has no properties to read. Shared by the
/// `Expr::Property` arm and the map projection's `.key` shorthand (which
/// reads a property of the projection target), so "what is not map-like"
/// is decided once.
fn reject_property_on_non_map(env: &Env, v: &SmallName) -> Result<()> {
    if matches!(
        env.get_name(v),
        Some(&Binding::Path | &Binding::Scalar | &Binding::List)
    ) {
        return Err(Error::syntax(
            "InvalidArgumentType: property access on a non-map value",
        ));
    }
    Ok(())
}

pub(crate) fn check_nonneg_int_arg(expr: &Expr) -> Result<()> {
    // One authored detail (`relate_core`), one token composition; this layer
    // decides only the class.
    let negative = matches!(expr, Expr::Int(i) if *i < 0)
        || matches!(
            expr,
            Expr::Unary {
                op: UnaryOp::Minus,
                expr,
            } if matches!(expr.as_ref(), Expr::Int(i) if *i > 0)
        );
    if negative {
        return Err(Error::syntax(relate_core::negative_page_bound_text()));
    }
    Ok(())
}

pub(crate) fn check_positive_int_arg(expr: &Expr, what: &str) -> Result<()> {
    match expr {
        Expr::Int(i) if *i <= 0 => Err(Error::syntax(format!(
            "InvalidArgument: {what} must be a positive integer"
        ))),
        Expr::Unary {
            op: UnaryOp::Minus,
            expr,
        } if matches!(expr.as_ref(), Expr::Int(_) | Expr::Float(_)) => Err(Error::syntax(format!(
            "InvalidArgument: {what} must be a positive integer"
        ))),
        _ if is_non_integer_literal(expr) => Err(Error::syntax(format!(
            "InvalidArgumentType: {what} must be an integer"
        ))),
        _ => Ok(()),
    }
}

/// Whether the expression is non-static for CIP-34 concurrency / CIP-198
/// retry-duration purposes: it references a variable, a map projection (its
/// base is a row value), a pattern, or a subquery — at any nesting depth.
/// This is a single fold over `Expr::children`, so no variant (e.g. a list
/// comprehension under a subscript) can smuggle a variable past the check.
pub(crate) fn expr_non_static(expr: &Expr) -> bool {
    expr.any(&mut |e| {
        matches!(
            e,
            Expr::Variable(_)
                | Expr::MapProjection { .. }
                | Expr::PatternPred { .. }
                | Expr::PatternComp { .. }
                | Expr::ShortestPath { .. }
                | Expr::ExistsSubquery { .. }
                | Expr::CountSubquery { .. }
                | Expr::CollectSubquery { .. }
        )
    })
}

fn expr_contains_rand(expr: &Expr) -> bool {
    expr.any(&mut |e| {
        matches!(e, Expr::Function { name, .. }
            if name.with_str(|n| n.eq_ignore_ascii_case("rand")))
    })
}

/// The known-function check the binder applies to lowered IR. Delegates to
/// the single registry owned by `relate-front` (`KNOWN_FUNCTIONS` plus
/// the unadvertised lowered-internal names such as `string.interp`), so a
/// function advertised by `SHOW FUNCTIONS` can no longer be rejected here
/// (and vice versa).
fn is_known_function(name: &str) -> bool {
    relate_front::is_known_function(name)
}

/// The policy every subquery-expression body (`EXISTS { … }`, `COUNT { … }`,
/// `COLLECT { … }`) must satisfy: resolve the enclosing scope as its imports,
/// reject a NEXT-stage projection that shadows one, forbid `FINISH` and
/// writes, validate the body under the enclosing bindings, and return the
/// body's validated output names. The one kind-specific rule — COLLECT's
/// single-column requirement — lives in its own `check_expr_ctx` arm, at the
/// site that owns that difference.
fn check_subquery_expression(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    query: &relate_ir::Query,
) -> Result<Vec<String>> {
    let imported: Vec<String> = env.keys().cloned().collect();
    reject_projection_shadowing(query, &imported)?;
    // The shadowing test reads the ORIGINAL body: `scoped_query` below
    // injects a bare `Variable(<outer>)` item into every `WITH`, so a
    // `WITH`-bearing body with two or more outer names would always count
    // as correlated there and the `UNWIND`/`LOAD CSV` shadowing rejection
    // would silently stop firing. The rewrite adds no `UNWIND`/`LOAD CSV`
    // clauses, so the loop sees the same clauses either way.
    reject_binding_shadowing(query, env)?;
    let scoped_query = query_with_persistent_imports(query, &imported);
    if query
        .parts
        .iter()
        .any(|part| matches!(part.clauses.last(), Some(Clause::Finish)))
    {
        return Err(Error::syntax(
            "InvalidClauseComposition: FINISH is not allowed in a subquery expression",
        ));
    }
    // Non-entity imports are shadowable by this body's own pattern
    // variables: `EXISTS { (n) … }` binds a fresh node `n` even when the
    // enclosing scope bound `n` to a scalar (the source's "subquery may open
    // with a WITH of an EXISTS containing a pattern and a WHERE"). An
    // entity import keeps its binding — reusing an outer node as a
    // relationship is a genuine type conflict. (The rejection itself ran
    // over the original body above, before the import rewrite.)
    // `Scope::expression_body` derives the aggregation imports (every outer
    // name — per-row constants the body may reference without grouping by
    // them, ADR-0453) and the shadowable non-entity subset from `env`.
    for part in &scoped_query.parts {
        if part
            .clauses
            .iter()
            .any(|clause| clause_is_write(clause, is_write_procedure))
        {
            return Err(Error::syntax(
                "InvalidClauseComposition: writes are not allowed in subquery expressions",
            ));
        }
        validate_part_with_env(
            part,
            crate::validate::Scope::expression_body(env.clone()),
            is_write_procedure,
        )?;
    }
    query_output_names_with_seed(&scoped_query, imported, &std::collections::HashSet::new())
}

/// ADR-0175: reject a float literal at the integer site `expr` carries,
/// naming that site's authored detail ([`float_integer_site`] owns the
/// list).
fn reject_float_integer_site(expr: &Expr) -> Result<()> {
    match float_integer_site(expr) {
        Some(site) => Err(Error::invalid_argument_type(site.type_error)),
        None => Ok(()),
    }
}

fn check_expr_ctx(
    env: &Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    expr: &Expr,
    allow_pattern_pred: bool,
) -> Result<()> {
    match expr {
        Expr::Variable(v) => {
            if !env.contains_name(v) {
                return Err(v.with_str(Error::undefined_variable));
            }
        }
        Expr::Star => {}
        Expr::Function { name, args, .. } => {
            let n = name.with_str(str::to_ascii_lowercase);
            // 42I13 offends before 42N72: a `graph.*` selector with the wrong
            // argument count is an arity failure, not the composite-database
            // refusal. The call text (`graph_call_text`) renders only for a
            // selector — the two CE errors below are its only readers, and
            // every ordinary function call used to pay the full
            // name-and-args render per check.
            if relate_core::is_graph_selector_function_name(&n) {
                let call_text = graph_call_text(name, args);
                relate_core::ce_messages::check_graph_selector_arity(&call_text, &n, args.len())?;
                // ADR-0116 §3: every composite-database `graph.*` selector
                // (`relate_core::GRAPH_SELECTOR_FUNCTIONS`,
                // `graph.byElementId` included) fails with CE's 42N72 text
                // in every expression position (the `USE` clause itself is
                // resolved by `use_target`). They stay listed by
                // `SHOW FUNCTIONS` through the executor's catalog-only
                // entries.
                return Err(relate_core::ce_messages::composite_only_graph_function(
                    &call_text,
                ));
            }
            if !is_known_function(&n) {
                return Err(name.with_str(Error::unknown_function));
            }
            // The one per-name rule table: arity (42I13), the argument
            // shape rule, the ADR-0175 integer-argument sites, the
            // binding-kind / scope rules, and the temporal constructor
            // family's static construction check — one dispatch point.
            apply_function_meta(env, &n, args)?;
            let child_pred = n == "exists";
            if is_agg_fn(&n) && args.iter().any(expr_contains_rand) {
                return Err(Error::syntax(
                    "NonConstantExpression: aggregations cannot contain rand()",
                ));
            }
            for (index, a) in args.iter().enumerate() {
                if n == "property_exists" && index == 1 {
                    continue;
                }
                check_expr_ctx(env, is_write_procedure, a, child_pred)?;
            }
        }
        Expr::Property { expr: inner, .. } => {
            if let Expr::Variable(v) = inner.as_ref() {
                reject_property_on_non_map(env, v)?;
            }
            expr.try_each_child(&mut |child| check_expr(env, &mut *is_write_procedure, child))?;
        }
        Expr::Unary { op, .. } => {
            // A boolean-operand position reads a predicate, so a pattern
            // expression is legal there even in a projection (`RETURN NOT
            // ()--()`), exactly as the front end's projection rule allows.
            let child = matches!(op, UnaryOp::Not);
            expr.try_each_child(&mut |c| check_expr_ctx(env, &mut *is_write_procedure, c, child))?;
        }
        Expr::Binary { op, left, right } => {
            // A binary node carrying its operator's absorbing boolean
            // constant (`x OR true`, `x AND false`) folds to that constant
            // before variable resolution — the same rewrite
            // `BinaryOp::absorbing_constant` documents — so every operand
            // is discarded and there is nothing here to check
            // (`MiscAcceptance` "IN predicate with undefined variable is
            // accepted when it can be rewritten away").
            if op.absorbing_constant(left, right).is_some() {
                return Ok(());
            }
            let child = op.is_boolean();
            for c in [left.as_ref(), right.as_ref()] {
                check_expr_ctx(env, is_write_procedure, c, child)?;
            }
            // The boolean operators require boolean operands; a statically
            // non-boolean operand is the compile-time SyntaxError class (live
            // Neo4j: `RETURN 1 AND true` is a SyntaxException). A value of
            // runtime-decided type keeps the runtime TypeError.
            if child
                && [left.as_ref(), right.as_ref()]
                    .into_iter()
                    .any(is_static_non_bool)
            {
                return Err(Error::static_requires_boolean(op.as_str()));
            }
            if *op == BinaryOp::In {
                if let Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) =
                    right.as_ref()
                {
                    return Err(Error::syntax(format!(
                        "InvalidArgumentType: {} requires a list",
                        BinaryOp::In.as_str()
                    )));
                }
            }
        }
        Expr::Case {
            when_then,
            else_expr,
        } => {
            for (when, then) in when_then {
                check_pred(env, is_write_procedure, when)?;
                check_expr_ctx(env, is_write_procedure, then, allow_pattern_pred)?;
            }
            if let Some(e) = else_expr {
                check_expr_ctx(env, is_write_procedure, e, allow_pattern_pred)?;
            }
        }
        Expr::ListComp {
            ident,
            list,
            predicate,
            projection,
        } => {
            check_expr(env, is_write_procedure, list)?;
            let mut inner = env.clone();
            inner.insert_name(ident.clone(), Binding::Value);
            if let Some(p) = predicate {
                check_pred(&inner, is_write_procedure, p)?;
            }
            check_expr(&inner, is_write_procedure, projection)?;
        }
        // CIP-272: the key/value (map source) or item (list source)
        // bindings scope the predicate and both projection expressions.
        Expr::MapComp {
            key_ident,
            value_ident,
            source,
            predicate,
            key_expr,
            value_expr,
        } => {
            check_expr(env, is_write_procedure, source)?;
            let mut inner = env.clone();
            if let Some(k) = key_ident {
                inner.insert_name(k.clone(), Binding::Value);
            }
            inner.insert_name(value_ident.clone(), Binding::Value);
            if let Some(p) = predicate {
                check_pred(&inner, is_write_procedure, p)?;
            }
            check_expr(&inner, is_write_procedure, key_expr)?;
            check_expr(&inner, is_write_procedure, value_expr)?;
        }
        Expr::Quantifier {
            ident,
            list,
            predicate,
            ..
        } => {
            check_expr(env, is_write_procedure, list)?;
            let mut inner = env.clone();
            inner.insert_name(ident.clone(), Binding::Value);
            check_pred(&inner, is_write_procedure, predicate)?;
        }
        Expr::Reduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
        } => {
            check_expr(env, is_write_procedure, initial)?;
            check_expr(env, is_write_procedure, list)?;
            let mut inner = env.clone();
            inner.insert_name(accumulator.clone(), Binding::Value);
            inner.insert_name(ident.clone(), Binding::Value);
            check_expr(&inner, is_write_procedure, expression)?;
        }
        Expr::AllReduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
            predicate,
        } => {
            check_expr(env, is_write_procedure, initial)?;
            check_expr(env, is_write_procedure, list)?;
            let mut inner = env.clone();
            inner.insert_name(accumulator.clone(), Binding::Value);
            inner.insert_name(ident.clone(), Binding::Value);
            check_expr(&inner, is_write_procedure, expression)?;
            check_pred(&inner, is_write_procedure, predicate)?;
        }
        Expr::PatternPred { path } => {
            if !allow_pattern_pred {
                return Err(Error::syntax(
                    "UnexpectedSyntax: pattern predicates are only valid in WHERE",
                ));
            }
            check_pattern_pred_vars(env, is_write_procedure, path)?;
            if path.rels.is_empty() {
                return Err(Error::syntax(
                    "InvalidArgumentType: a node pattern is not a valid predicate",
                ));
            }
            // The same expression checks a pattern comprehension's path gets
            // (the arm above): a pattern expression's element predicates are
            // Boolean-typed like every other WHERE-bearing pattern position
            // (CIP-190 `(where {…})` in a clause WHERE is this arm).
            check_path_exprs(env, is_write_procedure, path)?;
        }
        Expr::ShortestPath { path, .. } => {
            check_pattern_pred_vars(env, is_write_procedure, path)?;
            // Same rule as the pattern-expression arm above: a
            // `shortestPath()` pattern's element predicates are
            // Boolean-typed like every other WHERE-bearing pattern position.
            check_path_exprs(env, is_write_procedure, path)?;
        }
        Expr::PatternComp {
            path,
            predicate,
            projection,
        } => {
            // A pattern comprehension introduces its own pattern variables;
            // an inline element predicate may reference any of them, whether
            // declared earlier or later in the same pattern (CIP-17/CIP-18
            // 5.2.1), so check the pattern's expressions against the
            // comprehension's own scope. The scope reads the one pattern
            // binding classification (`pattern_local_bindings`, the table
            // `bind_path` also reads), so a var-length relationship
            // variable or a quantified-path variable binds the same way
            // here as in a MATCH.
            let mut inner = env.clone();
            for (name, binding) in pattern_local_bindings(path) {
                inner.insert_name(name.clone(), binding);
            }
            check_path_exprs(&inner, is_write_procedure, path)?;
            if let Some(p) = predicate {
                check_pred(&inner, is_write_procedure, p)?;
            }
            check_expr(&inner, is_write_procedure, projection)?;
        }
        Expr::MapProjection {
            expr: target,
            items,
        } => {
            check_expr(env, is_write_procedure, target)?;
            for item in items {
                match item {
                    MapProjectionItem::KeyValue { value, .. } => {
                        check_expr(env, is_write_procedure, value)?
                    }
                    MapProjectionItem::Property { .. } => {
                        // The `.key` shorthand reads a property of the
                        // projection target: same check as `Expr::Property`.
                        if let Expr::Variable(v) = target.as_ref() {
                            reject_property_on_non_map(env, v)?;
                        }
                    }
                    MapProjectionItem::Variable { name } => {
                        if !env.contains_name(name) {
                            return Err(name.with_str(Error::undefined_variable));
                        }
                    }
                    MapProjectionItem::AllProperties => {}
                }
            }
        }
        Expr::ExistsSubquery { query } | Expr::CountSubquery { query } => {
            check_subquery_expression(env, is_write_procedure, query)?;
        }
        Expr::CollectSubquery { query } => {
            let names = check_subquery_expression(env, is_write_procedure, query)?;
            if names.len() != 1 {
                return Err(Error::syntax(
                    "CollectExpressionMustReturnSingleColumn: COLLECT { } requires one column",
                ));
            }
        }
        // ADR-0175: a float-literal list index or slice bound fails at plan
        // time with the same `InvalidArgumentType` kind the site raises at
        // runtime (Neo4j's 22N27/22NB1 static gate fires at the same phase);
        // non-literal floats are handled at runtime.
        Expr::Subscript { expr: inner, index } => {
            reject_float_integer_site(expr)?;
            check_expr(env, is_write_procedure, inner)?;
            check_expr(env, is_write_procedure, index)?;
        }
        Expr::Slice { .. } => {
            reject_float_integer_site(expr)?;
            expr.try_each_child(&mut |child| check_expr(env, &mut *is_write_procedure, child))?;
        }
        // Every other variant (literals, parameters, label predicates,
        // type checks, lists, maps, distance-metric literals, ...) has no
        // bespoke validation: just check each child.
        other => {
            other.try_each_child(&mut |child| check_expr(env, &mut *is_write_procedure, child))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_names::SmallName;

    /// Every boolean-requiring planner site decides a *static* fact and
    /// raises the SyntaxError class — live Neo4j reports `WHERE 1`,
    /// `MATCH (n) WHERE n` and `RETURN 1 AND true` all as
    /// `Neo.ClientError.Statement.SyntaxError` (probed 2026-11-10). A value
    /// whose type is only known per row keeps the runtime TypeError.
    #[test]
    fn static_boolean_requiring_sites_raise_the_syntax_class() {
        let mut env = Env::new();
        env.insert("n".to_string(), Binding::Node);

        // `WHERE 1` — a statically non-boolean predicate.
        let err = check_pred(&env, &mut |_| false, &Expr::Int(1)).unwrap_err();
        assert!(matches!(err, Error::Syntax(_)), "predicate: {err:?}");
        assert!(
            err.to_string().contains("predicate requires a boolean"),
            "{err}"
        );

        // `WHERE n` — an entity variable in a predicate position.
        let err = check_pred(
            &env,
            &mut |_| false,
            &Expr::Variable(SmallName::from_name("n")),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Syntax(_)), "entity: {err:?}");
        assert!(
            err.to_string()
                .contains("an entity or path is not a boolean predicate"),
            "{err}"
        );

        // `RETURN 1 AND true` — a statically non-boolean operator operand.
        let and = Expr::Binary {
            op: BinaryOp::And,
            left: Box::new(Expr::Int(1)),
            right: Box::new(Expr::Bool(true)),
        };
        let err = check_expr(&env, &mut |_| false, &and).unwrap_err();
        assert!(matches!(err, Error::Syntax(_)), "{err:?}");
        assert!(err.to_string().contains("AND requires a boolean"), "{err}");
    }

    /// ADR-0511 (LABS-400): a binary node the constant fold collapses
    /// (`1 OR true`) is exempt from the static boolean-operand rule — the
    /// whole node is discarded, operands included, so there is nothing to
    /// check.
    #[test]
    fn a_folded_away_boolean_operand_is_exempt_from_the_static_rule() {
        let env = Env::new();
        let or = Expr::Binary {
            op: BinaryOp::Or,
            left: Box::new(Expr::Int(1)),
            right: Box::new(Expr::Bool(true)),
        };
        check_expr(&env, &mut |_| false, &or)
            .unwrap_or_else(|e| panic!("folded-away operand rejected: {e}"));
    }
}
