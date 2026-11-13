//! MATCH limit pushdown caps (DISTINCT/ORDERED), one-hop aggregate
//! folding (ADR-0036), residual predicates, and liveness trimming.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use relate_core::{Result, Value};
use relate_ir::{BinaryOp, Clause, Expr, PathPattern, RemoveItem, SetItem};
use relate_names::SmallName;

use crate::eval::{expr_vars, label_expr_vars, predicate_holds};
use crate::overlay::ExecGraph;
use crate::row::Row;

pub(crate) use super::agg_fold::FoldedReturn;
pub(crate) use super::match_cap::{match_limit_cap, MatchCap};
use super::scope::{
    add_gpm_element_names, call_scope_reads, for_each_pattern_bound_name, has_star_item,
    is_internal_column, ProjectionFields,
};

static ANON: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Residual<'a> {
    pub conjuncts: &'a [Expr],
    pub scope: &'a HashSet<String>,
}

/// The degenerate residual — no conjuncts, empty scope: the one spelling
/// of "read with no residual pushdown", used by every caller that reads
/// candidates without a MATCH predicate of its own (the eval walk engine,
/// the agg folds, the exists probe, the shortest-route start).
impl Residual<'_> {
    /// The degenerate case of [`Self::with_conjuncts`]: no conjuncts
    /// either.
    pub(crate) fn empty() -> Residual<'static> {
        Self::with_conjuncts(&[])
    }

    /// Seek material without a scope: conjuncts to consider, no names
    /// bound — the one spelling of that shape, so no caller fabricates a
    /// dead local scope beside its conjuncts.
    pub(crate) fn with_conjuncts(conjuncts: &[Expr]) -> Residual<'_> {
        static EMPTY_SCOPE: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
        Residual {
            conjuncts,
            scope: EMPTY_SCOPE.get_or_init(HashSet::new),
        }
    }
}

/// The unscoped readiness kernel: every variable the expression names is
/// bound in `row`. [`conjunct_ready`] narrows it to a residual's scope.
pub(crate) fn expr_ready(expr: &Expr, row: &Row) -> bool {
    let mut vars = HashSet::new();
    expr_vars(expr, &mut vars);
    vars.iter().all(|name| row.contains_key(name))
}

/// Whether `expr` can be evaluated against `row`: every variable it names
/// that `scope` can bind is present. Names outside the scope are
/// subquery-local — `EXISTS { MATCH (z) … }` names `z`, which the
/// subquery itself binds — so they never gate readiness; the caller's
/// later predicate pass covers an expression that is not ready here.
pub(crate) fn conjunct_ready(expr: &Expr, row: &Row, scope: &HashSet<String>) -> bool {
    let mut vars = HashSet::new();
    expr_vars(expr, &mut vars);
    vars.retain(|v| scope.contains(v));
    vars.iter().all(|v| row.contains_key(v))
}

/// The degenerate residual pass — no settled conjuncts — the one-line
/// form every no-skip caller reads; the body lives once in
/// [`residual_ok_skipping`].
pub(crate) fn residual_ok(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    residual: &Residual<'_>,
) -> Result<bool> {
    residual_ok_skipping(graph, row, params, residual, &[])
}

/// The residual's conjuncts that are ready on `row` — every scope name
/// they reference bound — plus, when the caller's loop binds one more
/// variable on every candidate before the residual runs (`binding`, the
/// element's own variable), the conjuncts that become ready with it —
/// minus the conjuncts whose indices `settled` names: the scan-predicate
/// gate already evaluated each of those for exactly their one variable
/// under the candidate's own store read, and the compile rules
/// (predicate_spec.rs) exist to make that evaluation's inputs identical
/// to this pass's, so the re-run is redundant work, never a second
/// verdict. A skipped conjunct is known-true for the row — the gate
/// rejected every candidate it failed. Indices are into the `Residual`'s
/// own conjunct slice, the one slice the compiling walk and every one of
/// its residual passes share. Computed **once per candidate loop**:
/// readiness and settledness are facts of the walk, not of any one
/// candidate, so a scan-side loop that re-derives them per candidate
/// pays a variable-set allocation and an expression walk per scanned
/// node for an answer that never changes (GBL SF0.1 q8: 13,750
/// label-scan candidates re-derived it per forum).
pub(crate) fn ready_conjuncts<'a>(
    residual: &'a Residual<'_>,
    row: &Row,
    binding: Option<&SmallName>,
    settled: &[usize],
) -> Vec<&'a Expr> {
    residual
        .conjuncts
        .iter()
        .enumerate()
        .filter(|(index, c)| {
            if settled.contains(index) {
                return false;
            }
            let mut vars = HashSet::new();
            expr_vars(c, &mut vars);
            vars.retain(|v| residual.scope.contains(v));
            if let Some(bound) = binding {
                bound.with_str(|text| {
                    vars.remove(text);
                });
            }
            vars.iter().all(|v| row.contains_key(v))
        })
        .map(|(_, c)| c)
        .collect()
}

/// Whether every conjunct of `ready` (a [`ready_conjuncts`] list) holds on
/// `row`: the per-candidate half of [`residual_ok`], the evaluation the
/// once-per-loop readiness derivation feeds.
pub(crate) fn conjuncts_hold(
    graph: &ExecGraph<'_>,
    row: &Row,
    ready: &[&Expr],
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    for c in ready {
        if !predicate_holds(graph, row, c, params)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The single-candidate residual pass ([`residual_ok`] with skips): the
/// backward walker's per-candidate spelling of the same selection
/// [`ready_conjuncts`] makes — the settled conjuncts skipped, the rest
/// evaluated when ready on the bound row — for callers with no candidate
/// loop to hoist the derivation out of.
pub(crate) fn residual_ok_skipping(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    residual: &Residual<'_>,
    settled: &[usize],
) -> Result<bool> {
    let ready = ready_conjuncts(residual, row, None, settled);
    conjuncts_hold(graph, row, &ready, params)
}

pub(crate) fn flatten_and_owned(expr: &Expr) -> Vec<Expr> {
    match expr {
        Expr::Binary {
            op: BinaryOp::And,
            left,
            right,
        } => {
            let mut v = flatten_and_owned(left);
            v.extend(flatten_and_owned(right));
            v
        }
        other => vec![other.clone()],
    }
}

/// The variables a projection (`RETURN` / `WITH`) still needs from the
/// carried row: its item expressions, its explicit grouping keys (CIP-236:
/// evaluated *before* projection), its sort keys, its `SKIP`/`LIMIT`, and —
/// for `WITH` — its `WHERE`. One helper for both clause arms, so a new
/// projection field lands in one place.
fn projection_needs(fields: &ProjectionFields<'_>, needed: &mut HashSet<String>) {
    for i in fields.items {
        expr_vars(&i.expr, needed);
    }
    if let Some(keys) = fields.group_by {
        for k in keys {
            expr_vars(k, needed);
        }
    }
    for o in fields.order {
        expr_vars(&o.expr, needed);
    }
    if let Some(s) = fields.skip {
        expr_vars(s, needed);
    }
    if let Some(l) = fields.limit {
        expr_vars(l, needed);
    }
    if let Some(w) = fields.where_clause {
        expr_vars(w, needed);
    }
}

/// A `RETURN`/`WITH` projection's live set: `None` under a `*` item (the
/// star expands the whole row) or when any of its expressions carries a
/// scope-bearing form, otherwise the names its fields need.
///
/// An expression subquery (`EXISTS`/`COUNT`/`COLLECT`) is seeded at runtime
/// from the *whole* enclosing row, and its body may reach outer names through
/// forms `expr_vars` does not enumerate (a naked `CALL { … }` body, a
/// `CALL (vars)` scope list, an inner `CALL`'s import list). Trimming to the
/// statically-referenced names would silently drop such an import, so the
/// projection keeps every incoming column.
fn projection_live(
    fields: &ProjectionFields<'_>,
    needed: &mut HashSet<String>,
) -> Option<HashSet<String>> {
    if has_star_item(fields.items) || projection_has_scope_bearing_expr(fields) {
        return None;
    }
    projection_needs(fields, needed);
    Some(std::mem::take(needed))
}

/// Whether any expression the projection evaluates carries a scope-bearing
/// form (a subquery, pattern predicate/comprehension, or `shortestPath`).
fn projection_has_scope_bearing_expr(fields: &ProjectionFields<'_>) -> bool {
    fn scope_bearing(expr: &Expr) -> bool {
        expr.any(&mut |node| node.is_scope_bearing())
    }
    fields.items.iter().any(|item| scope_bearing(&item.expr))
        || fields
            .group_by
            .as_ref()
            .is_some_and(|keys| keys.iter().any(scope_bearing))
        || fields.order.iter().any(|item| scope_bearing(&item.expr))
        || fields.skip.is_some_and(scope_bearing)
        || fields.limit.is_some_and(scope_bearing)
        || fields.where_clause.is_some_and(scope_bearing)
}

pub(crate) fn live_from(suffix: &[Clause]) -> Option<HashSet<String>> {
    let mut needed = HashSet::new();
    for c in suffix {
        match c {
            Clause::Catalog(_) => return None,
            Clause::Match {
                patterns,
                where_clause,
                search,
                ..
            } => {
                add_pattern_names(patterns, &mut needed);
                if let Some(w) = where_clause {
                    expr_vars(w, &mut needed);
                }
                if let Some(search) = search {
                    for expr in search.spec.exprs() {
                        expr_vars(expr, &mut needed);
                    }
                }
            }
            Clause::Create { patterns } => add_pattern_names(patterns, &mut needed),
            Clause::CreateIndex { .. }
            | Clause::DropIndex { .. }
            | Clause::ShowIndexes
            | Clause::Finish => {}
            Clause::LoadCsv { url, .. } => expr_vars(url, &mut needed),
            Clause::CallSubquery {
                in_transactions,
                scope,
                ..
            } => {
                // A whole-row scope keeps everything live; an import list
                // keeps the names it names (the one `call_scope_reads` rule).
                let vars = call_scope_reads(scope)?;
                needed.extend(vars.iter().map(|name| name.to_string()));
                if let Some(tx) = in_transactions {
                    if let Some(n) = &tx.batch_size {
                        expr_vars(n, &mut needed);
                    }
                }
            }
            Clause::Merge {
                pattern,
                on_create,
                on_match,
            } => {
                add_pattern_names(std::slice::from_ref(pattern), &mut needed);
                for s in on_create.iter().chain(on_match.iter()) {
                    set_item_vars(s, &mut needed);
                }
            }
            Clause::Delete { expressions, .. } => {
                for e in expressions {
                    expr_vars(e, &mut needed);
                }
            }
            Clause::Set { items } => {
                for i in items {
                    set_item_vars(i, &mut needed);
                }
            }
            Clause::Remove { items } => {
                for i in items {
                    remove_item_vars(i, &mut needed);
                }
            }
            Clause::Unwind { expression, .. } => expr_vars(expression, &mut needed),
            Clause::Call { args, .. } => {
                for a in args.args() {
                    expr_vars(a, &mut needed);
                }
            }
            Clause::Conditional { branches, .. } => {
                for (predicate, _) in branches {
                    expr_vars(predicate, &mut needed);
                }
                return None;
            }
            Clause::Foreach { list, .. } => {
                expr_vars(list, &mut needed);
                return None;
            }
            // A bare USE graph name is a catalog identifier, not a row variable.
            Clause::Use { .. } => {}
            Clause::Return { .. } | Clause::With { .. } => {
                let fields = ProjectionFields::from_clause(c)
                    .expect("Return/With always extract to ProjectionFields");
                return projection_live(&fields, &mut needed);
            }
        }
    }
    None
}

pub(crate) fn add_pattern_names(patterns: &[PathPattern], out: &mut HashSet<String>) {
    for p in patterns {
        if let Some(v) = &p.variable {
            out.insert(v.to_string());
        }
        if let Some(predicate) = p.where_clause.as_deref() {
            expr_vars(predicate, out);
        }
        for n in &p.nodes {
            if let Some(v) = &n.variable {
                out.insert(v.to_string());
            }
            if let Some(props) = &n.properties {
                expr_vars(props, out);
            }
            if let Some(expr) = &n.label_expr {
                label_expr_vars(expr, out);
            }
        }
        for r in &p.rels {
            if let Some(v) = &r.variable {
                out.insert(v.to_string());
            }
            if let Some(props) = &r.properties {
                expr_vars(props, out);
            }
            if let Some(expr) = &r.type_expr {
                label_expr_vars(expr, out);
            }
        }
        add_gpm_element_names(&p.elements, out);
    }
}

pub(crate) fn set_item_vars(item: &SetItem, out: &mut HashSet<String>) {
    match item {
        SetItem::Property { target, value }
        | SetItem::ReplaceAll { target, value }
        | SetItem::MergeMap { target, value } => {
            expr_vars(target, out);
            expr_vars(value, out);
        }
        SetItem::Labels { target, labels } => {
            expr_vars(target, out);
            for label in labels {
                label_expr_vars(label, out);
            }
        }
        SetItem::LabelsFunction { target, value, .. } => {
            expr_vars(target, out);
            expr_vars(value, out);
        }
    }
}

pub(crate) fn remove_item_vars(item: &RemoveItem, out: &mut HashSet<String>) {
    match item {
        RemoveItem::Property { target } => expr_vars(target, out),
        RemoveItem::Labels { target, labels } => {
            expr_vars(target, out);
            for label in labels {
                label_expr_vars(label, out);
            }
        }
    }
}

pub(crate) fn trim_rows(rows: &mut [Row], live: &HashSet<String>) {
    for row in rows {
        row.retain(|k, _| live.contains(k) || is_internal_column(k));
    }
}

pub(crate) fn match_scope(row: &Row, patterns: &[PathPattern]) -> HashSet<String> {
    let mut s: HashSet<String> = row
        .keys()
        .filter(|k| !is_internal_column(k))
        .cloned()
        .collect();
    for pattern in patterns {
        for_each_pattern_bound_name(pattern, &mut |name| {
            s.insert(name.to_string());
        });
    }
    s
}

pub(crate) fn anon_name() -> String {
    format!("__a{}", ANON.fetch_add(1, Ordering::Relaxed))
}
