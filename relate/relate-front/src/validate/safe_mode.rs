//! The opt-in safe-mode shape rules (LABS-680, ADR-0477): label-less new
//! nodes and unbounded reads. Kept in its own concern module so the
//! clause-composition rules in [`super::structure`] stay about composition
//! and the policy has one home.

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{CallSubqueryClause, Clause, Expr, PatternElement, PatternPath, ReturnItem};

use crate::agg::{
    contains_non_collect_aggregation, contains_pattern_comprehension, contains_unbounded_aggregate,
};
use crate::ast::collect::{clause_node_names, projected_node_scope};
use crate::ast::naming::effective_expr;

/// The opt-in safe-mode shape rules (LABS-680, ADR-0477).
///
/// Safe mode is a policy, not a database limit: every rule below refuses a
/// shape an agent commonly gets wrong and is deliberately narrow, so direct
/// human use (which never passes `--safe`) is unaffected and the ordinary
/// engine still admits the same statements. Wired through
/// [`super::validate_statement_tree_with_options_and_policy`]'s per-statement
/// battery, so it sees every nested body at every depth before anything
/// lowers.
///
/// Two guardrails:
///
/// 1. **Label-less new nodes.** A `CREATE`/`MERGE` node pattern with a
///    variable outside the live scope at the write and no label is refused.
///    Anonymous `()` endpoints (the relationship-only
///    `CREATE ()-[r:X]->()` form) and endpoints bound by an earlier clause
///    are exempt; `WITH` replaces the scope with its projections, and an
///    `OPTIONAL MATCH` binding stays unbound for this purpose.
/// 2. **Unbounded reads.** A body whose final `RETURN`/`WITH` has no
///    `LIMIT` and no bounding aggregate, after an earlier `MATCH`, is
///    refused. The aggregate exemption is per item: any `collect()` item
///    refuses even beside a reducing aggregate, because collect() buffers
///    and returns every row. A constant-only `RETURN 1`, a SHOW/CALL-only
///    statement, and a write tail (`WITH … SET`/`DELETE`) are exempt.
pub(crate) fn validate_statement_shape(
    clauses: &[Clause],
    safe_mode: bool,
) -> Result<(), EngineError> {
    // The bool resolves to the default feature set carried on the one
    // registry. The `SafetyFeature` vocabulary and its default flag are the
    // seam a later `--safe=<feature list>` (LABS-703) grows on: a parsed list
    // selects a subset of these rows without moving any guardrail code.
    for (_, check, default_on) in SAFETY_CHECKS {
        if safe_mode && *default_on {
            check(clauses)?;
        }
    }
    Ok(())
}

/// One guardrail safe mode can enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SafetyFeature {
    /// Guardrail 1: label-less new nodes.
    RefuseLabelLessCreate,
    /// Guardrail 2: unbounded reads.
    RefuseUnboundedRead,
}

/// The per-guardrail check signature.
type ShapeCheck = fn(&[Clause]) -> Result<(), EngineError>;

/// The feature-to-check table: one row per guardrail, in error-precedence
/// order (the historical label check runs before the read check). The third
/// field is whether `safe_mode: true` turns the row on by default; LABS-703's
/// declarative `--safe=<feature list>` will select a subset of these rows.
const SAFETY_CHECKS: &[(SafetyFeature, ShapeCheck, bool)] = &[
    (
        SafetyFeature::RefuseLabelLessCreate,
        validate_labeled_writes,
        true,
    ),
    (
        SafetyFeature::RefuseUnboundedRead,
        validate_bounded_reads,
        true,
    ),
];

/// The one safe-mode refusal constructor: the message names the shape, the
/// remedy, and that this is a policy rather than a database limit.
fn safe_mode_violation(shape: &str, remedy: &str) -> EngineError {
    EngineError::semantic(
        ErrorKind::SafeModeViolation,
        format!(
            "This statement is not allowed in safe mode: {shape}. Safe mode refuses \
             shapes an agent commonly gets wrong — {remedy}. This is a policy, not a \
             database limit; reopen without --safe to allow it."
        ),
    )
}

/// Guardrail 1: a new named node in CREATE/MERGE must carry a label.
///
/// The check runs against the **live node scope** at the write, not an
/// additive history of every name seen earlier: `WITH` replaces the scope
/// with its node-safe projections (so `MATCH (n) WITH 1 AS x CREATE (n)`
/// creates a new label-less node, not the dropped `n`), and an `OPTIONAL
/// MATCH` binding may be null, so it does not prove a variable names an
/// existing node (a `CREATE` over it stays subject to the guardrail). `WITH *`
/// keeps the current scope and adds its other projections, via
/// [`projected_node_scope`]'s star handling.
fn validate_labeled_writes(clauses: &[Clause]) -> Result<(), EngineError> {
    let mut scope: IdentHashSet<SmallName> = IdentHashSet::default();
    for clause in clauses {
        match clause {
            Clause::Create(cc) => check_new_nodes_labeled(&cc.pattern, &scope)?,
            Clause::Merge(mc) => {
                check_new_nodes_labeled(std::slice::from_ref(&mc.pattern), &scope)?
            }
            _ => {}
        }
        scope = node_scope_step(clause, &scope);
    }
    Ok(())
}

/// The node-safe scope after one clause: a `WITH` replaces it (carrying
/// node-ness only through bare node-safe variables or `*`); an `OPTIONAL
/// MATCH`/`OPTIONAL CALL` binding may be null, so it adds nothing; an inline
/// subquery adds its node-safe exports; anything else adds only its pattern
/// node variables (`UNWIND`/`LOAD CSV` aliases and procedure `YIELD`s bind
/// non-node values the executor will not reuse as a node).
fn node_scope_step(clause: &Clause, scope: &IdentHashSet<SmallName>) -> IdentHashSet<SmallName> {
    match clause {
        Clause::With(with) => projected_node_scope(&with.items, scope),
        Clause::Match(mc) if mc.optional => scope.clone(),
        Clause::CallSubquery(cs) if cs.optional => scope.clone(),
        Clause::CallSubquery(cs) => {
            let mut out = scope.clone();
            out.extend(node_safe_call_exports(cs, scope));
            out
        }
        _ => {
            let mut out = scope.clone();
            out.extend(clause_node_names(clause));
            out
        }
    }
}

/// The node-safe names a `CALL { ... }` subquery exports: the node-safe names
/// of its own final projection, resolved against the scope inside the
/// subquery (which starts from the enclosing scope). A non-node export —
/// `CALL { RETURN 1 AS m }` — is not node-safe, so a label-less `CREATE` over
/// it stays refused.
fn node_safe_call_exports(
    cs: &CallSubqueryClause,
    outer: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    let inner = node_scope_after(&cs.statement.clauses, outer);
    match last_projection(&cs.statement.clauses) {
        Some((_, _, items)) => projected_node_scope(items, &inner),
        None => IdentHashSet::default(),
    }
}

/// The node-safe scope after walking a whole clause list from `initial`.
fn node_scope_after(
    clauses: &[Clause],
    initial: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    let mut scope = initial.clone();
    for clause in clauses {
        scope = node_scope_step(clause, &scope);
    }
    scope
}

fn check_new_nodes_labeled(
    patterns: &[PatternPath],
    bound: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for path in patterns {
        for element in &path.elements {
            check_element_labeled(element, bound)?;
        }
    }
    Ok(())
}

fn check_element_labeled(
    element: &PatternElement,
    bound: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    match element {
        PatternElement::Node(node) => {
            if let Some(variable) = &node.variable {
                if node.label_expr.is_none() && !bound.contains(variable) {
                    return Err(safe_mode_violation(
                        &format!("CREATE or MERGE of a new node `{variable}` without a label"),
                        &format!(
                            "give the node a label, e.g. `CREATE ({variable}:Person)`, so the \
                             write is self-describing"
                        ),
                    ));
                }
            }
        }
        PatternElement::Relationship(_) => {}
        PatternElement::Quantified(quantified) => {
            for inner in &quantified.elements {
                check_element_labeled(inner, bound)?;
            }
        }
    }
    Ok(())
}

/// Guardrail 2: a read that starts with MATCH must bound its result.
fn validate_bounded_reads(clauses: &[Clause]) -> Result<(), EngineError> {
    // The body's own final projection drives the write-tail check.
    if let Some((position, _, _)) = last_projection(clauses) {
        // A write tail (`WITH … SET`/`CREATE`/`DELETE`) is the point of the
        // body; the design exempts MATCH … SET/DELETE shapes.
        if clauses[position + 1..]
            .iter()
            .any(crate::ast::collect::is_updating_clause)
        {
            return Ok(());
        }
    }
    // The projection whose result the statement returns, and the clause list
    // that owns it.
    let Some(proj) = result_projection(clauses) else {
        // SHOW/CALL-only statements (and write-only bodies) have no
        // projecting clause to bound.
        return Ok(());
    };
    // The rule needs a graph read to bound: a `MATCH`, or a pattern
    // comprehension in any projection — `RETURN [(:P)-[:R]->(m) | m]`
    // matches against the whole graph without a `MATCH` clause and fans out
    // per row, so it is an unbounded read too. Scanning every `RETURN`/`WITH`
    // (not only the final one) catches the `WITH [ … ] AS ms RETURN ms`
    // shape, and a trailing subquery contributes its own reads.
    if !reads_graph(clauses) {
        return Ok(());
    }
    // A trailing `CALL { ... }` runs once per outer row, so its own
    // `LIMIT`/aggregates bound only its own run — with a graph read in the
    // body, the statement is unbounded.
    if proj.trailing {
        return Err(safe_mode_violation(
            UNBOUNDED_READ_SHAPE,
            "add `LIMIT <n>` to the outer query (the subquery runs once per row, so its \
             own `LIMIT` does not bound the result)",
        ));
    }
    if proj.limit.is_some() {
        return Ok(());
    }
    // A pattern comprehension reads the whole graph wherever it sits (a
    // projection item, a `WHERE`/`ORDER BY`); only a `LIMIT` bounds it, so no
    // aggregate exemption applies.
    if has_pattern_comprehension_read(clauses) {
        return Err(safe_mode_violation(
            UNBOUNDED_READ_SHAPE,
            "add `LIMIT <n>` — a pattern comprehension reads the whole graph, so an \
             aggregate does not bound it",
        ));
    }
    // The projection *immediately* before the final one bounds the stream:
    // its own `LIMIT` caps it, or it reduces every item (`WITH count(n) AS c`)
    // to one row per group, so `RETURN c` is bounded too. It must be the
    // immediately-preceding clause: an earlier reducing `WITH` followed by
    // another `MATCH` re-expands the stream.
    if proj.position > 0 {
        if let Clause::With(with) = &proj.owner[proj.position - 1] {
            if with.limit.is_some()
                || with
                    .items
                    .iter()
                    .all(|item| is_bounding_item(effective_expr(item)))
            {
                return Ok(());
            }
        }
    }
    // The exemption is per projected item: one `collect()` beside a
    // reducing aggregate still returns the whole group, so it must not ride
    // the reducing aggregate's exemption.
    if proj
        .items
        .iter()
        .any(|item| contains_unbounded_aggregate(&item.expr))
    {
        return Err(safe_mode_violation(
            UNBOUNDED_READ_SHAPE,
            "add `LIMIT <n>` (collect() buffers every row, so it does not count as a \
             bound even beside count() or another reducing aggregate; use count() for a \
             bounded total)",
        ));
    }
    // The exemption is per projected item: the result is bounded only when
    // *every* item reduces to a scalar — a reducing aggregate or a constant.
    // A bare variable or property is a grouping key, and Cypher returns one
    // row per distinct key, so `RETURN n, count(*)` is still unbounded.
    if proj
        .items
        .iter()
        .all(|item| is_bounding_item(effective_expr(item)))
    {
        return Ok(());
    }
    Err(safe_mode_violation(
        UNBOUNDED_READ_SHAPE,
        "add `LIMIT <n>`, or return an aggregate such as `count(*)`, so the read has \
         a bounded result",
    ))
}

/// The shape named in every unbounded-read refusal.
const UNBOUNDED_READ_SHAPE: &str =
    "an unbounded read (`MATCH … RETURN` or `MATCH … WITH` without `LIMIT`)";

/// Whether a projected item bounds the result: a reducing aggregate (or
/// `count(*)`) yields one scalar per group, and a constant is a single value
/// per group. Anything else — a bare variable or property — is a grouping
/// key whose distinct values are unbounded.
/// Both callers pass [`effective_expr`] — the paren-transparent item
/// form (LABS-1615/1616) — so `WITH (1) AS c` bounds exactly like
/// `WITH 1 AS c` and no arm re-derives the policy callee-side.
fn is_bounding_item(expr: &Expr) -> bool {
    contains_non_collect_aggregation(expr) || matches!(expr, Expr::Literal(_))
}

/// The projection whose result the statement returns, with the clause list
/// that owns it.
struct ResultProjection<'a> {
    owner: &'a [Clause],
    position: usize,
    limit: &'a Option<Expr>,
    items: &'a [ReturnItem],
    /// True when the projection comes from a trailing `CALL { ... }` subquery
    /// (ADR-0210) rather than the body's own clauses. Its `LIMIT`/aggregates
    /// bound only the subquery's own run — which executes once per outer row —
    /// so they do not bound the whole statement.
    trailing: bool,
}

/// The projection whose result the statement returns: the outer final
/// `RETURN`/`WITH`, or — when the body ends in `CALL { ... }` — the
/// subquery's own final projection (ADR-0210: that `RETURN` is the
/// statement's result).
fn result_projection(clauses: &[Clause]) -> Option<ResultProjection<'_>> {
    if let Some((position, limit, items)) = last_projection(clauses) {
        return Some(ResultProjection {
            owner: clauses,
            position,
            limit,
            items,
            trailing: false,
        });
    }
    let body = trailing_call_body(clauses)?;
    let (position, limit, items) = last_projection(body)?;
    Some(ResultProjection {
        owner: body,
        position,
        limit,
        items,
        trailing: true,
    })
}

/// The clause list of a body's trailing `CALL { ... }` subquery, if the body
/// ends in one (ADR-0210: that subquery's `RETURN` is the statement's
/// result).
fn trailing_call_body(clauses: &[Clause]) -> Option<&[Clause]> {
    match clauses.last() {
        Some(Clause::CallSubquery(cs)) => Some(&cs.statement.clauses),
        _ => None,
    }
}

/// Whether any clause of the body — its own clauses or a trailing
/// `CALL { ... }` subquery's (ADR-0210) — satisfies `pred`. The one
/// body-surface fold behind [`reads_graph`] and
/// [`has_pattern_comprehension_read`].
fn body_any(clauses: &[Clause], pred: impl Fn(&Clause) -> bool) -> bool {
    clauses.iter().any(&pred)
        || trailing_call_body(clauses).is_some_and(|body| body.iter().any(&pred))
}

/// Whether the body performs a graph read: a `MATCH` anywhere, or a pattern
/// comprehension in any expression.
fn reads_graph(clauses: &[Clause]) -> bool {
    body_any(clauses, clause_reads_graph)
}

fn clause_reads_graph(clause: &Clause) -> bool {
    matches!(clause, Clause::Match(_)) || clause_reads_graph_expr(clause)
}

/// Whether a pattern comprehension anywhere in the body performs a graph
/// read (the read the reducing-projection exemption must not mask).
fn has_pattern_comprehension_read(clauses: &[Clause]) -> bool {
    body_any(clauses, clause_reads_graph_expr)
}

fn clause_reads_graph_expr(clause: &Clause) -> bool {
    match clause {
        Clause::Return(ret) => {
            exprs_read_graph(ret.items.iter().map(|item| &item.expr))
                || ret.order_by.iter().any(|o| reads_graph_expr(&o.expr))
                || opt_reads_graph(&ret.skip)
                || opt_reads_graph(&ret.limit)
        }
        Clause::With(with) => {
            exprs_read_graph(with.items.iter().map(|item| &item.expr))
                || opt_reads_graph(&with.where_expr)
                || with.order_by.iter().any(|o| reads_graph_expr(&o.expr))
                || opt_reads_graph(&with.skip)
                || opt_reads_graph(&with.limit)
        }
        Clause::Unwind(uw) => reads_graph_expr(&uw.expr),
        Clause::Foreach(foreach) => reads_graph_expr(&foreach.list_expr),
        Clause::Set(set) => set.items.iter().any(|item| opt_reads_graph(&item.value)),
        Clause::Delete(del) => exprs_read_graph(del.expressions.iter()),
        Clause::Merge(mc) => mc
            .on_create
            .iter()
            .chain(&mc.on_match)
            .any(|item| opt_reads_graph(&item.value)),
        Clause::CallProcedure(cp) => {
            exprs_read_graph(cp.args.iter()) || opt_reads_graph(&cp.where_expr)
        }
        _ => false,
    }
}

/// A pattern comprehension reads the graph wherever it sits in an
/// expression — a projection item, a `WHERE`/`ORDER BY`/`SKIP`/`LIMIT`, an
/// `UNWIND` source, a `SET`/`MERGE` value, or a `CALL` argument.
fn reads_graph_expr(expr: &Expr) -> bool {
    contains_pattern_comprehension(expr)
}

fn exprs_read_graph<'a>(exprs: impl IntoIterator<Item = &'a Expr>) -> bool {
    exprs.into_iter().any(reads_graph_expr)
}

fn opt_reads_graph(expr: &Option<Expr>) -> bool {
    expr.as_ref().is_some_and(reads_graph_expr)
}

/// The last `RETURN`/`WITH` in the body: its position, its `LIMIT`, and its
/// projected items. `None` when the body never projects.
fn last_projection(clauses: &[Clause]) -> Option<(usize, &Option<Expr>, &[ReturnItem])> {
    let mut found = None;
    for (index, clause) in clauses.iter().enumerate() {
        match clause {
            Clause::Return(ret) => found = Some((index, &ret.limit, ret.items.as_slice())),
            Clause::With(with) => found = Some((index, &with.limit, with.items.as_slice())),
            _ => {}
        }
    }
    found
}

/// The clauses whose presence after a projection makes the tail a write.
#[cfg(test)]
mod tests {
    use crate::validate::StatementPolicy;

    // --- LABS-680 (ADR-0477) safe-mode shape rules ---

    fn safe(policy: StatementPolicy) -> StatementPolicy {
        StatementPolicy {
            safe_mode: true,
            ..policy
        }
    }

    fn parse_and_lower_safe(query: &str) -> Result<(), String> {
        crate::parse_and_lower_statement_with_policy(
            query,
            relate_core::PreviewFeatures::none(),
            safe(StatementPolicy::default()),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    #[test]
    fn safe_mode_refuses_a_new_named_node_without_a_label() {
        for query in ["CREATE (n)", "MERGE (n) RETURN 1"] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("without a label"), "{query}: {error}");
            assert!(
                error.contains("This is a policy, not a database limit"),
                "{query}: {error}"
            );
        }
        // The relationship-only anonymous form the TCK uses is exempt.
        for query in ["CREATE ()-[r:X]->()", "MATCH (a) CREATE (a)-[r:X]->()"] {
            parse_and_lower_safe(query).unwrap_or_else(|e| panic!("{query} must be admitted: {e}"));
        }
        // A labeled new node is the remedy and is admitted.
        parse_and_lower_safe("CREATE (n:Person)").expect("a labeled new node is admitted");
    }

    /// The binding set is the LIVE scope, not an additive union: a `WITH`
    /// drops the old binding, so `CREATE (n)` afterwards creates a new
    /// label-less node and must be refused.
    #[test]
    fn safe_mode_refuses_a_label_less_node_after_with_dropped_the_binding() {
        for query in [
            "MATCH (n) WITH 1 AS x CREATE (n)",
            "MATCH (n) WITH n AS m CREATE (n)",
        ] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("without a label"), "{query}: {error}");
        }
        // `WITH *` keeps the binding, so a bound endpoint stays exempt.
        parse_and_lower_safe("MATCH (a) WITH * CREATE (a)-[:X]->(:B)")
            .expect("WITH * keeps the binding");
    }

    /// A `WITH` does not make a may-be-null `OPTIONAL MATCH` binding (or an
    /// `UNWIND` alias) node-safe: the executor creates a new node for a
    /// null/non-node binding, so the label-less CREATE is still refused.
    #[test]
    fn safe_mode_refuses_a_label_less_node_over_a_reprojected_non_node_binding() {
        for query in [
            "OPTIONAL MATCH (o) WITH o CREATE (o)-[:X]->()",
            "UNWIND [1, 2] AS x CREATE (x)-[:X]->()",
            // A relationship variable is not a node.
            "MATCH ()-[r:X]->() CREATE (r)-[:Y]->()",
        ] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("without a label"), "{query}: {error}");
        }
    }

    /// An inline subquery binds the names its final projection returns, so a
    /// CREATE over such a name is exempt like any other bound endpoint.
    #[test]
    fn safe_mode_exempts_a_name_bound_by_an_inline_subquery() {
        parse_and_lower_safe(
            "CALL { MATCH (n:Person) RETURN n AS m LIMIT 5 } CREATE (m)-[:X]->(:B)",
        )
        .expect("a name bound by the subquery's RETURN is in scope");
        // An `OPTIONAL CALL` export may be null, so it stays unbound.
        let error = parse_and_lower_safe(
            "MATCH (a) OPTIONAL CALL (a) { RETURN a AS m LIMIT 5 } CREATE (m)-[:X]->(:B)",
        )
        .unwrap_err();
        assert!(error.contains("SafeModeViolation"), "{error}");
        assert!(error.contains("without a label"), "{error}");
        // A non-node export is not node-safe either.
        let error = parse_and_lower_safe("CALL { RETURN 1 AS m } CREATE (m)-[:X]->()").unwrap_err();
        assert!(error.contains("SafeModeViolation"), "{error}");
        assert!(error.contains("without a label"), "{error}");
    }

    /// An `OPTIONAL MATCH` binding may be null, so it does not prove the
    /// variable names an existing node; a label-less CREATE over it is
    /// refused.
    #[test]
    fn safe_mode_refuses_a_label_less_node_over_an_optional_match_binding() {
        let error = parse_and_lower_safe("OPTIONAL MATCH (n) CREATE (n)").unwrap_err();
        assert!(error.contains("SafeModeViolation"), "{error}");
        assert!(error.contains("without a label"), "{error}");
    }

    #[test]
    fn safe_mode_refuses_an_unbounded_read() {
        for query in [
            "MATCH (n) RETURN n",
            "MATCH (n) WITH n RETURN n",
            "MATCH (n) RETURN collect(n)",
            "MATCH (n) RETURN collect(n) AS ns",
            // An earlier projection does not bound a later MATCH.
            "WITH 1 AS x MATCH (n) RETURN n",
            "MATCH (n) WITH count(n) AS c MATCH (m) RETURN m",
        ] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("unbounded read"), "{query}: {error}");
            assert!(error.contains("LIMIT"), "{query}: {error}");
        }
        // collect() is named as not counting toward the exemption.
        let error = parse_and_lower_safe("MATCH (n) RETURN collect(n)").unwrap_err();
        assert!(error.contains("collect()"), "{error}");
        // Legitimate bounded forms pass.
        for query in [
            "MATCH (n) RETURN n LIMIT 10",
            "MATCH (n) RETURN count(n)",
            "MATCH (n) RETURN count(n), sum(n.x)",
            // An earlier reducing projection already bounds the stream.
            "MATCH (n) WITH count(n) AS c RETURN c",
            // A LIMIT on the immediately-preceding projection bounds it too.
            "MATCH (n) WITH n LIMIT 5 RETURN n",
            "MATCH (n) SET n.seen = true",
            "MATCH (n) DELETE n",
            "MATCH (n) WITH n SET n.seen = true",
            "MATCH (n) WITH n LIMIT 5 RETURN n LIMIT 5",
            "WITH 1 AS n RETURN n",
            "RETURN 1",
            "SHOW INDEXES",
        ] {
            parse_and_lower_safe(query).unwrap_or_else(|e| panic!("{query} must be admitted: {e}"));
        }
    }

    /// A pattern comprehension in the projection is a whole-graph read with
    /// no `MATCH` clause; safe mode bounds it like any other read.
    #[test]
    fn safe_mode_refuses_a_bare_pattern_comprehension_read() {
        for query in [
            "RETURN [(:Person)-[:KNOWS]->(m) | m] AS ms",
            "RETURN size([(:Person)-[:KNOWS]->(m) | m])",
            // The comprehension in an intermediate projection fans out too.
            "WITH [(:Person)-[:KNOWS]->(m) | m] AS ms RETURN ms",
            // ... as does one in an UNWIND source.
            "UNWIND [(:Person)-[:KNOWS]->(m) | m] AS ms RETURN ms",
            // ... or in a WHERE/ORDER BY expression.
            "WITH 1 AS x WHERE size([(:Person)-[:KNOWS]->(m) | m]) > 0 RETURN x",
        ] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("unbounded read"), "{query}: {error}");
        }
        // A LIMIT bounds it, like any other read.
        parse_and_lower_safe("RETURN [(:Person)-[:KNOWS]->(m) | m] AS ms LIMIT 5")
            .expect("a LIMIT bounds the read");
    }

    /// A statement ending in `CALL { ... }` takes its result from the
    /// subquery's `RETURN` (ADR-0210); with a `MATCH` in the outer body and
    /// no `LIMIT`, it is still an unbounded read.
    #[test]
    fn safe_mode_refuses_an_unbounded_read_through_a_trailing_subquery() {
        let error = parse_and_lower_safe("MATCH (n) CALL { WITH n RETURN n }").unwrap_err();
        assert!(error.contains("SafeModeViolation"), "{error}");
        assert!(error.contains("unbounded read"), "{error}");
        // A LIMIT inside the subquery does NOT bound it: the subquery runs
        // once per outer row.
        let error =
            parse_and_lower_safe("MATCH (n) CALL (n) { RETURN n AS m LIMIT 5 }").unwrap_err();
        assert!(error.contains("SafeModeViolation"), "{error}");
        assert!(error.contains("unbounded read"), "{error}");
    }

    /// The collect() detection reaches into list literals, CASE arms and
    /// comprehensions: a hidden collect() still buffers every row, so a
    /// sibling reducing aggregate must not exempt it.
    #[test]
    fn safe_mode_refuses_a_collect_nested_in_an_expression() {
        for query in [
            "MATCH (n) RETURN count(n), [x IN collect(n) | x]",
            "MATCH (n) RETURN count(n), size(collect(n))",
            "MATCH (n) RETURN count(n), CASE WHEN true THEN collect(n) ELSE [] END",
        ] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("collect()"), "{query}: {error}");
        }
    }

    /// A grouping key beside a reducing aggregate is still one row per
    /// distinct key — unbounded.
    #[test]
    fn safe_mode_refuses_a_grouping_key_beside_a_reducing_aggregate() {
        for query in [
            "MATCH (n) RETURN n, count(*)",
            "MATCH (n) RETURN n.x, count(*)",
            "MATCH (n) RETURN n, sum(n.x)",
        ] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("unbounded read"), "{query}: {error}");
        }
        // All-aggregate (or constant) projections reduce to one row per group.
        for query in [
            "MATCH (n) RETURN count(n), sum(n.x)",
            "MATCH (n) RETURN count(*) AS c, 1 AS one",
        ] {
            parse_and_lower_safe(query).unwrap_or_else(|e| panic!("{query} must be admitted: {e}"));
        }
    }

    /// The aggregate exemption is per item: a `collect()` beside a reducing
    /// aggregate still returns the whole group and is refused.
    #[test]
    fn safe_mode_refuses_collect_beside_a_reducing_aggregate() {
        for query in [
            "MATCH (n) RETURN count(n), collect(n)",
            "MATCH (n) RETURN collect(n), count(*)",
            "MATCH (n) RETURN sum(n.x), collect(n.x) AS xs",
        ] {
            let error = parse_and_lower_safe(query).unwrap_err();
            assert!(error.contains("SafeModeViolation"), "{query}: {error}");
            assert!(error.contains("collect()"), "{query}: {error}");
        }
        for query in [
            "MATCH (n) RETURN count(n)",
            "MATCH (n) RETURN count(n), sum(n.x)",
        ] {
            parse_and_lower_safe(query).unwrap_or_else(|e| panic!("{query} must pass: {e}"));
        }
    }

    #[test]
    fn safe_mode_off_admits_every_shape() {
        for query in [
            "CREATE (n)",
            "MATCH (n) RETURN n",
            "MATCH (n) RETURN collect(n)",
        ] {
            crate::parse_and_lower(query)
                .unwrap_or_else(|e| panic!("{query} must be admitted without safe mode: {e}"));
        }
    }
}
