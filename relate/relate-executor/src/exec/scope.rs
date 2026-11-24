//! Row scope helpers: `RETURN *` column names, `CALL { … }` read rules,
//! and post-write row refresh.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::{
    walk_pattern_elements, CallScope, Clause, Expr, OrderItem, PathPattern, PatternElement,
    Projection, SingleQuery,
};
use relate_names::SmallName;

use crate::eval::is_agg_expr;
use crate::overlay::is_internal_column;
use crate::overlay::ExecGraph;
use crate::row::Row;

/// The output names of a projection's non-`*` items, in item order —
/// the one walk the parked-prefix check, the sink's output names and the
/// result-column models read.
pub(crate) fn projection_names(items: &[Projection]) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .filter(|(_, p)| !is_star_item(p))
        .map(|(i, p)| p.output_name(i))
        .collect()
}

/// A result column of a rectangular projection row: every projection path
/// binds each result column name — and, at projection time, every parked
/// `__ord*` sort key ([`park_order_keys`]) — on every row by contract, so a
/// missing name is a bug, not a null. The sites that assert it: the result
/// materialization ([`return_result`]), the named-column reads
/// ([`fill_columns`] — the parked-key readback and the DISTINCT key
/// extraction both go through it), the sink's output construction, and the
/// declared-column move ([`super::take_declared_columns`]).
pub(crate) fn result_column<'r>(row: &'r Row, name: &str) -> &'r Value {
    row.get(name)
        .expect("projection rows carry every result column")
}

/// Whether the projection produces `name` as one of its output columns.
pub(crate) fn projects_name(items: &[Projection], name: &SmallName) -> bool {
    // The projected output names are the String rendering domain; the
    // IR variable probes them through the text bridge.
    name.with_str(|text| projects_name_text(items, text))
}

fn projects_name_text(items: &[Projection], name: &str) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(index, item)| !is_star_item(item) && item.output_name(index) == name)
}

/// One projection's (`RETURN` / `WITH`) borrowed field bundle, so helpers
/// over a projection read named fields instead of positional parameters
/// (three of which are the identical `&Option<Expr>`). `where_clause` is
/// `None` for `RETURN`, the `WITH`'s own filter otherwise.
#[derive(Clone, Copy)]
pub(crate) struct ProjectionFields<'a> {
    pub distinct: bool,
    pub items: &'a [Projection],
    pub group_by: Option<&'a [Expr]>,
    pub order: &'a [OrderItem],
    pub skip: Option<&'a Expr>,
    pub limit: Option<&'a Expr>,
    pub where_clause: Option<&'a Expr>,
}

impl<'a> ProjectionFields<'a> {
    /// The one `RETURN`/`WITH` field extraction: the clause's projection
    /// fields in the shapes every consumer reads (group-by keys and the
    /// optional expressions as borrows), `None` for any other clause. All
    /// cap/sink/liveness sites consult this instead of re-destructuring the
    /// same seven fields.
    pub(crate) fn from_clause(clause: &'a Clause) -> Option<Self> {
        match clause {
            Clause::Return {
                distinct,
                group_by,
                items,
                order,
                skip,
                limit,
            } => Some(Self {
                distinct: *distinct,
                items,
                group_by: group_by.as_deref(),
                order,
                skip: skip.as_ref(),
                limit: limit.as_ref(),
                where_clause: None,
            }),
            Clause::With {
                distinct,
                group_by,
                items,
                order,
                skip,
                limit,
                where_clause,
                ..
            } => Some(Self {
                distinct: *distinct,
                items,
                group_by: group_by.as_deref(),
                order,
                skip: skip.as_ref(),
                limit: limit.as_ref(),
                where_clause: where_clause.as_ref(),
            }),
            _ => None,
        }
    }

    /// Whether an `ORDER BY` key or the `WITH` filter aggregates: the
    /// aggregating subclauses, which make a `DISTINCT` projection
    /// aggregating and route the filter to `HAVING`.
    pub(crate) fn subclause_aggregates(&self) -> bool {
        self.order.iter().any(|o| is_agg_expr(&o.expr))
            || self.where_clause.is_some_and(is_agg_expr)
    }

    /// Whether any projected item aggregates.
    pub(crate) fn items_aggregate(&self) -> bool {
        self.items.iter().any(|p| is_agg_expr(&p.expr))
    }

    /// [`Self::from_clause`] for the arm that has already matched a
    /// `WITH`/`RETURN`: the one place that states the invariant, as a
    /// typed engine error — never a panic beside the handled `None` of
    /// the same failure.
    pub(crate) fn of_projection(clause: &'a Clause) -> Result<Self> {
        Self::from_clause(clause).ok_or_else(|| {
            Error::internal("a projection arm was dispatched on a non-projection clause")
        })
    }
}

pub(crate) fn push_scope_name(names: &mut Vec<String>, name: impl Into<String>) {
    let name = name.into();
    if !names.iter().any(|n| n == &name) {
        names.push(name);
    }
}

/// The one walk of the names a path pattern BINDS: its path names (the outer
/// path variable and a named parenthesized subpath), every declared node and
/// relationship variable, then the element variables sunk in document order
/// through the canonical [`for_each_gpm_element_name`] walk (recursing into
/// quantified sub-paths). The names an element *reads* (its predicate,
/// property map, label expression) are not bindings — they reach the trim
/// through the IR's `clause_read_vars`, not this sink.
/// Every sink — null-binding, scope-set insertion, ordered column push —
/// differs only in what it does with the name.
pub(crate) fn for_each_pattern_bound_name(pattern: &PathPattern, sink: &mut impl FnMut(&str)) {
    // The names ride the IR's own representation; each sink call renders
    // through the allocation-free text read.
    for name in pattern.path_names() {
        name.with_str(|text| sink(text));
    }
    for node in &pattern.nodes {
        if let Some(variable) = &node.variable {
            variable.with_str(|text| sink(text));
        }
    }
    for rel in &pattern.rels {
        if let Some(variable) = &rel.variable {
            variable.with_str(|text| sink(text));
        }
    }
    // The element phase is the canonical walk, sunk in DOCUMENT order (the
    // pre-refactor contract): the caller's sink is order-sensitive —
    // `RETURN *` / `WITH *` column order and subquery export columns — so
    // the dedup is an order-preserving seen-set, never a HashSet iteration.
    let mut seen = HashSet::new();
    for_each_gpm_element_name(&pattern.elements, &mut |name| {
        if seen.insert(name.to_string()) {
            sink(name);
        }
    });
}

/// The one walk of the names a pattern element BINDS: each element's own
/// variable, recursing into quantified sub-paths, through the canonical
/// element walk.
fn for_each_gpm_element_name(elements: &[PatternElement], sink: &mut impl FnMut(&str)) {
    walk_pattern_elements(elements, &mut |payload| {
        if let Some(name) = payload.variable() {
            name.with_str(|text| sink(text));
        }
    });
}

/// The names one clause declares, in declaration order — the one walk of
/// the clause → bound-names vocabulary: pattern variables (a `MATCH`'s
/// attached `SEARCH` includes its `SCORE AS` column), `UNWIND` aliases,
/// the `LOAD CSV` variable, `CALL … YIELD` columns, and a projection's
/// non-`*` output names. Both consumers of the vocabulary fold over this
/// one walk — [`star_scope_names`] (the evolving scope: a `WITH` replaces
/// it) and [`declare_plan_names`] (the cumulative statement-wide declared
/// set) — so the two can never disagree about which names a clause binds.
/// This is the executor's walk over the *planned* clause; the front's
/// `clause_introduced_names` walks the parser AST before planning and also
/// folds the catalog commands' output columns, which no plan carries.
pub(crate) fn clause_declared_names(clause: &Clause) -> Vec<String> {
    let mut names = Vec::new();
    let pattern_names = |patterns: &[PathPattern], names: &mut Vec<String>| {
        for pattern in patterns {
            for_each_pattern_bound_name(pattern, &mut |name| {
                push_scope_name(names, name.to_string())
            });
        }
    };
    match clause {
        Clause::Match {
            patterns, search, ..
        } => {
            pattern_names(patterns, &mut names);
            // A `MATCH`'s attached `SEARCH` binds its `SCORE AS` column
            // exactly as a pattern variable does — `RETURN *` star-projects
            // it and the declaration walk records it.
            if let Some(score) = search.as_deref().and_then(|s| s.score_var.as_ref()) {
                push_scope_name(&mut names, score.to_string());
            }
        }
        Clause::Create { patterns } => pattern_names(patterns, &mut names),
        Clause::Merge { pattern, .. } => {
            for_each_pattern_bound_name(pattern, &mut |name| {
                push_scope_name(&mut names, name.to_string())
            });
        }
        Clause::Unwind { alias, .. } => push_scope_name(&mut names, alias.to_string()),
        Clause::LoadCsv { variable, .. } => push_scope_name(&mut names, variable.to_string()),
        Clause::Call { yield_items, .. } => {
            for name in yield_items.effective_names() {
                push_scope_name(&mut names, name.to_string());
            }
        }
        Clause::With { items, .. } | Clause::Return { items, .. } => {
            for name in projection_names(items) {
                push_scope_name(&mut names, name);
            }
        }
        _ => {}
    }
    names
}

/// The names a `RETURN *`/`WITH *` expands when there is no row to read them
/// off — the declared scope, so the empty table still has the right columns.
/// Every binding clause folds through the one per-clause vocabulary
/// ([`clause_declared_names`], which returns nothing for the non-binding
/// variants), so the star scope and the statement's declaration walk model
/// exactly the same names — a `LOAD CSV` alias or a later binding form
/// cannot appear in one and not the other. The only local policy is the
/// scope's own: a `WITH` replaces the carried names, the terminal `RETURN`
/// stops the walk.
pub(crate) fn star_scope_names(clauses: &[Clause]) -> Vec<String> {
    let mut names = Vec::new();
    for c in clauses {
        match c {
            Clause::With { items, .. } => {
                // A `WITH` replaces the scope; under `WITH *` it keeps the
                // names bound so far and adds the explicit items.
                let carried = std::mem::take(&mut names);
                names = return_columns(items, move || carried);
            }
            Clause::Return { .. } => break,
            _ => {
                for name in clause_declared_names(c) {
                    push_scope_name(&mut names, name);
                }
            }
        }
    }
    names.sort();
    names
}

/// Whether one projection item is the `*` item — the one spelling of the
/// per-item predicate every use site derives from.
pub(crate) fn is_star_item(item: &Projection) -> bool {
    matches!(item.expr, Expr::Star)
}

/// Whether a projection's item list carries a `*` item — the one spelling
/// of the list-level predicate, consulted by [`return_columns`] (which
/// expands it), the `WITH` arm of [`star_scope_names`] (which replaces the
/// carried scope) and the liveness walk ([`super::pushdown`], which gives
/// up on it).
pub(crate) fn has_star_item(items: &[Projection]) -> bool {
    items.iter().any(is_star_item)
}

/// The terminal `RETURN` of one UNION arm: its clause list (the declared
/// scope a `*` item expands) and its projection items. `None` when the arm
/// does not conclude in a `RETURN` (a `WHEN` body, `FINISH`, a write). The
/// one probe of that fact — [`return_columns`]'s callers
/// (`stage_export_columns`, `failed_batch_row`) read it instead of each
/// re-opening `clauses.last()`.
pub(crate) fn terminal_return_items(part: &SingleQuery) -> Option<(&[Clause], &[Projection])> {
    match part.clauses.last()? {
        Clause::Return { items, .. } => Some((part.clauses.as_slice(), items.as_slice())),
        _ => None,
    }
}

/// The `__` namespace reservation (OPLANE_REQ-00328162): no user-authored
/// output name (alias) may fall in the internal `__` namespace on an
/// optimized path. The match sink and the aggregate folds are those paths —
/// each declines a colliding alias so the ordinary projection, which treats
/// every name as a user column, answers it. Parked `ORDER BY` keys are not a
/// reason to decline: their prefix is chosen to miss every result column
/// (`super::order::OrderKeys`).
pub(crate) fn projection_collides_with_internal_names(items: &[Projection]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(index, item)| is_internal_column(&item.output_name(index)))
}

/// Whether two items resolve to the same output column name.
///
/// The front-end's `ColumnNameConflict` check compares parser-level names
/// (verbatim source text), while [`Projection::output_name`] canonicalizes —
/// so `n.name` and `` n.`name` `` pass that check yet resolve alike. A path
/// that writes items over one row by name would collapse them; the match sink
/// declines such a projection and leaves it to the ordinary projection.
pub(crate) fn projection_repeats_output_name(items: &[Projection]) -> bool {
    let mut seen = HashSet::new();
    items
        .iter()
        .enumerate()
        .any(|(index, item)| !seen.insert(item.output_name(index)))
}

/// The result columns of a `RETURN`/`WITH` item list: the names its `*`
/// item expands, followed by every non-star item's output name. `star` is
/// called only when the list has a `*` item, so each caller picks the
/// expansion source (the first row's visible keys, the declared scope for
/// an empty table, the `WITH`'s carried-over scope). Dedup-preserving: the
/// first occurrence wins, so an explicit item repeating a star name keeps
/// the star's place.
///
/// The one implementation of that policy: the projection's result columns
/// ([`super::project::projection_result_columns`]) and the `CALL { … }`
/// export set ([`super::call::call_export_columns`]) both call it, so a star
/// can never silently drop its sibling items.
pub(crate) fn return_columns(
    items: &[Projection],
    star: impl FnOnce() -> Vec<String>,
) -> Vec<String> {
    let mut columns = if has_star_item(items) {
        star()
    } else {
        Vec::new()
    };
    for (index, item) in items.iter().enumerate() {
        if is_star_item(item) {
            continue;
        }
        let name = item.output_name(index);
        if !columns.contains(&name) {
            columns.push(name);
        }
    }
    columns
}

/// The names a `CALL { … }` clause reads from the enclosing row, or `None`
/// when its scope reads the whole row (the naked `CALL { … }` and
/// `CALL (*) { … }` forms). The one statement of that rule, at the
/// granularity both liveness walks need: the `UNWIND` source strip asks
/// whether a given column is among them ([`super::unwind`]), the expression
/// liveness pass takes the whole set ([`super::pushdown::live_from`]).
pub(crate) fn call_scope_reads(scope: &CallScope) -> Option<&[SmallName]> {
    match scope {
        CallScope::Vars(vars) => Some(vars),
        CallScope::None | CallScope::Wildcard => None,
    }
}

/// Refill `out` with the row's named columns, cloned — the one spelling of
/// the named-column read, beside the [`result_column`] invariant it
/// asserts. A caller that owns a buffer across rows refills it; one that
/// needs a fresh vector wraps this.
pub(crate) fn fill_columns<'n>(
    row: &Row,
    names: impl IntoIterator<Item = &'n str>,
    out: &mut Vec<Value>,
) {
    out.clear();
    out.extend(
        names
            .into_iter()
            .map(|name| result_column(row, name).clone()),
    );
}

/// The row's user-visible keys: every key the executor did not synthesize.
pub(crate) fn visible_keys(graph: &ExecGraph<'_>, row: &Row) -> Vec<String> {
    row.keys()
        .filter(|k| !graph.is_internal_key(k))
        .cloned()
        .collect()
}

/// The seed row of a nested body that sees the enclosing row as its whole
/// scope — the implicit-import forms `CALL { … }`, `CALL (*) { … }` and the
/// expression subqueries `EXISTS { … }` / `COUNT { … }` / `COLLECT { … }`:
/// the enclosing row without the clause engine's internal columns. A
/// `__`-prefixed slot (the match sink's parked `__ord*` sort keys, the
/// `__agg_*` seeds, other clause temporaries) describes the *enclosing*
/// clause and never the body's own state; a body that inherited one reads
/// it as its own and skips items it never evaluated.
///
/// Not for the imperative bodies (`exec_conditional`'s CIP-135 `WHEN`/
/// `ELSE` branches, `exec_foreach`'s per-element body): they run on the
/// caller's row unchanged — a sink projection only ever feeds the clause
/// immediately after its MATCH, so a `Conditional`/`Foreach` body can never
/// receive sink-parked `__ord*`/`__agg_*` rows, and no strip is needed.
pub(crate) fn nested_seed_row(graph: &ExecGraph<'_>, row: &Row) -> Row {
    let mut seed = row.clone();
    seed.retain(|name, _| !graph.is_internal_key(name));
    seed
}

/// Re-attach the importing-subquery scope bindings a `WITH` projection
/// dropped: a binding already present in the row is the body's own value
/// and wins. The keep-visible half of the importing-subquery scope policy
/// (the seed half is [`nested_seed_row`]), shared by the `WITH` arm and the
/// `CALL { … }` NEXT-stage recursion. See `run_nested_clauses_scoped`.
pub(crate) fn carry_scope_bindings(rows: &mut [Row], scope: &Row) {
    for row in rows {
        for (name, value) in scope {
            row.entry(name.clone()).or_insert_with(|| value.clone());
        }
    }
}

/// Fold a write clause's effects back into the rows that drove it: a bound
/// entity the write updated is re-read from the store, and one the write
/// deleted becomes an **empty tombstone** — the same id, no labels, no
/// properties (a returned deleted node renders `()`).
/// The tombstone keeps the identity a later pattern needs: the clause
/// engine still matches a deleted-but-bound variable through it
/// (`node_ok`'s snapshot path), while a fresh scan of the store finds
/// nothing. A deleted entity is never refetched.
pub(crate) fn refresh_rows(graph: &ExecGraph<'_>, rows: &mut [Row]) {
    for row in rows {
        let keys: Vec<_> = row.keys().cloned().collect();
        for k in keys {
            match row.get(&k) {
                Some(Value::Node(n)) => {
                    if let Some(fresh) = graph.node(n.id) {
                        row.insert(k, Value::from_node(fresh));
                    } else if graph.is_deleted_node(n.id) {
                        row.insert(
                            k,
                            Value::from_node(relate_core::Node {
                                id: n.id,
                                labels: Arc::new(Vec::new()),
                                properties: relate_core::PropertyMap::share(BTreeMap::new()),
                            }),
                        );
                    }
                }
                Some(Value::Relationship(r)) => {
                    if let Some(fresh) = graph.rel(r.id) {
                        row.insert(k, Value::from_relationship(fresh));
                    } else if graph.is_deleted_rel(r.id) {
                        // Same tombstone contract as the node arm above: the
                        // identity survives (id, endpoints, type — what a
                        // later pattern matches on), the properties do not.
                        // A property read on the tombstone — `r.since` or
                        // the `r {.*}` projection — raises the one
                        // DeletedEntityAccess rejection, never the pre-delete
                        // map and never a silent `{}`.
                        row.insert(
                            k,
                            Value::from_relationship(relate_core::Relationship {
                                id: r.id,
                                start: r.start,
                                end: r.end,
                                rel_type: r.rel_type.clone(),
                                properties: relate_core::PropertyMap::share(BTreeMap::new()),
                            }),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}
