//! Aggregation semantics: nested aggregation, non-constant arguments,
//! single-input aggregate arity (LABS-276), grouping-key ambiguity in
//! mixed expressions, ORDER BY aggregation rules, and aggregation in the
//! expression positions that cannot aggregate.
//!
//! The family is split by concern:
//!
//! - [`order_by`]: ORDER BY aggregation rules
//!   ([`validate_order_by_aggregation`]).
//! - [`non_aggregating`]: aggregation rejected in the non-aggregating
//!   positions — comprehension projections/filters, quantifier predicates,
//!   `reduce()` steps, `UNWIND` sources and `WHEN` predicates
//!   ([`validate_expression_slots`] /
//!   [`validate_clause_slots`]).
//! - [`ambiguity`]: RETURN/WITH grouping-key ambiguity in mixed expressions
//!   (`is_ambiguous_aggregation` and the shared grouping-key variable set).
//!
//! This root holds the scope-tracking driver
//! ([`validate_aggregation_semantics`]), the grouping-key extraction
//! ([`grouping_key_exprs`]), the root-level per-call checks (nested
//! aggregation, non-constant arguments, and single-input arity — all
//! riding the shared [`check_agg_calls`] scaffold), and the shared
//! expression helpers the concerns build on.

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_ir::AggregateArity;
use relate_parser::{
    Clause, Expr, FunctionCallExpr, GroupBySpec, OrderByItem, ReturnItem, Statement,
};

use crate::agg::contains_aggregation;
use crate::ast::collect::{clause_introduced_names, collect_expr_variables, projected_names};
use crate::ast::tree::walk_expr_with;

use crate::agg::SINGLE_INPUT_AGGREGATES;

mod ambiguity;
mod non_aggregating;
mod order_by;

use self::ambiguity::{is_ambiguous_aggregation, GroupingKeys};

pub use self::non_aggregating::{validate_clause_slots, validate_expression_slots};
pub use self::order_by::validate_order_by_aggregation;

/// Check if a function name is an aggregation function.
fn is_aggregation_fn(name: &str) -> bool {
    crate::agg::is_aggregate_name(name)
}

/// Check if an expression is a direct aggregation call (not nested inside another expression).
fn is_direct_aggregation(expr: &Expr) -> bool {
    match expr {
        Expr::CountStar => true,
        Expr::FunctionCall(fc) => fc.name.with_str(is_aggregation_fn),
        _ => false,
    }
}

/// The child-descent vocabulary the aggregation walks in this module agree
/// on (F-4-1): Binary/Comparison operands, Unary/Paren inner expressions,
/// Property *values*, and StringInterp embeds — expressed as a
/// [`walk_expr_with`] edge filter mirroring `contains_aggregation`, not a
/// second hand-rolled recursion. Function-call arguments are *not* part of
/// the vocabulary: the checks that ride this filter treat a non-aggregating
/// call's arguments as opaque (the historical verdict — e.g. nested
/// aggregation hidden inside `sin(...)` is not re-checked there) and
/// consume an aggregating call's arguments through their own per-argument
/// predicate instead of walking them.
fn agg_enter_child(parent: &Expr, child: &Expr) -> bool {
    match parent {
        Expr::Binary(_) | Expr::Comparison(_) | Expr::Unary(_) | Expr::Paren(_) => true,
        // A dynamic property key (`n[<expr>]`) is not descended into; the
        // static value side always is.
        Expr::Property(p) => std::ptr::eq(child, &p.expr),
        // The walker only offers the Interp embeds as children.
        Expr::StringInterp(_) => true,
        _ => false,
    }
}

/// The one per-aggregation-call guard scaffold (F-4-5/F-4-6/F-4-8): at an
/// aggregating function call (a node of the shared walker's visit, the
/// walker owning the recursion), `pred` judges the call itself — its
/// error propagates. The walker's edge filter ([`agg_enter_child`]) never
/// descends into function-call arguments: an aggregating call's arguments
/// are consumed by `pred`, and a non-aggregating call's arguments were
/// opaque to the historical hand-rolled recursion. Every root-level
/// aggregation check rides this single walk.
fn check_agg_calls(
    expr: &Expr,
    mut pred: impl FnMut(&FunctionCallExpr) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let mut visit = |node: &Expr| {
        if let Expr::FunctionCall(fc) = node {
            if fc.name.with_str(is_aggregation_fn) {
                pred(fc)?;
            }
        }
        Ok(())
    };
    walk_expr_with(expr, &mut visit, agg_enter_child)
}

/// The per-argument adapter of [`check_agg_calls`]: every argument of an
/// aggregating call must fail `pred` — one that satisfies it raises the
/// `kind`/`msg` semantic error.
fn check_agg_args(
    expr: &Expr,
    pred: impl Fn(&Expr) -> bool,
    kind: ErrorKind,
    msg: &str,
) -> Result<(), EngineError> {
    check_agg_calls(expr, |fc| {
        if fc.args.iter().any(&pred) {
            Err(EngineError::semantic(kind, msg.to_string()))
        } else {
            Ok(())
        }
    })
}

/// Check for nested aggregation: aggregation calls inside arguments of aggregation functions.
/// e.g., count(count(*)) should raise NestedAggregation.
fn check_nested_aggregation(expr: &Expr) -> Result<(), EngineError> {
    check_agg_args(
        expr,
        contains_aggregation,
        ErrorKind::NestedAggregation,
        "aggregation functions cannot be nested",
    )
}

/// Check for non-constant expressions inside aggregation function args.
/// e.g., count(rand()) should raise NonConstantExpression.
fn check_non_constant_in_aggregation(expr: &Expr) -> Result<(), EngineError> {
    check_agg_args(
        expr,
        contains_non_deterministic,
        ErrorKind::NonConstantExpression,
        "non-constant expression used inside aggregation",
    )
}

/// The one front-channel construction of the single-input arity
/// rejection (ADR-0217/ADR-0225): the same kind and clause
/// [`AggregateArity::rejection`] spells for the executor's `agg_eval`
/// backstop, with the kind in [`EngineError`]'s typed slot instead of
/// inline — `Display` prefixes it, so both channels render the same
/// text. The clause is restated here (not derived from `rejection`)
/// only because the contract exposes it kind-inlined for the
/// core-`Error` channel; the
/// `front_arity_rejection_matches_the_ir_contract_text` test fails if
/// either spelling drifts.
fn arity_rejection(name: &str, got: usize) -> EngineError {
    EngineError::semantic(
        ErrorKind::InvalidNumberOfArguments,
        format!("{name}(input) requires exactly one argument, got {got}"),
    )
}

/// Check the arity of single-input aggregation calls (LABS-276): any
/// aggregate in [`SINGLE_INPUT_AGGREGATES`] with an arity other than the
/// class contract's is a semantic `InvalidNumberOfArguments` error (Neo4j
/// 42I13 shape: it names the function and its signature) instead of
/// silently aggregating the constant 1 per row. Rides the shared
/// [`check_agg_calls`] scaffold — the same walk and reach as the other
/// aggregation checks. Membership, required count, and rejection text
/// all come from the one `relate-ir` contract (ADR-0217) the
/// executor's backstop enforces: `count` stays out (the parser's keyword
/// path already guarantees its arity) and the percentiles keep their own
/// two-argument call-site check — exactly the split
/// [`SINGLE_INPUT_AGGREGATES`] derives from the contract.
fn check_aggregate_arity(expr: &Expr) -> Result<(), EngineError> {
    check_agg_calls(expr, |fc| {
        let canonical = fc
            .name
            .with_str(|n| crate::agg::canonical_agg_name(&n.to_ascii_lowercase()).to_string());
        if SINGLE_INPUT_AGGREGATES.contains(&canonical.as_str())
            && fc.args.len() != AggregateArity::SingleInput.required()
        {
            return Err(fc.name.with_str(|n| arity_rejection(n, fc.args.len())));
        }
        Ok(())
    })
}

/// The RETURN/WITH arity driver (LABS-276): the item expressions, the
/// clause's grouping-key expressions (explicit GROUP BY keys when
/// present, the inferred non-aggregating items otherwise — both already
/// computed by the caller for the item checks), and the ORDER BY
/// expressions of one projection clause all pass through
/// [`check_aggregate_arity`].
fn check_clause_aggregate_arity(
    items: &[ReturnItem],
    key_exprs: &[&Expr],
    order_by: &[OrderByItem],
) -> Result<(), EngineError> {
    for item in items {
        check_aggregate_arity(&item.expr)?;
    }
    for key in key_exprs {
        check_aggregate_arity(key)?;
    }
    for ob in order_by {
        check_aggregate_arity(&ob.expr)?;
    }
    Ok(())
}

/// Check if an expression contains a non-deterministic function call like
/// rand(). Rides the shared walker with an edge filter that reproduces the
/// historical reach exactly: any function call's arguments, plus the
/// module's [`agg_enter_child`] vocabulary — everything else (subscripts,
/// list literals, comprehensions, …) hides its contents from this check,
/// as it always has. `pub(super)`: the CALL subquery's `DISJOINT BY` keys
/// reuse it (CIP-260 determinism rule).
pub(super) fn contains_non_deterministic(expr: &Expr) -> bool {
    let mut found = false;
    let mut visit = |node: &Expr| {
        found |= matches!(node, Expr::FunctionCall(fc)
            if matches!(fc.name.with_str(str::to_lowercase).as_str(), "rand" | "randomuuid"));
        Ok(())
    };
    // Delta over [`agg_enter_child`]: this check also descends into any
    // function call's arguments; everything else is the shared vocabulary.
    let enter_child = |parent: &Expr, child: &Expr| match parent {
        Expr::FunctionCall(_) => true,
        _ => agg_enter_child(parent, child),
    };
    // Infallible here: the visitor never fails, and every caller runs after
    // the whole-tree depth cap, so the walker's own cap cannot fire on
    // these trees (the hand-rolled recursion would have overflowed instead).
    let result = walk_expr_with(expr, &mut visit, enter_child);
    debug_assert!(
        result.is_ok(),
        "contains_non_deterministic walked a tree that already passed the depth cap"
    );
    found
}

/// Collect variables from the non-aggregation parts of an expression.
/// This collects variables that appear outside of aggregation function calls.
/// Rides the shared walker: an aggregating call's whole subtree is skipped
/// (its arguments are aggregation inputs), a non-aggregating call's
/// arguments are walked, and a subscript contributes its postfix base and
/// static index but not its slice bounds.
fn collect_non_agg_variables(expr: &Expr, vars: &mut IdentHashSet<SmallName>) {
    let mut visit = |node: &Expr| {
        if let Expr::Variable(v) = node {
            vars.insert(v.clone());
        }
        Ok(())
    };
    // Deltas over [`agg_enter_child`] (the fn doc above gives the
    // semantics of each).
    let enter_child = |parent: &Expr, child: &Expr| match parent {
        Expr::FunctionCall(fc) => !fc.name.with_str(is_aggregation_fn),
        Expr::Subscript(s) => {
            std::ptr::eq(child, &s.expr) || s.index.as_ref().is_some_and(|i| std::ptr::eq(child, i))
        }
        _ => agg_enter_child(parent, child),
    };
    // Infallible for the same reason as `contains_non_deterministic`.
    let result = walk_expr_with(expr, &mut visit, enter_child);
    debug_assert!(
        result.is_ok(),
        "collect_non_agg_variables walked a tree that already passed the depth cap"
    );
}

/// The mixed-star projection policy, the one spelling both projection
/// consumers in this module use (the RETURN DISTINCT orderable set and
/// the WITH scope reset): the canonical [`projected_names`] sliced from
/// the first `*` item — the star carries the incoming scope through,
/// post-star items add their names on top, pre-star items are replaced
/// away; no star, the slice is the whole list (the engine's established
/// mixed-star dialect; `WITH a, *` parses and runs).
fn star_sliced_names(
    items: &[ReturnItem],
    bound: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    let star = items.iter().position(|i| i.all).unwrap_or(0);
    projected_names(&items[star..], bound)
}

/// The one "is this expression verbatim one of the clause's projected
/// items" predicate. The ORDER BY restated-projection exemptions
/// (`RETURN DISTINCT length(p) ORDER BY length(p)`, ReturnOrderBy6 [5]'s
/// `ORDER BY me.age + you.age + count(*)` beside that item) and the
/// grouping-key ambiguity check all decide this same fact, so they ask it
/// here instead of re-spelling the comparison.
///
/// The `RETURN *` / `WITH *` (and the `ALL` spelling) item projects the
/// incoming scope rather than an expression — its `Expr::Star` is a
/// marker, never a value being compared — so it never answers the item
/// question. Deciding that here keeps every caller star-agnostic.
fn is_projected_item(items: &[ReturnItem], expr: &Expr) -> bool {
    items.iter().any(|item| !item.all && item.expr == *expr)
}

/// The one construction of the grouping-key ambiguity rejection: the
/// projection-item check and the WITH ORDER BY check judge the same
/// policy on different expression sets, so the error they raise is
/// built once (LABS-295: two inline copies spelled the clause
/// independently).
fn ambiguous_aggregation_error() -> EngineError {
    EngineError::semantic(
        ErrorKind::AmbiguousAggregationExpression,
        "expression contains both aggregation and non-grouped variables".to_string(),
    )
}

/// The per-item checks RETURN and WITH share (F-1-9): nested aggregation,
/// non-constant aggregation arguments, and grouping-key ambiguity in
/// mixed expressions.
fn check_projection_items(
    items: &[ReturnItem],
    grouping_key_exprs: &[&Expr],
) -> Result<(), EngineError> {
    let keys = GroupingKeys::new(items, grouping_key_exprs);
    for item in items {
        check_nested_aggregation(&item.expr)?;
        check_non_constant_in_aggregation(&item.expr)?;

        // Ambiguous aggregation: is_ambiguous_aggregation owns both
        // preconditions.
        if is_ambiguous_aggregation(&item.expr, &keys) {
            return Err(ambiguous_aggregation_error());
        }
    }
    Ok(())
}

/// A projection clause (`RETURN`/`WITH`) as one value: the parts every
/// projection check consumes, plus which kind it is (a WITH replaces the
/// scope driver's scope; a RETURN is final). The one extraction — the scope
/// driver and the ORDER BY battery both consume it
/// (AGENTS.md one-canonical-helper-per-policy).
struct ProjectionParts<'a> {
    /// Which projection kind the clause is: a WITH replaces the scope
    /// driver's scope; a RETURN is final.
    kind: ProjectionKind,
    items: &'a [ReturnItem],
    group_by: Option<&'a GroupBySpec>,
    order_by: &'a [OrderByItem],
    /// The WITH clause's `WHERE` subclause (a RETURN has none): a DISTINCT
    /// or aggregating WITH re-evaluates it over the projected row
    /// ([`order_by::check_projection_subclauses`]).
    where_expr: Option<&'a Expr>,
    distinct: bool,
}

/// The two projection kinds.
enum ProjectionKind {
    Return,
    With,
}

/// The projection parts of a clause, or `None` for a non-projection clause.
fn projection_parts(clause: &Clause) -> Option<ProjectionParts<'_>> {
    match clause {
        Clause::Return(rc) => Some(ProjectionParts {
            kind: ProjectionKind::Return,
            items: &rc.items,
            group_by: rc.group_by.as_ref(),
            order_by: &rc.order_by,
            where_expr: None,
            distinct: rc.distinct,
        }),
        Clause::With(wc) => Some(ProjectionParts {
            kind: ProjectionKind::With,
            items: &wc.items,
            group_by: wc.group_by.as_ref(),
            order_by: &wc.order_by,
            where_expr: wc.where_expr.as_ref(),
            distinct: wc.distinct,
        }),
        _ => None,
    }
}

/// The scope-absorption policy, one owner: a non-projection clause extends
/// the driver's scope with the names the canonical per-clause fold
/// introduces (LABS-518) — except MERGE and CALL-procedure, which stay
/// deliberately out (never tracked here; the scope feeds only the
/// DISTINCT/ORDER BY checks).
fn extend_aggregation_scope(scope: &mut IdentHashSet<SmallName>, clause: &Clause) {
    if !matches!(clause, Clause::Merge(_) | Clause::CallProcedure(_)) {
        scope.extend(clause_introduced_names(clause));
    }
}

/// Validate aggregation semantics across RETURN and WITH clauses.
/// Checks for:
/// - Nested aggregation (count(count(*)))
/// - Non-constant expressions in aggregation (count(rand()))
/// - Ambiguous aggregation expressions (non-grouped variables mixed with aggregation)
/// - Aggregation in ORDER BY when clause has no aggregation is owned by
///   the battery's validate_order_by_aggregation, which runs before this
///   validator (one owner for that policy)
/// - Undefined variables in ORDER BY aggregation expressions
///
pub fn validate_aggregation_semantics(stmt: &Statement) -> Result<(), EngineError> {
    let mut scope: IdentHashSet<SmallName> = IdentHashSet::default();

    for clause in &stmt.clauses {
        // Both projection kinds run one check sequence (LABS-665): the item
        // checks, then the clause-level ORDER BY policy — a WITH additionally
        // replaces the scope with its projected variables.
        match projection_parts(clause) {
            Some(parts) => {
                check_grouping_element_aliases(parts.group_by, parts.items)?;
                // Collect grouping key expressions once for the clause
                // (explicit GROUP BY keys when present, otherwise the
                // non-aggregation items); the item checks and the ORDER BY
                // check share them.
                let grouping_key_exprs = grouping_key_exprs(parts.group_by, parts.items);
                check_projection_items(parts.items, &grouping_key_exprs)?;
                check_clause_aggregate_arity(parts.items, &grouping_key_exprs, parts.order_by)?;

                // No local no-aggregation ORDER BY re-check: the battery's
                // validate_order_by_aggregation (which runs at validate
                // time, before lowering calls this validator, lower/mod.rs)
                // already rejects it with the identical predicate —
                // including the DISTINCT and explicit GROUP BY exemptions
                // (ADR-0453) — one owner for the policy.
                order_by::check_projection_subclauses(&parts, &grouping_key_exprs, &scope)?;

                // A WITH replaces the scope with its projected variables
                // through [`star_sliced_names`]; a RETURN is final.
                if matches!(parts.kind, ProjectionKind::With) {
                    scope = star_sliced_names(parts.items, &scope);
                }
            }
            // [`extend_aggregation_scope`] owns the fold and its two
            // exclusions (MERGE, CALL-procedure).
            None => extend_aggregation_scope(&mut scope, clause),
        }
    }
    Ok(())
}

/// The family's shared test entry: parse and run the full driver — the
/// level the ORDER BY policies are judged at (the battery-only level is
/// each concern module's own helper).
#[cfg(test)]
fn validate_semantics(query: &str) -> Result<(), EngineError> {
    let stmt = relate_parser::parse(query).unwrap();
    validate_aggregation_semantics(&stmt)
}

/// The grouping-key expressions of a projection clause (`WITH`/`RETURN`).
///
/// With an explicit `<group by clause>` the keys are those written by the
/// user: `GROUP BY ALL` means every non-aggregating item, `GROUP BY ()` means
/// none, and `GROUP BY <expr>...` means exactly those expressions. An
/// explicit key that is a bare variable matching a projection alias resolves
/// to that item's expression (the same rule the lowering applies). A
/// property of a *self-shadowing* alias (`a.p AS a` + `GROUP BY a.p`) stays
/// pre-resolution here; only the lowering rewrites it to `(a.p).p`
/// ([`alias_shadows_its_own_base`], `lower::lower_group_by`), because the
/// rewrite builds a new expression and these keys are borrowed. Without a
/// `GROUP BY` subclause the keys are inferred from the non-aggregating items.
/// Borrowed, not owned: every caller immediately re-borrows the result
/// (round-3 review — the owned version deep-cloned each key expression).
fn grouping_key_exprs<'a>(
    group_by: Option<&'a GroupBySpec>,
    items: &'a [relate_parser::ReturnItem],
) -> Vec<&'a Expr> {
    match group_by {
        None | Some(GroupBySpec::All) => non_aggregating(items).map(|i| &i.expr).collect(),
        Some(GroupBySpec::Empty) => Vec::new(),
        Some(GroupBySpec::Keys(keys)) => keys
            .iter()
            .map(|k| match k {
                Expr::Variable(name) => items
                    .iter()
                    .find(|i| is_aliased(i, name))
                    .map(|i| &i.expr)
                    .unwrap_or(k),
                _ => k,
            })
            .collect(),
    }
}

/// The projection items that do not aggregate — the one partition the
/// inferred grouping keys ([`grouping_key_exprs`]) and the alias
/// extension of the WITH ORDER BY check both derive from.
fn non_aggregating(items: &[ReturnItem]) -> impl Iterator<Item = &ReturnItem> {
    items.iter().filter(|i| !contains_aggregation(&i.expr))
}

/// The one alias-matching predicate the grouping-key resolutions share —
/// [`grouping_key_exprs`] here and `lower::lower_group_by` (the third
/// spelling this replaces): whether `item` is explicitly aliased `name`.
/// [`projection_item_expr`] below is the wider name→expression rule (it
/// also resolves a bare-variable item's own name) and spells that extra
/// condition once, on top of this predicate.
pub(crate) fn is_aliased(item: &ReturnItem, name: &SmallName) -> bool {
    item.alias.as_ref() == Some(name)
}

/// GBN1.5: the one grouping-element shape that resolves *through* a
/// projection-item alias — the alias names a property of the variable it
/// shadows (`a.p AS a`, so `GROUP BY a.p` means `(a.p).p`). Every other
/// element that references an alias with a property access is an invalid
/// grouping element (GBN1.6, GroupByAcceptance [47]/[49]/[80]).
pub(crate) fn alias_shadows_its_own_base(alias: &SmallName, item_expr: &Expr) -> bool {
    matches!(
        item_expr,
        Expr::Property(property)
            if matches!(&property.expr, Expr::Variable(root) if root == alias)
    )
}

/// Validate the grouping elements of an explicit `GROUP BY` key list that
/// reference a projection-item alias (GroupByAcceptance [47]/[49]/[80]):
/// a grouping element `alias.<property>` is only valid when the alias
/// shadows the variable it is a property of (GBN1.5); an alias of a
/// different variable is 42I80 — the element must be a simple variable
/// reference, so the key is whatever the aliased expression means.
fn check_grouping_element_aliases(
    group_by: Option<&GroupBySpec>,
    items: &[ReturnItem],
) -> Result<(), EngineError> {
    let Some(GroupBySpec::Keys(keys)) = group_by else {
        return Ok(());
    };
    for key in keys {
        // A bare variable naming an alias is the valid form (it resolves to
        // the item expression, Rule 4.2).
        if matches!(key, Expr::Variable(_)) {
            continue;
        }
        let mut key_vars = IdentHashSet::default();
        collect_expr_variables(key, &mut key_vars);
        // Sorted, not hash order: the error below names `base`, and the
        // reported alias must not follow the process-seeded hash order
        // (ADR-0505 §4; `SmallName`'s `Ord` is name-text order).
        let mut key_vars: Vec<SmallName> = key_vars.into_iter().collect();
        key_vars.sort_unstable();
        for base in &key_vars {
            let Some(item_expr) = projection_item_expr(items, base) else {
                continue;
            };
            // GBN1.5: `a.p AS a` + `GROUP BY a.p` resolves through the
            // shadowing alias to `(a.p).p`; every other alias reference in a
            // grouping element is invalid (GBN1.6).
            if let Expr::Property(property) = key {
                if matches!(&property.expr, Expr::Variable(root) if root == base)
                    && alias_shadows_its_own_base(base, item_expr)
                {
                    continue;
                }
            }
            return Err(EngineError::semantic(
                ErrorKind::InvalidAggregation,
                format!(
                    "invalid grouping element. The grouping element '{}' is not a valid \
                     grouping key. A grouping element that references the projection item \
                     alias `{base}` must be a simple variable reference",
                    grouping_element_text(key)
                ),
            ));
        }
    }
    Ok(())
}

/// The expression a projected name stands for: an explicit alias, or a
/// bare-variable item's own name (`RETURN a` projects the name `a` with the
/// expression `a`). Neo4j's 42I80 speaks of the "projection item alias",
/// which covers both spellings.
fn projection_item_expr<'a>(items: &'a [ReturnItem], name: &SmallName) -> Option<&'a Expr> {
    items
        .iter()
        .find(|item| {
            is_aliased(item, name)
                || (item.alias.is_none() && matches!(&item.expr, Expr::Variable(v) if v == name))
        })
        .map(|item| &item.expr)
}

/// Render a grouping element for the 42I80 description: the expression's
/// source text is not kept on the AST node, so the two shapes a grouping
/// element takes in this rejection — a property access and a binary
/// expression — are spelled structurally.
fn grouping_element_text(expr: &Expr) -> String {
    match expr {
        Expr::Property(property) => match &property.expr {
            Expr::Variable(base) => format!("{base}.{}", property.property),
            other => format!("{}.{}", grouping_element_text(other), property.property),
        },
        Expr::Variable(name) => name.to_string(),
        Expr::Binary(binary) => format!(
            "{} {} {}",
            grouping_element_text(&binary.left),
            binary.op,
            grouping_element_text(&binary.right)
        ),
        _ => "<grouping element>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // LABS-260: an ORDER BY aggregate over the projection's input scope is
    // valid when the DISTINCT projection itself aggregates.
    #[test]
    fn distinct_return_order_by_aggregate_uses_input_scope() {
        // Direct re-aggregation of a projected aggregate.
        let ok = validate_semantics(
            "MATCH (p:Person) RETURN DISTINCT p.community, count(p) ORDER BY count(p) DESC",
        );
        assert!(ok.is_ok(), "projected aggregate in ORDER BY: {ok:?}");

        // Ordering by the aggregate's alias keeps working.
        let ok = validate_semantics(
            "MATCH (p:Person) RETURN DISTINCT p.community, count(p) AS c ORDER BY c DESC",
        );
        assert!(ok.is_ok(), "alias ordering: {ok:?}");

        // A variable that only exists pre-projection but is referenced
        // outside any aggregation is still undefined (ReturnOrderBy2 [13]
        // shape: DISTINCT does not preserve it).
        let err = validate_semantics(
            "MATCH (p:Person) RETURN DISTINCT p.community, count(p) ORDER BY p.name",
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UndefinedVariable);

        // A variable inside the ORDER BY aggregate that was never in scope
        // is still undefined.
        let err = validate_semantics(
            "MATCH (p:Person) RETURN DISTINCT p.community, count(p) ORDER BY count(z)",
        )
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UndefinedVariable);
    }

    // LABS-276 / ADR-0225: every single-input aggregate requires exactly
    // one argument (Neo4j 42I13), wherever it appears in the projection
    // clause — items, GROUP BY keys, ORDER BY expressions, and both the
    // RETURN and WITH forms.
    #[test]
    fn zero_argument_single_input_aggregates_error() {
        // The canonical SINGLE_INPUT_AGGREGATES table drives the loop, so
        // a name added to the table without validation reach fails here
        // first.
        for agg in SINGLE_INPUT_AGGREGATES {
            let err = validate_semantics(&format!("CREATE (n) RETURN {agg}()")).unwrap_err();
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidNumberOfArguments,
                "{agg}() item: {err:?}"
            );
            let err = validate_semantics(&format!("UNWIND [1] AS x WITH {agg}() AS s RETURN s"))
                .unwrap_err();
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidNumberOfArguments,
                "WITH {agg}() item: {err:?}"
            );
        }
    }

    #[test]
    fn zero_argument_aggregate_nested_in_expression_errors() {
        let err = validate_semantics("MATCH (n) RETURN 1 + sum()").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidNumberOfArguments);
        let err = validate_semantics("MATCH (n) RETURN count(*) ORDER BY min()").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidNumberOfArguments);
        let err = validate_semantics("MATCH (n) RETURN n.id GROUP BY max()").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidNumberOfArguments);
    }

    // ADR-0217: the front's arity rejection and the executor's `agg_eval`
    // backstop rejection are one contract — the `AggregateArity` table in
    // `relate-ir`. The core-`Error` channel spells the kind inline in
    // the contract's `rejection`; the front's `EngineError` carries the
    // kind typed and `Display` prefixes it. This pins the byte-equivalence
    // of the two renderings, so neither spelling can drift alone.
    #[test]
    fn front_arity_rejection_matches_the_ir_contract_text() {
        for (name, got) in [("sum", 0), ("stDev", 2), ("collect_list", 3)] {
            let front = arity_rejection(name, got).to_string();
            let contract = AggregateArity::SingleInput.rejection(name, got);
            assert_eq!(
                front, contract,
                "{name} rejection drifted from the contract"
            );
        }
    }

    #[test]
    fn grouping_element_referencing_an_alias_must_be_bare() {
        for query in [
            "WITH 1 AS a RETURN a, count(*) GROUP BY a + 1",
            "WITH {p: 1} AS c, 2 AS b RETURN c AS a, sum(b) AS s GROUP BY a.p",
        ] {
            let err = validate_semantics(query).unwrap_err();
            assert_eq!(
                err.kind(),
                ErrorKind::InvalidAggregation,
                "{query}: {err:?}"
            );
        }
        // A bare alias reference resolves to the aliased item (Rule 4.2).
        assert!(validate_semantics("WITH 1 AS a RETURN a, count(*) GROUP BY a").is_ok());
    }

    #[test]
    fn one_argument_aggregates_and_the_matching_shapes_still_validate() {
        // One-argument calls, count(*), count(x), and the zero-input-row
        // semantics (sum=0, avg/min/max/collect of nothing) are unchanged.
        assert!(validate_semantics(
            "UNWIND [] AS x RETURN sum(x), avg(x), min(x), max(x), collect(x), count(x), count(*)"
        )
        .is_ok());
        assert!(validate_semantics("MATCH (n:Person) RETURN n.age GROUP BY n.age").is_ok());
        assert!(
            validate_semantics("MATCH (n:Person) RETURN count(n) ORDER BY count(n) DESC").is_ok()
        );
    }

    // LABS-744: aggregates under the aggregation-transparent positions are
    // not this module's to reject. The finding's shapes (`RETURN
    // -count(*)`, `-sum(n.v)`) already pass front validation: a Unary
    // operand sits inside the aggregation-transparent reach
    // ([`agg_enter_child`] here, `historical_enter_child` in `crate::agg`),
    // so the item is classified aggregating, and no plan-time rejection of
    // these shapes exists anywhere in the front. The error users see is the
    // executor's scalar-eval backstop ("count(*) only valid in
    // aggregation", eval/mod.rs; "<fn>() is an aggregating function",
    // eval/function.rs), reached through `agg_eval`'s `_` wildcard arm —
    // the gap LABS-744 closes with a shared aggregate-transparent child
    // map in `relate-ir`'s walk.rs, consumed by `agg_eval`. Pin the
    // front's acceptance so that cross-crate fix cannot regress into a
    // plan-time rejection added here.
    #[test]
    fn aggregates_under_transparent_positions_are_not_rejected_here() {
        for query in [
            "MATCH (n) RETURN -count(*)",
            "MATCH (n) RETURN -sum(n.v)",
            "MATCH (n) RETURN n.age, -count(*) GROUP BY n.age",
        ] {
            let ok = validate_semantics(query);
            assert!(ok.is_ok(), "{query} must pass front validation: {ok:?}");
        }
    }

    // The percentile aggregates stay out of the arity table (ADR-0225):
    // their two-argument check lives at their call site, not here.
    #[test]
    fn percentile_aggregates_keep_their_own_call_site_arity_check() {
        assert!(validate_semantics("MATCH (n) RETURN percentileCont(n.age, 0.5)").is_ok());
        // Zero or one argument is not this module's error — the call-site
        // check in the executor rejects it.
        assert!(validate_semantics("MATCH (n) RETURN percentileCont()").is_ok());
        assert!(validate_semantics("MATCH (n) RETURN percentileDisc(n.age)").is_ok());
    }

    // Only a projected *key* is a grouping key: a non-aggregating item the
    // GROUP BY list does not name stays ambiguous, while a `GROUP BY`
    // expression the projection never restates is a key of its own (both
    // sides of the pre-helper behaviour the shared predicate must keep).
    #[test]
    fn grouping_keys_are_the_key_list_not_every_projected_item() {
        let err =
            validate_semantics("MATCH (n) RETURN n.age * 2 + count(*), n.age * 2 GROUP BY n.age")
                .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AmbiguousAggregationExpression);

        assert!(
            validate_semantics("MATCH (n) RETURN n.age * 2 + count(*) GROUP BY n.age * 2").is_ok(),
            "a GROUP BY expression the projection does not restate is a key"
        );
    }

    // The projected-item predicate owns the RETURN `*`/`ALL` decision: a
    // star item projects the incoming scope, not an expression, so it never
    // answers the item question for any caller.
    #[test]
    fn projected_item_excludes_star_items() {
        let star = ReturnItem {
            expr: Expr::Star,
            alias: None,
            all: true,
            source_text: None,
        };
        let named = ReturnItem {
            expr: Expr::Variable("n".into()),
            alias: None,
            all: false,
            source_text: None,
        };
        assert!(!is_projected_item(std::slice::from_ref(&star), &Expr::Star));
        assert!(is_projected_item(
            &[star, named.clone()],
            &Expr::Variable("n".into())
        ));
        assert!(!is_projected_item(&[named], &Expr::Variable("m".into())));
    }
}
