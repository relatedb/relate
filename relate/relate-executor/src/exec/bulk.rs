//! Batched execution of global bulk writes (ADR-0209, LABS-263).
//!
//! A statement like `MATCH (n) DETACH DELETE n` or
//! `MATCH (n) MATCH (m) WITH n AS a, m AS b CREATE (a)-[:TAGGED]->(b)`
//! has no selective anchor: it touches every matched entity, and running
//! it as one giant transaction materializes the whole redo in a single
//! scratch and lands it as a single WAL record (which can cross the
//! 64 MiB payload limit or exhaust memory). This module detects those
//! shapes at plan time — conservatively, only when per-row effects are
//! independent of batch boundaries — and provides the pieces the
//! auto-commit path (`relate::commit`) needs to run them in bounded
//! batches, one WAL commit per batch:
//!
//! - [`analyze_bulk_write`] — plan-shape detection (this file);
//! - [`BulkWrite::collect_targets`] — phase 1: snapshot the matched
//!   entity id space (memory O(entities), never O(cross product));
//! - [`BulkWrite::execute_chunk`] — phase 2: apply up to `budget` work
//!   units starting at `cursor`, on the caller's scratch.
//!
//! The two phases live in [`run`]; this file holds the plan analysis.
//!
//! Semantics: the matched set is fixed before the first batch writes,
//! and every eligible write's per-row effect is independent of when
//! sibling rows ran, so the final state and the summed write statistics
//! equal the unbatched run. On the error path earlier batches stay
//! committed — the documented `CALL { } IN TRANSACTIONS` trade-off. One
//! error is preflighted before the first commit: a non-`DETACH` node
//! `DELETE`'s `DeleteConnectedNode` check. That keeps the statement
//! atomic for this error only as long as no concurrent writer adds a
//! relationship after the preflight snapshot; such a race surfaces as a
//! mid-run error with earlier batches committed, like any other
//! mid-statement failure.

use std::collections::HashSet;

use relate_ir::{Clause, Expr, PathPattern, RemoveItem, SetItem};
use relate_names::SmallName;

use crate::graph::Graph;
use relate_planner::LogicalPlan;

use super::create_admission::{create_patterns_eligible, refs_within};
use super::label_expr_ok;

mod run;

pub use run::{BulkCursor, BulkTargets};

#[cfg(test)]
mod tests;

/// Default work estimate at or above which an eligible statement is
/// batched instead of running as one transaction.
pub const DEFAULT_BATCH_THRESHOLD: u64 = 10_000;
/// Default per-batch work budget (estimated touched entities).
pub const DEFAULT_BATCH_BUDGET: u64 = 25_000;

/// One matched variable's identity source: a bare node pattern
/// `(<var> [:labels…])` or a bare relationship pattern
/// `()-[<var> [:types…]]->()`.
#[derive(Debug, Clone)]
pub(super) enum FactorSource {
    Nodes { labels: Vec<String> },
    Rels { types: Vec<String> },
}

/// A matched variable and where its entities come from.
#[derive(Debug, Clone)]
pub(super) struct Factor {
    var: String,
    source: FactorSource,
}

/// The write clause of an eligible statement, plus what it may
/// reference: one variant per write flavor, carrying the payload the
/// execution arm needs so execution matches one enum exactly once.
#[derive(Debug, Clone)]
pub(super) enum BulkKind {
    Delete {
        detach: bool,
        /// The variables the `DELETE` names; factors not named keep
        /// their entities.
        targets: HashSet<String>,
    },
    Set {
        items: Vec<SetItem>,
    },
    Remove {
        items: Vec<RemoveItem>,
    },
    Create {
        patterns: Vec<PathPattern>,
    },
}

/// A plan-time eligible global bulk write (ADR-0209 §1).
#[derive(Debug, Clone)]
pub struct BulkWrite {
    pub(super) kind: BulkKind,
    pub(super) factors: Vec<Factor>,
}

/// Whether `pattern` is a bare node pattern `(<var> [:labels…])`: named,
/// no relationship, no properties, no predicates anywhere, no label
/// expression, no path variable, no GPM-only features.
fn bare_node_factor(pattern: &PathPattern) -> Option<Factor> {
    if pattern.has_path_name()
        || !pattern.rels.is_empty()
        || pattern.nodes.len() != 1
        || pattern.needs_gpm()
    {
        return None;
    }
    let node = &pattern.nodes[0];
    if !label_expr_ok(&node.label_expr, &node.labels)
        || node.properties.is_some()
        || node.where_clause.is_some()
    {
        return None;
    }
    Some(Factor {
        // Text render: the bulk factor's label/type lists feed the
        // store's count APIs (String domain).
        var: node.variable.as_ref()?.to_string(),
        source: FactorSource::Nodes {
            labels: node.label_names().map(|label| label.to_string()).collect(),
        },
    })
}

/// Whether `pattern` is a bare relationship pattern
/// `()-[<var> [:types…]]->()`: both endpoints anonymous and bare, the
/// relationship named, fixed length, no properties or predicates.
fn bare_rel_factor(pattern: &PathPattern) -> Option<Factor> {
    if pattern.has_path_name()
        || pattern.nodes.len() != 2
        || pattern.rels.len() != 1
        || pattern.needs_gpm()
    {
        return None;
    }
    for node in &pattern.nodes {
        if node.variable.is_some()
            || !node.labels.is_empty()
            || node.label_expr.is_some()
            || node.properties.is_some()
            || node.where_clause.is_some()
        {
            return None;
        }
    }
    let rel = &pattern.rels[0];
    if rel.var_length
        || rel.quantified
        || !label_expr_ok(&rel.type_expr, &rel.types)
        || rel.properties.is_some()
        || rel.where_clause.is_some()
    {
        return None;
    }
    Some(Factor {
        var: rel.variable.as_ref()?.to_string(),
        source: FactorSource::Rels {
            types: rel.type_names().map(|ty| ty.to_string()).collect(),
        },
    })
}

/// The variable a `SET`/`REMOVE` target expression roots at, for the bulk
/// eligibility rule below: `n`, `n.k`, or `n[k]` with `n` a plain
/// variable. Deliberately one level deep — a deeper access chain
/// (`n[x][y]`) is not eligible here, because the bulk path's per-row
/// effect must depend on a single matched variable. The recursive twin
/// ([`relate_ir::root_variable`], REL8-1813) answers the different
/// question the write-target class decision asks — "is this target
/// variable-rooted at all" — and must not replace this test.
fn target_root_var(expr: &Expr) -> Option<&SmallName> {
    match expr {
        Expr::Variable(name) => Some(name),
        Expr::Property { expr, .. } | Expr::DynamicProperty { expr, .. } => match expr.as_ref() {
            Expr::Variable(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// Shared per-item eligibility rule for `SET`/`REMOVE`: every target
/// roots at a matched variable, and every value/dynamic-key expression
/// references at most that same variable (per-row effects then cannot
/// depend on when sibling rows ran — ADR-0209 §1). Each item arrives as
/// its target expression plus the value/dynamic-key expressions to
/// scope-check.
fn per_item_eligible<'a>(
    items: impl IntoIterator<Item = (&'a Expr, Vec<&'a Expr>)>,
    scope: &HashSet<String>,
) -> bool {
    items.into_iter().all(|(target, value_exprs)| {
        let Some(root) = target_root_var(target) else {
            return false;
        };
        if !root.with_str(|text| scope.contains(text)) {
            return false;
        }
        // Text render: the allowed-set is the refs_within String domain.
        let allowed = HashSet::from([root.to_string()]);
        value_exprs.iter().all(|expr| refs_within(expr, &allowed))
    })
}

/// Whether the `SET` items are batch-safe (see [`per_item_eligible`]).
fn set_items_eligible(items: &[SetItem], scope: &HashSet<String>) -> bool {
    !items.is_empty()
        && per_item_eligible(
            items.iter().map(|item| match item {
                SetItem::Property { target, value } => {
                    let mut exprs = vec![value];
                    if let Expr::DynamicProperty { key, .. } = target {
                        exprs.push(key);
                    }
                    (target, exprs)
                }
                SetItem::ReplaceAll { target, value } => (target, vec![value]),
                SetItem::MergeMap { target, value } => (target, vec![value]),
                SetItem::Labels { target, .. } => (target, Vec::new()),
                SetItem::LabelsFunction { target, value, .. } => (target, vec![value]),
            }),
            scope,
        )
}

/// Whether the `REMOVE` items are batch-safe (same target rule; the only
/// expressions are dynamic property keys).
fn remove_items_eligible(items: &[RemoveItem], scope: &HashSet<String>) -> bool {
    !items.is_empty()
        && per_item_eligible(
            items.iter().map(|item| match item {
                RemoveItem::Property { target } => {
                    let exprs = match target {
                        Expr::DynamicProperty { key, .. } => vec![key.as_ref()],
                        _ => Vec::new(),
                    };
                    (target, exprs)
                }
                RemoveItem::Labels { target, .. } => (target, Vec::new()),
            }),
            scope,
        )
}

/// The `SET` target kind — eligible when every item reads only the
/// final scope's variables ([`set_items_eligible`]'s own contract).
fn set_kind(items: &[SetItem], scope: &HashSet<String>) -> Option<BulkKind> {
    set_items_eligible(items, scope).then(|| BulkKind::Set {
        items: items.to_vec(),
    })
}

/// The `REMOVE` target kind — eligible when every item reads only the
/// final scope's variables ([`remove_items_eligible`]'s own contract).
fn remove_kind(items: &[RemoveItem], scope: &HashSet<String>) -> Option<BulkKind> {
    remove_items_eligible(items, scope).then(|| BulkKind::Remove {
        items: items.to_vec(),
    })
}

/// The `CREATE` target kind — eligible when every pattern's refs stay
/// within the final scope ([`create_patterns_eligible`]'s own
/// contract).
fn create_kind(patterns: &[PathPattern], scope: &HashSet<String>) -> Option<BulkKind> {
    create_patterns_eligible(patterns, scope).then(|| BulkKind::Create {
        patterns: patterns.to_vec(),
    })
}

/// Whether the `DELETE` clause is batch-safe, and its kind when it is:
/// every expression must be a plain variable naming a matched factor
/// (one walk collects the named factors and classifies their sources),
/// and — unless `DETACH` — the targets may not mix nodes and
/// relationships: the connectedness preflight would have to ignore the
/// statement's own relationship targets, so that shape stays on the
/// unbatched path.
fn delete_kind(detach: bool, expressions: &[Expr], factors: &[Factor]) -> Option<BulkKind> {
    if expressions.is_empty() {
        return None;
    }
    let mut named = HashSet::with_capacity(expressions.len());
    let mut touches_nodes = false;
    let mut touches_rels = false;
    for expr in expressions {
        let Expr::Variable(name) = expr else {
            return None;
        };
        let factor = factors.iter().find(|f| name == f.var.as_str())?;
        match factor.source {
            FactorSource::Nodes { .. } => touches_nodes = true,
            FactorSource::Rels { .. } => touches_rels = true,
        }
        named.insert(name.to_string());
    }
    if !detach && touches_nodes && touches_rels {
        return None;
    }
    Some(BulkKind::Delete {
        detach,
        targets: named,
    })
}

/// A pure-rename `WITH` clause: whether it carries `*`, plus the
/// old → new name pair of each explicit variable item.
struct RenameProjection {
    star: bool,
    renames: Vec<(String, String)>,
}

/// Whether a `WITH` clause is a pure rename projection: no aggregation,
/// distinct, ordering, paging, or filter, and every item a variable or
/// `*`. `None` when the clause is not a pure rename.
fn with_rename_projection(clause: &Clause) -> Option<RenameProjection> {
    let Clause::With {
        distinct,
        group_by,
        items,
        order,
        skip,
        limit,
        where_clause,
        ..
    } = clause
    else {
        return None;
    };
    if *distinct
        || group_by.is_some()
        || !order.is_empty()
        || skip.is_some()
        || limit.is_some()
        || where_clause.is_some()
    {
        return None;
    }
    let mut star = false;
    let mut renames = Vec::new();
    for item in items {
        match &item.expr {
            Expr::Star => star = true,
            Expr::Variable(name) => {
                // Text render: the rename list is the projection's
                // String column domain.
                renames.push((
                    name.to_string(),
                    item.alias
                        .as_ref()
                        .map(|alias| alias.to_string())
                        .unwrap_or_else(|| name.to_string()),
                ));
            }
            _ => return None,
        }
    }
    Some(RenameProjection { star, renames })
}

/// Rewrite the factors through one pure-rename `WITH`, following the
/// clause's simultaneous-assignment semantics: every item reads the
/// incoming scope, so each factor's name is rewritten by at most one
/// item (its first match). The projection must cover the factors
/// exactly: under `*` every factor survives unchanged (identity
/// renames only — a real rename would bind two names to one entity),
/// and without `*` every factor must be renamed — a factor no item
/// mentions is dropped, and a dropped factor still multiplies the row
/// space (its emptiness empties the write, its multiplicity scales it
/// — REL8-2023 issues 72 and 77's WITH shape), which the batched
/// path, targeting the surviving factors' entities once each, can see
/// neither. `None` also when an alias collides with another surviving
/// factor's name (it would shadow it). The input is consumed whole, so
/// a decline never leaves a half-rewritten scope behind.
fn apply_rename_projection(
    factors: Vec<Factor>,
    projection: &RenameProjection,
) -> Option<Vec<Factor>> {
    if projection.star {
        if projection.renames.iter().any(|(old, new)| old != new) {
            return None;
        }
        return Some(factors);
    }
    let mut kept = Vec::with_capacity(factors.len());
    for mut factor in factors {
        let (_, new) = projection
            .renames
            .iter()
            .find(|(old, _)| *old == factor.var)?;
        factor.var = new.clone();
        kept.push(factor);
    }
    let names: HashSet<&str> = kept.iter().map(|f| f.var.as_str()).collect();
    (names.len() == kept.len()).then_some(kept)
}

/// Plan-time detection of a batchable global bulk write (ADR-0209 §1).
///
/// Conservative by construction: any deviation from the recognized
/// shapes — a `WHERE`, an aggregating `WITH`, a graph-sensitive `CREATE`
/// expression, a `RETURN` after the write, multiple parts, `UNION`,
/// `NEXT` — yields `None` and the statement runs unbatched.
impl BulkWrite {
    /// An O(1) upper bound on the touched-entity count, from the label
    /// index (plan-time counts, ADR-0218): product of per-factor upper
    /// bounds for row writes, sum over the named factors for a delete.
    /// `None` when a factor has no cheap bound (unlabeled rels) — the
    /// caller then pays the real collection. Overestimates are safe
    /// (the cost is collecting anyway); underestimates would wrongly
    /// skip batching, so every arm errs upward.
    pub fn estimate_upper_bound(&self, graph: &Graph) -> Option<u64> {
        let factor_bound = |f: &Factor| -> Option<u64> {
            match &f.source {
                FactorSource::Nodes { labels } if !labels.is_empty() => {
                    // intersection ≤ smallest label count
                    labels.iter().map(|l| graph.label_count(l)).min()
                }
                FactorSource::Nodes { .. } => Some(graph.live_node_count() as u64),
                // no cheap rel-type count index: decline to estimate
                FactorSource::Rels { .. } => None,
            }
        };
        match &self.kind {
            // detach work is entities + their incident rels (1 + degree per
            // node) — label counts give no cheap bound, so decline and let
            // phase 1's exact total_work() decide.
            BulkKind::Delete { detach: true, .. } => None,
            BulkKind::Delete { targets, .. } => self
                .factors
                .iter()
                .filter(|f| targets.contains(&f.var))
                .try_fold(0u64, |acc, f| {
                    factor_bound(f).map(|b| acc.saturating_add(b))
                }),
            BulkKind::Set { .. } | BulkKind::Remove { .. } | BulkKind::Create { .. } => {
                self.factors.iter().try_fold(1u64, |acc, f| {
                    factor_bound(f).map(|b| acc.saturating_mul(b))
                })
            }
        }
    }
}

pub fn analyze_bulk_write(plan: &LogicalPlan) -> Option<BulkWrite> {
    if plan.parts.len() != 1 || !plan.union_all.is_empty() || plan.next.is_some() {
        return None;
    }
    let clauses = &plan.parts[0];
    let mut factors: Vec<Factor> = Vec::new();
    let mut i = 0;
    while let Some(Clause::Match {
        optional,
        patterns,
        where_clause,
        search,
        ..
    }) = clauses.get(i)
    {
        if *optional || where_clause.is_some() || search.is_some() || patterns.is_empty() {
            return None;
        }
        for pattern in patterns {
            factors.push(bare_node_factor(pattern).or_else(|| bare_rel_factor(pattern))?);
        }
        i += 1;
    }
    if factors.is_empty() {
        return None;
    }
    // A repeated variable across patterns is a self-join in the
    // unbatched engine, not a cross product of independent factors.
    if factors.iter().map(|f| &f.var).collect::<HashSet<_>>().len() != factors.len() {
        return None;
    }
    // Pure-rename WITHs only; the write clause references the final
    // names. The decoder owns the `With` variant test — this loop owns
    // only the phase boundary: any clause the pure-rename decoder
    // declines, `With` or not, ends the phase here, and the terminal
    // checks below decline the leftover shape either way.
    while let Some(clause) = clauses.get(i) {
        let Some(projection) = with_rename_projection(clause) else {
            break;
        };
        factors = apply_rename_projection(factors, &projection)?;
        i += 1;
    }
    // Exactly one terminal write clause; nothing may follow it — the
    // one binding that carries both the arity and the scrutinee.
    let [write] = &clauses[i..] else {
        return None;
    };
    let scope: HashSet<String> = factors.iter().map(|f| f.var.clone()).collect();
    // One constructor shape per write kind — eligibility is each
    // constructor's own `Option`; anything else is not a terminal
    // write clause, and the recognizer declines.
    let kind = match write {
        Clause::Delete {
            detach,
            expressions,
        } => delete_kind(*detach, expressions, &factors),
        Clause::Set { items } => set_kind(items, &scope),
        Clause::Remove { items } => remove_kind(items, &scope),
        Clause::Create { patterns } => create_kind(patterns, &scope),
        _ => return None,
    }?;
    Some(BulkWrite { kind, factors })
}
