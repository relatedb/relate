//! The variable-scope detectors (ADR-0363 decision 3): a `CALL { … }` body
//! variable shadowing an outer name, and a `SEARCH` index identifier
//! sharing a name with a variable in scope. Split from [`super::shape`]
//! (the clause-local shape detectors) because these walk the clause tree
//! with a carried scope frame.

use std::collections::BTreeSet;

use relate_ir::{
    walk_pattern_payloads, CallScope, Clause, Expr, PathPattern, PatternPayload, Query, SearchSpec,
};
use relate_model::wire::{Notification, NotificationKind};

use super::shape::ShapeCtx;

/// The variable-scope facts that need the clause tree: a `CALL { … }` body
/// variable shadowing an outer name (03N60), and a `SEARCH` index identifier
/// sharing a name with a variable in scope (03N63).
///
/// One structural recursion over the tree carries the scope it is in: each
/// clause list (a statement part, a `CALL { … }` arm, a `WHEN`/`ELSE`
/// branch, a `FOREACH` body) starts from the frame its enclosing position
/// supplies and grows one clause at a time by [`introduced_names`], so a
/// subquery body is analysed against the names actually visible at its CALL
/// clause. Imported names — a scope clause's or a scope-less body's legacy
/// leading `WITH` — are not shadowing.
///
/// An expression subquery (`EXISTS`/`COUNT`/`COLLECT`) lives in an
/// expression, not in the clause tree, so the recursion above does not reach
/// it. Its body still opens a scope of its own: a `CALL { … }` inside it is
/// checked against the names the body binds before it, with an **empty**
/// starting frame — a name bound only outside the expression subquery is not
/// part of that body's "outer query" (verified against Neo4j 2026.10.1: the
/// same `CALL` shape shadowing an enclosing-statement name raises nothing).
/// Every such body is therefore analysed with a fresh frame, wherever it
/// sits, including inside another expression subquery's body.
pub(super) fn observe_subquery_scopes(ctx: &mut ShapeCtx<'_>, parts: &[&[Clause]]) {
    let (notifications, meter) = (&mut *ctx.notifications, &mut *ctx.meter);
    // The per-subquery work scales with the statement-wide name set (a
    // wildcard import clones it): declined past the shared budget, like the
    // sibling detectors. The SEARCH family stays checked — its cost is per
    // index-specification name, not per scope.
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut subqueries = 0usize;
    let mut expression_bodies: Vec<&Query> = Vec::new();
    for part in parts {
        relate_ir::walk_clauses(
            part,
            &mut |clause| {
                if matches!(clause, Clause::CallSubquery { .. }) {
                    subqueries += 1;
                }
                declared.extend(introduced_names(clause));
            },
            &mut |expr| {
                if let Some(query) = expr.subquery_body() {
                    expression_bodies.push(query);
                }
            },
        );
    }
    subqueries += expression_bodies.len();
    let subquery_scan = meter.charge_pairs(subqueries, declared.len());
    // The one visit both checks share: the SEARCH shadow test always, the
    // `CALL { … }` body test only while the subquery scan is affordable.
    let mut visit = |clause: &Clause, scope: &Scope, own: &BTreeSet<String>| {
        observe_search_shadow(notifications, clause, scope, own);
        if let Clause::CallSubquery {
            query,
            scope: call_scope,
            ..
        } = clause
        {
            if subquery_scan {
                observe_call_body(notifications, query, call_scope, scope);
            }
        }
    };
    // A UNION arm and a `NEXT` stage are independent statements: each is its
    // own clause list, so neither sees the other's names.
    for part in parts {
        walk_scope_frame(part, &Scope::new(), &mut visit);
    }
    // `walk_query_frames` descends into clause-level bodies only, so each
    // expression body is analysed exactly once here; a body nested in
    // another expression body is its own entry in the list.
    for query in expression_bodies {
        walk_query_frames(query, &Scope::new(), &mut visit);
    }
}

/// The names in force at one point of the clause tree.
type Scope = BTreeSet<String>;

/// One structural recursion over a clause list, carrying the frame the list
/// runs in, and calling `visit` once per clause with the frame before the
/// clause and the names the clause introduces.
///
/// `outer` is the frame the enclosing position supplies (empty for a
/// top-level part, the frame at the `CALL { … }` clause for its body, plus
/// the loop variable for a `FOREACH` body). A clause sees the frame its
/// predecessors built; its own names join it for the clauses after it.
fn walk_scope_frame(
    clauses: &[Clause],
    outer: &Scope,
    visit: &mut impl FnMut(&Clause, &Scope, &BTreeSet<String>),
) {
    let mut scope = outer.clone();
    for clause in clauses {
        let own = introduced_names(clause);
        visit(clause, &scope, &own);
        walk_clause_bodies(clause, &scope, visit);
        scope.extend(own);
    }
}

/// The `SEARCH` shadow test against the frame before the clause (plus the
/// clause's own pattern variables, which its index specification sees).
fn observe_search_shadow(
    notifications: &mut Vec<Notification>,
    clause: &Clause,
    scope: &Scope,
    own: &BTreeSet<String>,
) {
    if let Clause::Match {
        search: Some(search),
        ..
    } = clause
    {
        for name in search_index_names(&search.spec) {
            if scope.contains(name) || own.contains(name) {
                notifications.push(Notification::new(
                    NotificationKind::SearchIdentifierShadowingVariable,
                    format!(
                        "The identifier `{name}` in the index specification clause has \
                         the same name as a variable in scope."
                    ),
                ));
            }
        }
    }
}

/// The descent into whatever clause lists a clause carries.
fn walk_clause_bodies(
    clause: &Clause,
    scope: &Scope,
    visit: &mut impl FnMut(&Clause, &Scope, &BTreeSet<String>),
) {
    match clause {
        // The body runs in the frame at the CALL clause, whichever arm.
        Clause::CallSubquery { query, .. } => walk_query_frames(query, scope, visit),
        Clause::Conditional {
            branches,
            else_query,
        } => {
            for (_, query) in branches {
                walk_query_frames(query, scope, visit);
            }
            if let Some(query) = else_query.as_deref() {
                walk_query_frames(query, scope, visit);
            }
        }
        Clause::Foreach {
            variable, clauses, ..
        } => {
            let mut inner = scope.clone();
            inner.insert(variable.to_string());
            walk_scope_frame(clauses, &inner, visit);
        }
        _ => {}
    }
}

/// Every part of a nested [`Query`], each against the same `outer` frame:
/// UNION arms and `NEXT` stages do not see each other's names.
fn walk_query_frames(
    query: &Query,
    outer: &Scope,
    visit: &mut impl FnMut(&Clause, &Scope, &BTreeSet<String>),
) {
    for part in &query.parts {
        walk_scope_frame(&part.clauses, outer, visit);
    }
    if let Some(next) = query.next.as_deref() {
        walk_query_frames(next, outer, visit);
    }
}

/// The 03N60 test for one `CALL { … }` body: per UNION arm, the names the
/// arm introduces and does not import, kept to those the outer frame holds.
///
/// Each arm introduces its own names and imports its own, so the test runs
/// per arm: a name arm A declares is a shadow even when arm B imports a name
/// with the same spelling. One notification per spelling, even when several
/// arms shadow it.
fn observe_call_body(
    notifications: &mut Vec<Notification>,
    query: &Query,
    call_scope: &CallScope,
    outer: &Scope,
) {
    let own_by_arm: Vec<BTreeSet<String>> = query
        .parts
        .iter()
        .map(|part| part.clauses.iter().flat_map(introduced_names).collect())
        .collect();
    let imports_by_arm = call_imports(call_scope, query, outer);
    let shadowing: BTreeSet<&String> = own_by_arm
        .iter()
        .zip(imports_by_arm.iter())
        .flat_map(|(own, imports)| own.difference(imports))
        .filter(|name| outer.contains(name.as_str()))
        .collect();
    for name in shadowing {
        notifications.push(Notification::new(
            NotificationKind::SubqueryVariableShadowing,
            format!(
                "The variable `{name}` in the subquery uses the same name as a \
                 variable in the outer scope; import the outer one with \
                 `CALL ({name}) {{ … }}` or rename the inner one."
            ),
        ));
    }
}

/// The names a clause introduces into the scope after it (the IR analog of
/// the front's `clause_introduced_names`): pattern variables for
/// MATCH/CREATE/MERGE, a WITH item's output name, and the alias of
/// UNWIND/LOAD CSV/FOREACH or a procedure CALL's YIELD. `*` is not a name.
fn introduced_names(clause: &Clause) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    match clause {
        Clause::Match {
            patterns, search, ..
        } => {
            for pattern in patterns {
                collect_pattern_names(pattern, &mut names);
            }
            if let Some(score) = search.as_ref().and_then(|search| search.score_var.as_ref()) {
                names.insert(score.to_string());
            }
        }
        Clause::Create { patterns } => {
            for pattern in patterns {
                collect_pattern_names(pattern, &mut names);
            }
        }
        Clause::Merge { pattern, .. } => collect_pattern_names(pattern, &mut names),
        Clause::Unwind { alias, .. } => {
            names.insert(alias.to_string());
        }
        Clause::LoadCsv { variable, .. } => {
            names.insert(variable.to_string());
        }
        Clause::Call { yield_items, .. } => {
            names.extend(yield_items.effective_names().map(|name| name.to_string()));
        }
        // A `RETURN` is terminal and exports columns rather than binding
        // names in a scope later clauses (or an outer scope) can shadow; a
        // `WITH` replaces the scope, so its outputs count.
        Clause::With { items, .. } => {
            for (index, item) in items.iter().enumerate() {
                if !matches!(item.expr, Expr::Star) {
                    names.insert(item.output_name(index));
                }
            }
        }
        Clause::Foreach { variable, .. } => {
            names.insert(variable.to_string());
        }
        _ => {}
    }
    names
}

/// Every variable a path pattern binds: its path names, and every node and
/// relationship variable including quantified groups.
fn collect_pattern_names(pattern: &PathPattern, out: &mut BTreeSet<String>) {
    // Text render: the collected scope names are the String sink.
    out.extend(pattern.path_names().map(|name| name.to_string()));
    walk_pattern_payloads(pattern, &mut |payload| {
        let variable = match payload {
            PatternPayload::Node(node) => &node.variable,
            PatternPayload::Relationship(rel) => &rel.variable,
            PatternPayload::Quantified(_) => return,
        };
        if let Some(variable) = variable {
            out.insert(variable.to_string());
        }
    });
}

/// The names each UNION arm of a `CALL { … }` body imports from its outer
/// scope, in arm order.
///
/// A scope clause declares one import set for the whole body (`*` imports
/// everything in scope), so every arm reports it. A scope-less body imports
/// per arm from that arm's own legacy leading `WITH`
/// ([`relate_ir::SingleQuery::leading_imports`] — the same derivation
/// the planner's 42I28 rule consults): one arm's import must never suppress
/// a sibling arm's shadowing diagnostic.
pub(super) fn call_imports(
    scope: &CallScope,
    query: &Query,
    outer: &BTreeSet<String>,
) -> Vec<BTreeSet<String>> {
    // Resolve the body-wide import set once: a scope clause declares one
    // for every arm; a scope-less body imports per arm.
    let shared: Option<BTreeSet<String>> = match scope {
        CallScope::Wildcard => Some(outer.clone()),
        // Text render: the import sets are the String scope domain.
        CallScope::Vars(names) => Some(names.iter().map(|name| name.to_string()).collect()),
        CallScope::None => None,
    };
    query
        .parts
        .iter()
        .map(|part| match &shared {
            Some(imports) => imports.clone(),
            None => part.leading_imports(outer),
        })
        .collect()
}

/// The index names a `SEARCH` source specification names.
fn search_index_names(spec: &SearchSpec) -> Vec<&str> {
    match spec {
        SearchSpec::Index(index) => vec![index.index_name.as_str()],
        SearchSpec::Combine(combine) => combine
            .sources
            .iter()
            .flat_map(|source| search_index_names(&source.spec))
            .collect(),
    }
}
