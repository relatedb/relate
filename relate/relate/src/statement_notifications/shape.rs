//! Clause-shape notification detectors (ADR-0363 decisions 3/5): the
//! deprecated forms whose facts live in the statement's clause tree rather
//! than in a catalog lookup or a plan operator.
//!
//! The projection-subclause deprecations (ambiguous and complex references
//! in `ORDER BY`/`WHERE`), the `CREATE` self-reference, the repeated
//! relationship variable, the unsatisfiable relationship type expression,
//! the subquery and `SEARCH` shadowing facts, and the exhaustive
//! shortest-path predicate are all raised here. [`super::Facts`] owns the
//! accumulator and calls into this module; each detector appends to the
//! notification list it is handed.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use relate_ir::{
    expr_vars, is_agg_call, is_agg_expr, walk_pattern_payloads, CallScope, Clause, Expr, LabelExpr,
    OrderItem, PathPattern, PatternPayload, Projection, Query, SearchSpec,
};
use relate_model::wire::{Notification, NotificationKind};

use super::collect_label_names;
use crate::plan_node::patterns_share_an_anchor;

/// The statement-shape checks' work budget, in pair comparisons.
///
/// Three detector families cost the product of two attacker-widenable
/// dimensions: the type-expression satisfiability check (names x expression
/// nodes), the complex-reference check (nodes x projection items), and the
/// scans over created names, shadowing aliases and subquery name sets. The
/// parser caps nesting depth, not width, and these tight loops never
/// observe the per-query cancellation token, so a crafted `EXPLAIN`
/// statement within the transport's statement cap would otherwise spin for
/// hours.
///
/// The policy, stated once: every detector here raises an advisory
/// deprecation hint only, never a correctness notification, so silence is
/// the conservative direction. A detector that cannot afford its scan
/// declines it and emits nothing. The bound sits above realistic statements
/// (a 2000-item projection ordered by a 2000-element list is ~8M) and well
/// below the pathological widths the statement cap admits (~10^12).
const SHAPE_CHECK_WORK_BUDGET: u64 = 16 * 1024 * 1024;

/// The one work meter of a statement's shape pass. It is threaded through
/// every detector that scans a product of two statement dimensions, so the
/// bound is a property of the pass: the budget is shared across detectors
/// and a new detector must take the meter to scan at all.
pub(super) struct WorkMeter {
    remaining: u64,
}

impl Default for WorkMeter {
    fn default() -> Self {
        Self {
            remaining: SHAPE_CHECK_WORK_BUDGET,
        }
    }
}

impl WorkMeter {
    /// Spend `units` pair comparisons, or return `false` (spending
    /// nothing) when the pass cannot afford them and the detector must
    /// decline its scan silently.
    fn charge(&mut self, units: u64) -> bool {
        if units > self.remaining {
            return false;
        }
        self.remaining -= units;
        true
    }

    /// [`Self::charge`] for the product of two statement dimensions, the
    /// one shape every detector spends in.
    fn charge_pairs(&mut self, a: u64, b: usize) -> bool {
        self.charge(a.saturating_mul(u64::try_from(b).unwrap_or(u64::MAX)))
    }
}

/// The `MATCH`-shape notifications: a nested multi-pattern cartesian (the
/// top-level form is the operator-scoped warning), repeated relationship
/// variables, unsatisfiable relationship type expressions, and the
/// exhaustive-shortest-path predicate.
pub(super) fn observe_match(
    notifications: &mut Vec<Notification>,
    meter: &mut WorkMeter,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    nested: bool,
) {
    if nested && patterns.len() > 1 && !patterns_share_an_anchor(patterns) {
        notifications.push(Notification::new(
            NotificationKind::CartesianProduct,
            "The MATCH patterns share no join anchor.",
        ));
    }
    let mut repeated: BTreeMap<String, usize> = BTreeMap::new();
    for pattern in patterns {
        walk_pattern_payloads(pattern, &mut |payload| {
            let PatternPayload::Relationship(rel) = payload else {
                return;
            };
            if let Some(variable) = &rel.variable {
                // Text render: the repeated-rel counter is the String
                // notification domain.
                *repeated.entry(variable.to_string()).or_default() += 1;
            }
            if let Some(expression) = &rel.type_expr {
                if type_expression_unsatisfiable(expression, meter) {
                    notifications.push(Notification::new(
                        NotificationKind::UnsatisfiableRelationshipTypeExpression,
                        "The relationship type expression cannot be satisfied because \
                         relationships have exactly one type.",
                    ));
                }
            }
        });
    }
    for (variable, count) in repeated {
        if count > 1 {
            notifications.push(Notification::new(
                NotificationKind::RepeatedRelationshipVariable,
                format!(
                    "The relationship variable `{variable}` is repeated in the same \
                     MATCH, which leads to no results."
                ),
            ));
        }
    }
    if let Some(where_clause) = where_clause {
        // One walk of the predicate, then a constant-time check per
        // pattern: the detector used to re-walk the whole `WHERE` for
        // every shortest-path pattern in the MATCH.
        let forcing = ExhaustivePredicateVars::collect(where_clause);
        if patterns.iter().any(|pattern| forcing.matches(pattern)) {
            notifications.push(Notification::new(
                NotificationKind::ExhaustiveShortestPath,
                "The query runs with exhaustive shortest path due to an existential \
                 predicate; separate the predicate from the MATCH with WITH where possible.",
            ));
        }
    }
}

/// The variables a `WHERE` clause names in the two positions that force an
/// exhaustive shortest-path evaluation: the list of a quantifier over the
/// relationship variable, and the first argument of a `length(…)` call over
/// the path variable. Collected in one walk so each pattern's check is a
/// pair of set lookups rather than a re-walk of the predicate. The names are
/// owned because `Expr::visit` hands the callback borrows that cannot escape
/// it; only the two forcing positions are copied.
#[derive(Default)]
struct ExhaustivePredicateVars {
    quantifier_lists: HashSet<String>,
    length_arguments: HashSet<String>,
}

impl ExhaustivePredicateVars {
    fn collect(expr: &Expr) -> Self {
        let mut facts = Self::default();
        expr.visit(&mut |node| match node {
            Expr::Quantifier { list, .. } => {
                if let Expr::Variable(variable) = list.as_ref() {
                    // Text render: the fact sets are the String sink the
                    // pattern probes read.
                    facts.quantifier_lists.insert(variable.to_string());
                }
            }
            Expr::Function { name, args, .. }
                if name.with_str(|n| n.eq_ignore_ascii_case("length")) =>
            {
                if let Some(Expr::Variable(variable)) = args.first() {
                    facts.length_arguments.insert(variable.to_string());
                }
            }
            _ => {}
        });
        facts
    }

    /// Whether this pattern's predicates force the exhaustive evaluation: a
    /// quantifier over its relationship variable, or a `length(…)` over one
    /// of its path names. Only a `shortestPath` pattern can be forced.
    fn matches(&self, pattern: &PathPattern) -> bool {
        if !pattern.is_shortest_path_function {
            return false;
        }
        pattern
            .rels
            .iter()
            .filter_map(|rel| rel.variable.as_ref())
            .any(|rel| rel.with_str(|text| self.quantifier_lists.contains(text)))
            || pattern
                .path_names()
                .any(|path| path.with_str(|text| self.length_arguments.contains(text)))
    }
}

/// The `CREATE` self-reference (Create.feature): a property definition of
/// any entity created in this clause reading another (or the same) entity
/// created in the same clause. One notification per (referencing property
/// expression, referenced entity) pair.
pub(super) fn observe_create(
    notifications: &mut Vec<Notification>,
    meter: &mut WorkMeter,
    patterns: &[PathPattern],
) {
    // One post-rewrite walk collects both facts the scan needs: the created
    // names and the property maps that may read them.
    let mut created: BTreeSet<String> = BTreeSet::new();
    let mut property_maps: Vec<&[(_, Expr)]> = Vec::new();
    for pattern in patterns {
        walk_pattern_payloads(pattern, &mut |payload| {
            let (variable, properties) = match payload {
                PatternPayload::Node(node) => (&node.variable, &node.properties),
                PatternPayload::Relationship(rel) => (&rel.variable, &rel.properties),
                PatternPayload::Quantified(_) => return,
            };
            // Text render: the created-name set is the String sink the
            // property expressions' variable reads probe.
            if let Some(variable) = variable {
                created.insert(variable.to_string());
            }
            if let Some(Expr::Map(items)) = properties {
                property_maps.push(items);
            }
        });
    }
    if created.is_empty() {
        return;
    }
    // The (property value, created name) pairs the scan below tests: declined
    // past the shared budget, like the sibling detectors, so a crafted `CREATE`
    // with a wide property map and many created entities cannot pin a core
    // before execution.
    let values: u64 = property_maps.iter().map(|items| items.len() as u64).sum();
    if !meter.charge_pairs(values, created.len()) {
        return;
    }
    for items in property_maps {
        for (_, value) in items {
            let mut vars = HashSet::new();
            expr_vars(value, &mut vars);
            for name in &created {
                if vars.contains(name) {
                    notifications.push(Notification::new(
                        NotificationKind::CreateSelfReference,
                        format!(
                            "Creating an entity (`{name}`) and referencing that entity \
                             in a property definition in the same CREATE is deprecated."
                        ),
                    ));
                }
            }
        }
    }
}

/// The projection-subclause deprecations (ProjectionSubclauseAcceptance,
/// GroupByAcceptance):
///
/// - an `ORDER BY`/`WHERE` expression referencing a variable that a
///   projection alias re-binds is an *ambiguous* reference (01N01);
/// - an `ORDER BY`/`WHERE` expression containing a complex projection item
///   expression as a proper sub-expression is a *complex* reference (01N01).
///
/// A bare variable naming a projection alias is the alias itself, not an
/// ambiguous reference; an aggregation is a recognized sort key, not a
/// complex projection item. The complex-reference deprecation only applies
/// under implicit grouping or `DISTINCT`, where the subclause cannot see
/// the pre-projection scope.
pub(super) fn observe_projection(
    notifications: &mut Vec<Notification>,
    meter: &mut WorkMeter,
    items: &[Projection],
    order: &[OrderItem],
    where_clause: Option<&Expr>,
    distinct: bool,
) {
    if items.is_empty() {
        return;
    }
    let grouped = distinct || items.iter().any(|item| is_agg_expr(&item.expr));
    let mut item_vars = HashSet::new();
    for item in items {
        expr_vars(&item.expr, &mut item_vars);
    }
    // The alias sets render once (the String sink the expr_vars probes
    // read); a bare alias reference compares by text.
    let aliases: HashSet<String> = items
        .iter()
        .filter_map(|item| item.alias.as_ref().map(|a| a.to_string()))
        .collect();
    let shadowing: BTreeSet<String> = items
        .iter()
        .filter_map(|item| {
            let alias = item.alias.as_ref()?;
            let alias_text = alias.to_string();
            if matches!(&item.expr, Expr::Variable(variable) if variable == alias) {
                return None;
            }
            item_vars
                .contains(alias_text.as_str())
                .then_some(alias_text)
        })
        .collect();
    // The complex projection items, selected once. The per-pair loop below
    // used to re-run the shape test for every (subclause expression, item)
    // pair, so a large projection paid for the same answer once per pair.
    let complex: Vec<(usize, &Projection)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| grouped && is_complex_projection_item(&item.expr))
        .collect();
    // The (subclause expression, shadowing name) pairs the ambiguous-reference
    // scan tests: declined past the shared budget, while the complex-reference
    // pass below keeps its own guard. One wide dimension therefore never
    // silences the other family.
    let shadowing_scan = meter.charge_pairs(
        order.len() as u64 + u64::from(where_clause.is_some()),
        shadowing.len(),
    );
    for expr in order.iter().map(|item| &item.expr).chain(where_clause) {
        let bare_alias = matches!(expr, Expr::Variable(variable)
                if variable.with_str(|text| aliases.contains(text)));
        let mut vars = HashSet::new();
        expr_vars(expr, &mut vars);
        if !bare_alias && shadowing_scan {
            for name in &shadowing {
                if vars.contains(name.as_str()) {
                    notifications.push(Notification::new(
                        NotificationKind::AmbiguousSubclauseReference,
                        format!(
                            "The subclause expression contains an ambiguous reference to \
                             variable `{name}`; reference the projection item by an alias \
                             or use the GROUP BY clause."
                        ),
                    ));
                }
            }
        }
    }
    if complex.is_empty() {
        return;
    }
    for expr in order.iter().map(|item| &item.expr).chain(where_clause) {
        for (index, item) in complex_references(meter, expr, &complex) {
            let text = item
                .source_text
                .clone()
                .unwrap_or_else(|| item.output_name(index));
            notifications.push(Notification::new(
                NotificationKind::ComplexSubclauseReference,
                format!(
                    "The subclause expression contains a reference to the complex \
                     projection item expression `{text}`; reference it by an alias."
                ),
            ));
        }
    }
}

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
pub(super) fn observe_subquery_scopes(
    notifications: &mut Vec<Notification>,
    meter: &mut WorkMeter,
    parts: &[&[Clause]],
) {
    // The per-subquery work scales with the statement-wide name set (a
    // wildcard import clones it): declined past the shared budget, like the
    // sibling detectors. The SEARCH family stays checked — its cost is per
    // index-specification name, not per scope.
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut subqueries = 0u64;
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
    subqueries += expression_bodies.len() as u64;
    let subquery_scan = meter.charge_pairs(subqueries, declared.len());
    // A UNION arm and a `NEXT` stage are independent statements: each is its
    // own clause list, so neither sees the other's names.
    for part in parts {
        observe_scope_frame(notifications, part, &Scope::new(), subquery_scan);
    }
    // `observe_query_frames` descends into clause-level bodies only, so each
    // expression body is analysed exactly once here; a body nested in
    // another expression body is its own entry in the list.
    for query in expression_bodies {
        observe_query_frames(notifications, query, &Scope::new(), subquery_scan);
    }
}

/// The names in force at one point of the clause tree.
type Scope = BTreeSet<String>;

/// One structural recursion over a clause list, carrying the frame the list
/// runs in.
///
/// `outer` is the frame the enclosing position supplies (empty for a
/// top-level part, the frame at the `CALL { … }` clause for its body, plus
/// the loop variable for a `FOREACH` body). A clause sees the frame its
/// predecessors built; its own names join it for the clauses after it.
fn observe_scope_frame(
    notifications: &mut Vec<Notification>,
    clauses: &[Clause],
    outer: &Scope,
    subquery_scan: bool,
) {
    let mut scope = outer.clone();
    for clause in clauses {
        let own = introduced_names(clause);
        observe_scope_clause(notifications, clause, &scope, &own, subquery_scan);
        scope.extend(own);
    }
}

/// The per-clause half of [`observe_scope_frame`]: the `SEARCH` shadow test
/// against the frame before the clause (plus the clause's own pattern
/// variables, which its index specification sees), and the descent into
/// whatever clause lists the clause carries.
fn observe_scope_clause(
    notifications: &mut Vec<Notification>,
    clause: &Clause,
    scope: &Scope,
    own: &BTreeSet<String>,
    subquery_scan: bool,
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
    match clause {
        Clause::CallSubquery {
            query,
            scope: call_scope,
            ..
        } => {
            if subquery_scan {
                observe_call_body(notifications, query, call_scope, scope);
            }
            // The body runs in the frame at the CALL clause, whichever arm.
            observe_query_frames(notifications, query, scope, subquery_scan);
        }
        Clause::Conditional {
            branches,
            else_query,
        } => {
            for (_, query) in branches {
                observe_query_frames(notifications, query, scope, subquery_scan);
            }
            if let Some(query) = else_query.as_deref() {
                observe_query_frames(notifications, query, scope, subquery_scan);
            }
        }
        Clause::Foreach {
            variable, clauses, ..
        } => {
            let mut inner = scope.clone();
            inner.insert(variable.to_string());
            observe_scope_frame(notifications, clauses, &inner, subquery_scan);
        }
        _ => {}
    }
}

/// Every part of a nested [`Query`], each against the same `outer` frame:
/// UNION arms and `NEXT` stages do not see each other's names.
fn observe_query_frames(
    notifications: &mut Vec<Notification>,
    query: &Query,
    outer: &Scope,
    subquery_scan: bool,
) {
    for part in &query.parts {
        observe_scope_frame(notifications, &part.clauses, outer, subquery_scan);
    }
    if let Some(next) = query.next.as_deref() {
        observe_query_frames(notifications, next, outer, subquery_scan);
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
    walk_pattern_payloads(pattern, &mut |payload| match payload {
        PatternPayload::Node(node) => {
            if let Some(variable) = &node.variable {
                out.insert(variable.to_string());
            }
        }
        PatternPayload::Relationship(rel) => {
            if let Some(variable) = &rel.variable {
                out.insert(variable.to_string());
            }
        }
        PatternPayload::Quantified(_) => {}
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
fn call_imports(
    scope: &CallScope,
    query: &Query,
    outer: &BTreeSet<String>,
) -> Vec<BTreeSet<String>> {
    match scope {
        CallScope::Wildcard => query.parts.iter().map(|_| outer.clone()).collect(),
        CallScope::Vars(names) => {
            // Text render: the import sets are the String scope domain.
            let declared: BTreeSet<String> = names.iter().map(|name| name.to_string()).collect();
            query.parts.iter().map(|_| declared.clone()).collect()
        }
        CallScope::None => query
            .parts
            .iter()
            .map(|part| part.leading_imports(outer))
            .collect(),
    }
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

/// Whether a projection item is a *complex* expression — one the ORDER
/// BY/WHERE deprecation asks to be aliased. A bare variable, a literal and
/// a parameter are simple; an aggregate is a recognized sort key.
fn is_complex_projection_item(expr: &Expr) -> bool {
    !is_agg_call(expr)
        && !matches!(
            expr,
            Expr::RuntimeValue(_)
                | Expr::Null
                | Expr::Bool(_)
                | Expr::Int(_)
                | Expr::Float(_)
                | Expr::String(_)
                | Expr::Parameter(_)
                | Expr::Variable(_)
                | Expr::Star
                | Expr::CountStar
        )
}

/// The complex projection items `expr` contains as a *proper* sub-expression,
/// in projection order.
///
/// One pre-order walk of the expression's children: every visited node is
/// tested against the still-unmatched candidates, and a match drops its
/// candidate so the walk never re-tests a pair. Nothing is materialized, so
/// the walk answers every candidate per expression rather than once per
/// (expression, item) pair.
///
/// The walk's cost is nodes × items, so it declines once that product passes
/// [`WorkMeter`]: a wide projection ordered by a wide
/// expression would otherwise pin a core, and a partial answer (some
/// references reported) would be no more truthful than none.
///
/// The root is skipped by walking `expr.children()`: an expression equal to
/// the whole subclause expression is the projection item itself, a
/// recognized sort key rather than a reference. Identity would be a cheaper
/// key than the structural comparison, but it is not available here: `visit`
/// hands the callback borrows that cannot escape the closure, and lowering
/// builds `ORDER BY` expressions from a separately parsed syntax tree, so
/// the same expression reaches this check as a structural copy rather than a
/// shared node.
fn complex_references<'a>(
    meter: &mut WorkMeter,
    expr: &Expr,
    candidates: &[(usize, &'a Projection)],
) -> Vec<(usize, &'a Projection)> {
    if candidates.is_empty() {
        return Vec::new();
    }
    // Count the visited nodes first — a comparison-free walk — so a
    // pathological nodes × items pair declines before any of the pair work
    // rather than partway through it.
    let mut nodes = 0u64;
    for child in expr.children() {
        child.visit(&mut |_| nodes += 1);
    }
    if !meter.charge_pairs(nodes, candidates.len()) {
        return Vec::new();
    }
    let mut matched = vec![false; candidates.len()];
    for child in expr.children() {
        child.visit(&mut |node| {
            for (position, (_, item)) in candidates.iter().enumerate() {
                if !matched[position] && node == &item.expr {
                    matched[position] = true;
                }
            }
        });
    }
    candidates
        .iter()
        .zip(matched)
        .filter_map(|(candidate, matched)| matched.then_some(*candidate))
        .collect()
}

/// Whether a relationship type expression admits no single type: the
/// exactly-one-type universe is the expression's own names plus one fresh
/// name no literal mentions. A dynamic part makes the answer undecidable
/// here, so it declines (returns `false`).
///
/// The check tries every candidate type against the whole expression, so its
/// cost is candidates × expression nodes. Every candidate can fail — that is
/// what an unsatisfiable expression is — so nothing short-circuits. Past
/// [`WorkMeter`] the check declines (returns `false`, emitting
/// no notification): a wide crafted conjunction must not pin a core.
fn type_expression_unsatisfiable(expr: &LabelExpr, meter: &mut WorkMeter) -> bool {
    let mut names = BTreeSet::new();
    collect_label_names(expr, &mut names);
    let candidate_count = names.len() as u64 + 1;
    if !meter.charge_pairs(candidate_count, label_expression_size(expr)) {
        return false;
    }
    let mut candidates: Vec<Option<&str>> = names.iter().map(|name| Some(name.as_str())).collect();
    candidates.push(None);
    // The first candidate's evaluation visits every node, so a dynamic
    // member anywhere makes the first call `None` and declines the check.
    let mut satisfiable = false;
    for candidate in candidates {
        match label_expression_satisfied(expr, candidate) {
            Some(true) => satisfiable = true,
            Some(false) => {}
            None => return false,
        }
    }
    !satisfiable
}

/// The expression's node count — the per-candidate cost of
/// [`label_expression_satisfied`].
fn label_expression_size(expr: &LabelExpr) -> usize {
    match expr {
        LabelExpr::Label(_) | LabelExpr::Wildcard | LabelExpr::Dynamic { .. } => 1,
        LabelExpr::Not(inner) => 1 + label_expression_size(inner),
        LabelExpr::And(items) | LabelExpr::Or(items) => {
            1 + items.iter().map(label_expression_size).sum::<usize>()
        }
    }
}

/// The expression's truth under a relationship whose single type is
/// `candidate`; `None` is a type the expression does not name. The result
/// is `None` when the expression holds a dynamic label (its truth is a
/// runtime fact), so the undecidable case is the callee's typed answer, not
/// a caller-side guard. Every node is visited (no short-circuit), so the
/// `None` surfaces whatever the candidate.
fn label_expression_satisfied(expr: &LabelExpr, candidate: Option<&str>) -> Option<bool> {
    Some(match expr {
        LabelExpr::Label(name) => name.with_str(|text| candidate == Some(text)),
        LabelExpr::Wildcard => true,
        LabelExpr::Not(inner) => !label_expression_satisfied(inner, candidate)?,
        LabelExpr::And(items) => {
            let mut all = true;
            for item in items {
                all &= label_expression_satisfied(item, candidate)?;
            }
            all
        }
        LabelExpr::Or(items) => {
            let mut any = false;
            for item in items {
                any |= label_expression_satisfied(item, candidate)?;
            }
            any
        }
        LabelExpr::Dynamic { .. } => return None,
    })
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;
