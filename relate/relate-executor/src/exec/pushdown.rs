//! MATCH limit pushdown caps (DISTINCT/ORDERED), one-hop aggregate
//! folding (ADR-0036), residual predicates, and liveness trimming.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use relate_core::{Result, Value};
use relate_ir::{clause_read_vars, BinaryOp, Clause, Expr, PathPattern};
use relate_names::SmallName;

use crate::eval::{expr_vars, predicate_holds};
use crate::overlay::ExecGraph;
use crate::row::{row_bound, Row};

pub(crate) use super::agg_fold::FoldedReturn;
pub(crate) use super::match_cap::{match_limit_cap, MatchCap};
use super::scope::{
    call_scope_reads, for_each_pattern_bound_name, has_star_item, ProjectionFields,
};

static ANON: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Residual<'a> {
    pub conjuncts: &'a [Expr],
    pub scope: &'a HashSet<String>,
}

impl Residual<'_> {
    /// The degenerate residual — no conjuncts, empty scope: the one
    /// spelling of "read with no residual pushdown".
    pub(crate) fn empty() -> Residual<'static> {
        static EMPTY_SCOPE: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
        Residual {
            conjuncts: &[],
            scope: EMPTY_SCOPE.get_or_init(HashSet::new),
        }
    }
}

/// The unscoped readiness kernel: every variable the expression names is
/// bound in `row`. [`conjunct_ready`] narrows it to a residual's scope.
/// The names-bound walk is the eval tier's one kernel
/// ([`relate_ir::all_vars_satisfy`]) with the row tier's one membership
/// predicate ([`crate::row::row_bound`]) — the same spelling the seek
/// recognizers' readiness reads, never re-spelled here.
pub(crate) fn expr_ready(expr: &Expr, row: &Row) -> bool {
    relate_ir::all_vars_satisfy(expr, crate::row::row_bound(row))
}

/// Whether `expr` can be evaluated against `row`: every variable it names
/// that `scope` can bind is present, the loop's optional `binding` (the
/// element's own variable, bound on every candidate before the residual
/// runs) counting as present. Names outside the scope are
/// subquery-local — `EXISTS { MATCH (z) … }` names `z`, which the
/// subquery itself binds — so they never gate readiness; the caller's
/// later predicate pass covers an expression that is not ready here.
/// The kernel ([`relate_ir::all_vars_satisfy`]) with the scope's
/// narrowing folded into the membership — the one spelling of it, read
/// by both this predicate and [`ready_conjuncts`]'s filter: a name gates
/// readiness only when the scope can bind it and neither the row nor
/// the loop's binding has it.
pub(crate) fn conjunct_ready(
    expr: &Expr,
    row: &Row,
    scope: &HashSet<String>,
    binding: Option<&SmallName>,
) -> bool {
    let bound = row_bound(row);
    relate_ir::all_vars_satisfy(expr, |v| {
        !scope.contains(v) || binding.is_some_and(|b| b.with_str(|t| t == v)) || bound(v)
    })
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
            // The binding name compares through `with_str` inside the one
            // readiness predicate — no owned String per call (this
            // derivation runs once per candidate loop's entry row).
            !settled.contains(index) && conjunct_ready(c, row, residual.scope, binding)
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

/// The one give-up policy of the MATCH trim: a catalog command, a
/// conditional or `FOREACH` body, or a whole-row `CALL { }` scope (which
/// imports every column) leaves the trim undecided, so the caller keeps the
/// whole row.
fn abandons_trim(clause: &Clause) -> bool {
    match clause {
        Clause::Catalog(_) | Clause::Conditional { .. } | Clause::Foreach { .. } => true,
        Clause::CallSubquery { scope, .. } => call_scope_reads(scope).is_none(),
        _ => false,
    }
}

pub(crate) fn live_from(suffix: &[Clause]) -> Option<HashSet<String>> {
    let mut needed = HashSet::new();
    for c in suffix {
        // The one give-up policy, consulted once before any clause's reads.
        if abandons_trim(c) {
            return None;
        }
        match c {
            // A projection's live set is its own position policy.
            Clause::Return { .. } | Clause::With { .. } => {
                let fields = ProjectionFields::from_clause(c)
                    .expect("Return/With always extract to ProjectionFields");
                return projection_live(&fields, &mut needed);
            }
            // Every other clause's read set is the canonical per-clause
            // enumeration (an import list's names included), so no slot list
            // lives here.
            other => clause_read_vars(other, &mut needed),
        }
    }
    None
}

pub(crate) fn trim_rows(graph: &ExecGraph<'_>, rows: &mut [Row], live: &HashSet<String>) {
    for row in rows {
        row.retain(|k, _| live.contains(k) || graph.is_internal_key(k));
    }
}

pub(crate) fn match_scope(
    graph: &ExecGraph<'_>,
    row: &Row,
    patterns: &[PathPattern],
) -> HashSet<String> {
    // The row-key half is the one visible-keys walk; the pattern half adds
    // the names this clause is about to bind.
    let mut s: HashSet<String> = super::scope::visible_keys(graph, row).into_iter().collect();
    for pattern in patterns {
        for_each_pattern_bound_name(pattern, &mut |name| {
            s.insert(name.to_string());
        });
    }
    s
}

/// A fresh anonymous-element name: the `__a` prefix through the one
/// freshening policy ([`ExecGraph::uncolliding_prefix`], lengthened past
/// every declared name), then the global counter — so a user binding such
/// as `` `__a0` `` is never overwritten by (or read back as) a synthetic
/// one, by construction rather than a retry loop.
pub(crate) fn anon_name(graph: &ExecGraph<'_>) -> String {
    let prefix = graph.uncolliding_prefix("__a", std::iter::empty());
    format!("{prefix}{}", ANON.fetch_add(1, Ordering::Relaxed))
}
