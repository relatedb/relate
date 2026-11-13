//! The post-projection subclause *variable visibility* rules (LABS-665): which variables an ORDER BY item or a WITH `WHERE` may name
//! once a projection has run — a DISTINCT projection's orderable columns, or
//! an aggregating projection's projected aliases and restated items. The
//! aggregation-*shape* battery lives in the parent module.

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::EngineError;
use relate_parser::{Clause, Expr, ReturnItem, Statement};

use super::super::ambiguity::{is_ambiguous_aggregation, GroupingKeys};
use super::super::{
    ambiguous_aggregation_error, is_direct_aggregation, is_projected_item, star_sliced_names,
    ProjectionParts,
};
use super::{projection_mode, ProjectionMode};
use crate::ast::collect::{
    check_names_allowed, clause_introduced_names, collect_expr_variables, expr_child_reaches_scope,
};
use crate::ast::stmt_ref::StmtRef;
use crate::ast::tree::{walk_expr_with, walk_statement_tree};

/// The post-projection subclause driver (LABS-665): one entry for a
/// `RETURN`/`WITH` projection's ORDER BY and a WITH's `WHERE`, so the
/// projection is classified once and the restatable-name set is resolved
/// once for both.
///
/// A DISTINCT or aggregating projection's subclauses run over the projected
/// row, so they may name the projected columns and restate a projected item
/// — nothing else ([`check_post_projection_expr`]). A plain projection's
/// ORDER BY and `WHERE` see the full pre-projection scope: nothing to check
/// here ([`validate_order_by_aggregation`] has already rejected aggregation
/// in its ORDER BY, and the expression validator owns its `WHERE`). The
/// aggregating mode adds the ambiguity check to each ORDER BY item.
///
/// Precondition (caller-enforced): [`validate_order_by_aggregation`] has
/// rejected every aggregating ORDER BY item of a non-aggregating projection
/// (no aggregate item, no `GROUP BY`, not DISTINCT) before this runs.
pub(in crate::validate::aggregation) fn check_projection_subclauses(
    parts: &ProjectionParts,
    grouping_key_exprs: &[&Expr],
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let mode = projection_mode(parts);
    if matches!(mode, ProjectionMode::Plain)
        || (parts.order_by.is_empty() && parts.where_expr.is_none())
    {
        return Ok(());
    }
    let projected = post_projection_names(parts, scope);
    let restatable = post_projection_restatable(parts, projected, scope, grouping_key_exprs);
    // The ambiguity model still owns the mixed aggregation policy
    // (ADR-0198, ADR-0453); the visibility rule owns which names the
    // subclause may read.
    let ambiguous_keys = matches!(mode, ProjectionMode::Aggregating).then(|| {
        GroupingKeys::new(grouping_key_exprs)
            .with_aliases(parts.items.iter().filter_map(|i| i.alias.as_ref().cloned()))
    });
    for ob in parts.order_by {
        check_post_projection_expr(&ob.expr, &restatable)?;
        // The ambiguity predicate self-gates on aggregation-containing
        // expressions, so it runs unconditionally.
        if let Some(keys) = &ambiguous_keys {
            if is_ambiguous_aggregation(&ob.expr, keys) {
                return Err(ambiguous_aggregation_error());
            }
        }
    }
    // A DISTINCT or aggregating WITH's WHERE re-evaluates over the projected
    // row, exactly like its ORDER BY (Neo4j 42N44).
    if let Some(where_expr) = parts.where_expr {
        check_post_projection_expr(where_expr, &restatable)?;
    }
    Ok(())
}

/// What a post-projection subclause may restate, resolved once per clause
/// ([`check_post_projection_expr`] owns the rule).
struct Restatable<'a> {
    /// The clause's projection items (implicit grouping: any projected item
    /// may be restated at any depth).
    items: &'a [ReturnItem],
    /// The projected row's column names (aliases and effective names).
    projected: IdentHashSet<SmallName>,
    /// The pre-projection scope: what an aggregation's arguments resolve
    /// against.
    incoming: &'a IdentHashSet<SmallName>,
    /// The explicit `GROUP BY` keys (including `GROUP BY ALL`/`()`): a key
    /// may be restated as the whole expression however complex it is, and at
    /// any depth when it is a variable or property access. `None` under
    /// implicit grouping, where restating a projected item is the rule.
    keys: Option<&'a [&'a Expr]>,
}

/// Resolve [`Restatable`] for a projection clause: only an explicit
/// `GROUP BY` switches the subclause to the inset-key rule (live Neo4j
/// 2026.09 — `GROUP BY ALL` behaves like an explicit key list, not like
/// implicit grouping).
fn post_projection_restatable<'a>(
    parts: &ProjectionParts<'a>,
    projected: IdentHashSet<SmallName>,
    scope: &'a IdentHashSet<SmallName>,
    grouping_key_exprs: &'a [&'a Expr],
) -> Restatable<'a> {
    Restatable {
        items: parts.items,
        projected,
        incoming: scope,
        keys: parts.group_by.is_some().then_some(grouping_key_exprs),
    }
}

/// The projected row's column names both modes share: the star-aware
/// projection ([`star_sliced_names`]) — a `*` item carries the *incoming*
/// scope through, so `WITH *, min(1) AS m WHERE p IS NOT NULL` keeps `p`
/// visible after the projection (verified live Neo4j 2026.09).
fn post_projection_names(
    parts: &ProjectionParts,
    scope: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    star_sliced_names(parts.items, scope)
}

/// The one post-projection subclause visibility rule, shared by ORDER BY and
/// WITH `WHERE` (verified against live Neo4j 2026.09 under `CYPHER 25`):
///
/// - the whole subclause expression restating a column is that column — a
///   projected item under implicit grouping, a `GROUP BY` key under an
///   explicit one (`GROUP BY b + 1 ORDER BY b + 1`);
/// - every other variable must name a projected column or a `GROUP BY` key;
/// - under implicit grouping a projected item is also restatable at any
///   depth, so `ORDER BY 1 + (a.p + 0)` beside `RETURN a.p + 0 AS b` is
///   accepted (it is the deprecated-but-valid form);
/// - under an explicit `GROUP BY` a key is restatable at depth only when it
///   is a variable or property access (`GROUP BY a.p ORDER BY 2 + a.p + 1`)
///   — a complex key is whole-expression-only, so `GROUP BY a.p + 0` and
///   `ORDER BY 1 + (a.p + 0)` is 42N44;
/// - an aggregation re-evaluates over the projection's input, so the
///   variables inside it are checked against the incoming scope (Neo4j
///   42I79: a projected alias is not visible inside one);
/// - a subquery expression re-evaluates over the projected row, so a
///   pre-projection variable it references is inaccessible (Neo4j 42N44:
///   `ORDER BY COUNT { RETURN a.p } + 0` beside a projected
///   `COUNT { RETURN a.p }`).
fn check_post_projection_expr(expr: &Expr, restatable: &Restatable<'_>) -> Result<(), EngineError> {
    // A whole subclause expression that restates a column is that column —
    // of any expression kind, a subquery included.
    if restatable.restates_whole(expr) {
        return Ok(());
    }
    let mut visit = |node: &Expr| -> Result<(), EngineError> {
        if let Some(statement) = subquery_statement(node) {
            return check_subquery_correlation(
                statement,
                &restatable.projected,
                restatable.incoming,
            );
        }
        if is_direct_aggregation(node) {
            let mut vars = IdentHashSet::default();
            collect_expr_variables(node, &mut vars);
            return check_names_allowed(&vars, |v| restatable.incoming.contains(v));
        }
        match node {
            Expr::Variable(v)
                if !restatable.projected.contains(v) && !restatable.is_variable_key(v) =>
            {
                Err(v.with_str(EngineError::undefined_variable))
            }
            _ => Ok(()),
        }
    };
    // A restated column and an aggregation's arguments are opaque: their
    // inner variables belong to the projection, not to the subclause scope.
    // A subquery stays walkable even when it restates a column — a subquery
    // is *not* substituted as a subexpression, it re-evaluates over the
    // projected row. The scope-reach filter keeps comprehension/quantifier/
    // reduce bindings out of the check, as they are for
    // [`collect_expr_variables`].
    let enter_child = |parent: &Expr, child: &Expr| {
        expr_child_reaches_scope(parent, child)
            && !is_direct_aggregation(parent)
            && !restatable.restates_child(child)
    };
    walk_expr_with(expr, &mut visit, enter_child)
}

impl Restatable<'_> {
    /// The whole expression restates a column: a projected item under
    /// implicit grouping (any shape), a `GROUP BY` key under an explicit
    /// one (any shape).
    fn restates_whole(&self, expr: &Expr) -> bool {
        match self.keys {
            Some(keys) => keys.iter().any(|key| **key == *expr),
            None => is_projected_item(self.items, expr),
        }
    }

    /// A subexpression restates a column without being re-resolved: a
    /// projected item under implicit grouping, or a *simple* (variable or
    /// property access) explicit grouping key. A subquery never restates —
    /// it re-evaluates over the projected row ([`is_subquery`]).
    fn restates_child(&self, expr: &Expr) -> bool {
        if is_subquery(expr) {
            return false;
        }
        match self.keys {
            Some(keys) => {
                matches!(expr, Expr::Variable(_) | Expr::Property(_))
                    && keys.iter().any(|key| **key == *expr)
            }
            None => is_projected_item(self.items, expr),
        }
    }

    /// A bare variable that is itself an explicit grouping key
    /// (`GROUP BY a ORDER BY 1 + a`).
    fn is_variable_key(&self, name: &SmallName) -> bool {
        self.keys.is_some_and(|keys| {
            keys.iter()
                .any(|key| matches!(key, Expr::Variable(v) if v == name))
        })
    }
}

/// The inner statement of a subquery expression (`EXISTS`/`COUNT`/`COLLECT
/// { ... }`), which re-evaluates its inner statement rather than being a
/// value the projection already computed — `None` for every other
/// expression. The vocabulary is spelled once here; [`is_subquery`] and
/// the correlation check derive from it.
fn subquery_statement(expr: &Expr) -> Option<&Statement> {
    match expr {
        Expr::ExistsSubquery(e) => Some(&e.statement),
        Expr::CountSubquery(e) => Some(&e.statement),
        Expr::CollectSubquery(e) => Some(&e.statement),
        _ => None,
    }
}

/// Whether `expr` is a subquery expression ([`subquery_statement`]'s
/// vocabulary).
fn is_subquery(expr: &Expr) -> bool {
    subquery_statement(expr).is_some()
}

/// A subquery expression in a post-projection subclause re-evaluates over
/// the projected row: its own bindings and the projected columns are
/// visible, an incoming-only variable is not (Neo4j 42N44).
fn check_subquery_correlation(
    statement: &Statement,
    projected: &IdentHashSet<SmallName>,
    incoming: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let mut used = IdentHashSet::default();
    let mut introduced = IdentHashSet::default();
    let mut no_stmt = |_: StmtRef<'_>| Ok(());
    let mut collect_introduced = |clause: &Clause| {
        introduced.extend(clause_introduced_names(clause));
        Ok(())
    };
    let mut collect_used = |node: &Expr| {
        if let Expr::Variable(v) = node {
            used.insert(v.clone());
        }
        Ok(())
    };
    walk_statement_tree(
        statement,
        &mut no_stmt,
        &mut collect_introduced,
        &mut collect_used,
    )?;
    // The sorted-first offender (`SmallName`'s `Ord` is name-text order,
    // ADR-0505 §4).
    let offender = used
        .iter()
        .filter(|v| incoming.contains(*v) && !projected.contains(*v) && !introduced.contains(*v))
        .min();
    match offender {
        Some(v) => Err(v.with_str(EngineError::undefined_variable)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::validate_semantics;
    use relate_error::ErrorKind;

    /// The one refusal assertion every negative test makes.
    fn assert_undefined(query: &str) {
        match validate_semantics(query) {
            Ok(()) => panic!("expected an UndefinedVariable error, got Ok: {query}"),
            Err(err) => assert_eq!(err.kind(), ErrorKind::UndefinedVariable, "{query}"),
        }
    }

    /// The one acceptance assertion every positive test makes.
    fn assert_accepted(query: &str) {
        let result = validate_semantics(query);
        assert!(result.is_ok(), "{query}: {result:?}");
    }

    // LABS-665 (Jepsen Class A): the Jepsen-shrunk one-liner — Relate
    // returned a row, Neo4j rejects ("not possible to access variables
    // declared before the WITH/RETURN").
    #[test]
    fn aggregating_projection_order_by_rejects_a_pre_aggregation_variable() {
        let err =
            validate_semantics("MATCH (b) RETURN count(*) AS c ORDER BY b.num DESC").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UndefinedVariable);

        // The WITH twin of the same shape.
        let err =
            validate_semantics("MATCH (b) WITH count(*) AS c ORDER BY b.num RETURN c").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UndefinedVariable);
    }

    // An explicit GROUP BY aggregates even with no aggregate item
    // (projection_aggregates), so the restriction applies — an in-scope
    // variable that is not a projected column is refused, on RETURN and on
    // the WITH twin alike. A grouping-key *expression* is accessible only as
    // a restated column, never through its variables (verified live Neo4j
    // 2026.09: `GROUP BY b.name ORDER BY b.other` is 42N44 "inaccessible
    // variable", not an accepted key-variable read).
    #[test]
    fn explicit_group_by_order_by_rejects_a_non_key_variable() {
        for query in [
            "MATCH (b)-[r]->(d) RETURN b.name AS n GROUP BY b.name ORDER BY d.x",
            "MATCH (b)-[r]->(d) WITH b.name AS n GROUP BY b.name ORDER BY d.x RETURN n",
            "MATCH (b) RETURN b.name AS n GROUP BY b.name ORDER BY b.other",
            "MATCH (b) RETURN b.name AS n GROUP BY b.name ORDER BY b",
        ] {
            assert_undefined(query);
        }
        // The key expression itself, and an alias, stay orderable.
        for query in [
            "MATCH (b) RETURN b.name AS n GROUP BY b.name ORDER BY b.name",
            "MATCH (b) RETURN b.name AS n GROUP BY b.name ORDER BY n",
            "MATCH (b) RETURN b.name AS n GROUP BY b.name ORDER BY 1 + n",
        ] {
            assert_accepted(query);
        }
    }

    // A DISTINCT projection restricts ORDER BY to the orderable columns,
    // on WITH exactly as on RETURN (the two kinds share one policy).
    #[test]
    fn distinct_projection_order_by_is_restricted_on_both_kinds() {
        for query in [
            "MATCH (b) RETURN DISTINCT b.name AS w ORDER BY b.age DESC",
            "MATCH (b) WITH DISTINCT b.name AS w ORDER BY b.age RETURN w",
        ] {
            assert_undefined(query);
        }
    }

    // A projected alias is not visible *inside* an ORDER BY aggregate
    // (Neo4j 42I79, verified live), on every projection kind.
    #[test]
    fn projected_alias_not_visible_inside_order_by_aggregate() {
        for query in [
            "MATCH (b) RETURN count(*) AS c ORDER BY sum(c)",
            "MATCH (b) WITH count(*) AS c ORDER BY sum(c) RETURN c",
            "MATCH (b) RETURN DISTINCT count(*) AS c ORDER BY sum(c)",
        ] {
            assert_undefined(query);
        }
    }

    // LABS-666 (Jepsen Class B): a DISTINCT projection's ORDER BY sees the
    // projected columns and the exact pre-alias variables — nothing else.
    // Every shape below is verified against live Neo4j 2026.09 (CYPHER 25):
    // the accepted ones return rows there, the refused ones get 42N44
    // ("inaccessible variable").
    #[test]
    fn distinct_order_by_visibility_matches_neo4j() {
        // Accepted: the pre-alias variable, a restated projected item
        // (however it is spelled), and a second projected column.
        for query in [
            "MATCH (b) RETURN DISTINCT b.name AS w ORDER BY b.name DESC",
            "MATCH (b) RETURN DISTINCT [b.name] AS w ORDER BY [b.name] DESC",
            "MATCH (b) RETURN DISTINCT b.name AS w, b.id ORDER BY b.id",
            "MATCH (b) WITH DISTINCT b.name AS w ORDER BY b.name RETURN w",
        ] {
            assert_accepted(query);
        }
        // Refused (Neo4j 42N44): a different expression DISTINCT does not
        // preserve — another property, a function over it, a relationship
        // variable, or a variable wrapped by the projected expression.
        for query in [
            "MATCH (b) RETURN DISTINCT b.name AS w ORDER BY b.age DESC",
            "MATCH (b) RETURN DISTINCT b.name AS w ORDER BY toString(b.id)",
            "MATCH (b)-[r]->(d) RETURN DISTINCT b.name AS w ORDER BY r.x DESC",
            "MATCH (b) RETURN DISTINCT coalesce(b.name,'x') AS w ORDER BY b.name DESC",
            "MATCH (b) RETURN DISTINCT toString(b.id) AS w ORDER BY b.id",
            "MATCH (b) WITH DISTINCT b.name AS w ORDER BY b.age RETURN w",
        ] {
            assert_undefined(query);
        }
    }

    // The accepted shapes across every rule: the grouping key, the alias, a
    // restated projected expression, a non-aggregating RETURN (whose ORDER
    // BY may use any in-scope variable), and DISTINCT on the pre-alias
    // variable.
    #[test]
    fn order_by_accepted_shapes_stay_accepted() {
        for query in [
            "MATCH (b) RETURN b.name AS n, count(*) AS c ORDER BY b.name",
            "MATCH (b) RETURN count(*) AS c ORDER BY c DESC",
            "MATCH (b) RETURN b.name AS n, count(*) AS c ORDER BY count(*) DESC",
            "MATCH (b) RETURN b.name AS n ORDER BY b.num",
            "MATCH (b) RETURN DISTINCT b.name AS w ORDER BY b.name DESC",
        ] {
            assert_accepted(query);
        }
    }

    // The ProjectionSubclauseDeprecationAcceptance "unrecognized"
    // forms. The deprecated pre-Cypher-25 projection forms warn; these are
    // the ones Neo4j *rejects* (42N44 "inaccessible variable") and
    // Relate accepted — the grouping key's variables were treated as
    // orderable instead of only the projected columns. Verified live Neo4j
    // 2026.09 under CYPHER 25.
    //
    // One case per form: the wrapper around the projected key expression
    // (`1 + <expr>`, `<expr> + 1`) on RETURN and WITH, the subquery form,
    // and the WITH `WHERE` subclause.
    #[test]
    fn aggregating_order_by_rejects_unrecognized_complex_expressions() {
        let prelude = "UNWIND [1] AS x WITH x AS a ";
        for tail in [
            // [35] RETURN ORDER BY `1 + <expr>`
            "RETURN a.p + 0 AS b, sum(a.p) AS s ORDER BY 1 + a.p + 0",
            // [37] RETURN ORDER BY `<expr> + 1` (here `a.p > 0 + 1` parses
            // as `a.p > (0 + 1)`, a different expression from the item)
            "RETURN a.p > 0, sum(a.p) AS s ORDER BY a.p > 0 + 1",
            // [42] WITH ORDER BY `1 + <expr>`
            "WITH a.p + 0 AS b, sum(a.p) AS s ORDER BY 1 + a.p + 0 RETURN s",
            // [44] WITH ORDER BY `<expr> + 1`
            "WITH a.p > 0 AS b, sum(a.p) AS s ORDER BY a.p > 0 + 1 RETURN s",
            // [49] WITH WHERE
            "WITH a.p + 0 AS b, sum(a.p) AS s WHERE 0 < 1 + a.p + 0 RETURN s",
            // [54] RETURN ORDER BY subquery expression
            "RETURN COUNT { RETURN a.p + 1 AS cnt } AS b, sum(a.p) AS s \
             ORDER BY COUNT { RETURN a.p + 1 AS cnt } + 0",
            // [59] WITH ORDER BY subquery expression
            "WITH COUNT { RETURN a.p + 1 AS cnt } AS b, sum(a.p) AS s \
             ORDER BY COUNT { RETURN a.p + 1 AS cnt } + 0 RETURN s",
        ] {
            assert_undefined(&format!("{prelude}{tail}"));
        }
    }

    // The other half of [54]/[59]: a subquery that is not the whole item is
    // re-evaluated over the projected row, so its correlated pre-projection
    // variable is inaccessible even when it appears alone.
    #[test]
    fn subquery_in_post_projection_subclause_rejects_correlation() {
        let prelude = "UNWIND [1] AS x WITH x AS a ";
        for tail in [
            "RETURN COUNT { RETURN a.p } AS b, sum(a.p) AS s ORDER BY 1 + COUNT { RETURN a.p }",
            "RETURN COUNT { RETURN a.p } + 0 AS b, sum(a.p) AS s \
             ORDER BY 0 + COUNT { RETURN a.p } + 0",
            "WITH a.p + 0 AS b, sum(a.p) AS s WHERE 0 < 1 + COUNT { RETURN a.p } RETURN s",
        ] {
            assert_undefined(&format!("{prelude}{tail}"));
        }
        // A subquery over a projected alias (and over constants) is fine.
        for tail in [
            "WITH a.p + 0 AS b, sum(a.p) AS s ORDER BY 0 + COUNT { RETURN b } RETURN s",
            "WITH a.p + 0 AS b, sum(a.p) AS s WHERE COUNT { RETURN b } > 0 RETURN s",
            "RETURN COUNT { RETURN 1 } AS b, sum(a.p) AS s ORDER BY COUNT { RETURN 1 } + 0",
        ] {
            assert_accepted(&format!("{prelude}{tail}"));
        }
    }

    // The "equal" and "recognized" shapes stay accepted: a restated projected
    // item is that column at the top level and as a subexpression, an alias
    // is readable anywhere, and a non-aggregating WITH's WHERE still sees
    // the pre-projection scope.
    #[test]
    fn post_projection_restatements_and_aliases_stay_accepted() {
        let prelude = "UNWIND [1] AS x WITH x AS a ";
        for tail in [
            // restated item as a subexpression
            "RETURN a.p + 0 AS b, sum(a.p) AS s ORDER BY 1 + (a.p + 0)",
            "RETURN a.p + 0 AS b, sum(a.p) AS s ORDER BY a.p + 0 + 1",
            "RETURN a.p * 0 AS b, sum(a.p) AS s ORDER BY a.p * 0 + 1",
            "WITH a.p + 0 AS b, sum(a.p) AS s ORDER BY 1 + (a.p + 0) RETURN s",
            // aliases
            "RETURN a.p + 0 AS b, sum(a.p) AS s ORDER BY 1 + b",
            "WITH a.p + 0 AS b, sum(a.p) AS s ORDER BY b + 1 RETURN s",
            // WITH WHERE: restated item and alias
            "WITH a.p + 0 AS b, sum(a.p) AS s WHERE a.p + 0 > 0 RETURN s",
            "WITH a.p + 0 AS b, sum(a.p) AS s WHERE 0 < a.p + 0 RETURN s",
            "WITH a.p + 0 AS b, sum(a.p) AS s WHERE 1 + b > 0 RETURN s",
            // exact subquery item
            "RETURN COUNT { RETURN a.p + 1 AS cnt } AS b, sum(a.p) AS s \
             ORDER BY COUNT { RETURN a.p + 1 AS cnt }",
            "RETURN COUNT { RETURN a.p + 1 AS cnt } + 0 AS b, sum(a.p) AS s \
             ORDER BY COUNT { RETURN a.p + 1 AS cnt } + 0 + 0",
        ] {
            assert_accepted(&format!("{prelude}{tail}"));
        }
        // A non-aggregating WITH's WHERE keeps the pre-projection scope.
        assert_accepted(
            "UNWIND [{a: 1}] AS row WITH row.a AS a WITH a.p AS b WHERE a.q > 0 RETURN b",
        );
    }

    // An explicit GROUP BY's "inset" keys (GroupByAcceptance [12], [14],
    // [23], [67]-[70], [75], verified live Neo4j 2026.09): a variable or
    // property key is restatable at any depth, a complex key only as the
    // whole subclause expression. GROUP BY ALL follows the explicit rule,
    // not the implicit one.
    #[test]
    fn explicit_group_by_inset_keys_follow_neo4j() {
        let prelude = "UNWIND [{a: {p: 1}, b: 20, c: 100}] AS row \
                       WITH row.a AS a, row.b AS b, row.c AS c ";
        for tail in [
            "RETURN count(c) AS cnt GROUP BY a.p ORDER BY a.p",
            "RETURN count(c) AS cnt GROUP BY a.p ORDER BY 1 + a.p",
            "RETURN a.p + 1 AS x, count(c) AS cnt GROUP BY a.p ORDER BY 2 + a.p + 1",
            "RETURN b + 1 AS b1, count(c) AS cnt GROUP BY b + 1 ORDER BY b + 1",
            "RETURN count(c) AS cnt GROUP BY a ORDER BY 1 + a",
            "WITH count(c) AS cnt GROUP BY a.p WHERE 1 + a.p > 0 RETURN cnt",
            "RETURN a.p AS k, count(c) AS cnt GROUP BY ALL ORDER BY 1 + a.p",
            "RETURN a.p + 0 AS k, count(c) AS cnt GROUP BY ALL ORDER BY a.p + 0",
        ] {
            assert_accepted(&format!("{prelude}{tail}"));
        }
        for tail in [
            "RETURN count(c) AS cnt GROUP BY a.p + 0 ORDER BY 1 + (a.p + 0)",
            "RETURN count(c) AS cnt GROUP BY a.p + 0 ORDER BY a.p + 0 + 1",
            "RETURN count(c) AS cnt GROUP BY [a.p] ORDER BY size([a.p])",
            "WITH count(c) AS cnt GROUP BY a.p + 0 WHERE 1 + (a.p + 0) > 0 RETURN cnt",
            "RETURN a.p + 0 AS k, count(c) AS cnt GROUP BY ALL ORDER BY 1 + (a.p + 0)",
            "RETURN count(c) AS cnt GROUP BY a.p ORDER BY a.q",
        ] {
            assert_undefined(&format!("{prelude}{tail}"));
        }
    }

    // A `*` item carries the incoming scope through the projection, so its
    // names stay visible in an aggregating WITH's WHERE (ProjectedPaths
    // Acceptance: `WITH *, min(1) AS min WHERE p IS NOT NULL`).
    #[test]
    fn star_projection_keeps_the_incoming_scope_visible() {
        assert_accepted(
            "MATCH p = () WITH *, min(1) AS min WHERE p IS NOT NULL RETURN DISTINCT 1 AS foo",
        );
        assert_accepted(
            "MATCH p = () WITH *, count(*) AS c ORDER BY p IS NOT NULL RETURN DISTINCT 1 AS foo",
        );
    }

    // A DISTINCT WITH's WHERE is restricted like its ORDER BY (live Neo4j
    // 2026.09: the pre-projection variable is 42N44).
    #[test]
    fn distinct_with_where_is_restricted() {
        assert_accepted(
            "UNWIND [1] AS x WITH x AS a WITH DISTINCT a.p AS w WHERE a.p > 0 RETURN w",
        );
        assert_accepted(
            "UNWIND [1] AS x WITH x AS a WITH DISTINCT a.p AS w WHERE 1 + a.p > 0 RETURN w",
        );
        assert_undefined(
            "UNWIND [1] AS x WITH x AS a WITH DISTINCT a.p AS w WHERE a.q > 0 RETURN w",
        );
    }
}
