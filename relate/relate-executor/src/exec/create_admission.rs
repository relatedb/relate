//! The one per-row-independence admission for a `CREATE` pattern set
//! (ADR-0209 §1), shared by the two consumers that must agree on it: the
//! bulk-write analyzer ([`super::bulk`], a MATCH-seeded global write) and the
//! batch-tail classifier ([`super::batch_tail`], a generator-seeded tail). It
//! also carries the expression-scope helpers the sibling write-rule checks in
//! `bulk` (`SET`/`REMOVE`) read, so one scope vocabulary serves every
//! per-row-independence rule.

use std::collections::HashSet;

use relate_ir::{Expr, NodePattern, PathPattern, RelPattern};

use super::label_expr_ok;

/// `expr` references no variable outside `allowed`.
pub(super) fn refs_within(expr: &Expr, allowed: &HashSet<String>) -> bool {
    let mut vars = HashSet::new();
    relate_ir::expr_vars(expr, &mut vars);
    vars.is_subset(allowed)
}

/// Whether a pattern uses the compact (non-GPM) representation and binds no
/// path value — the shape both the CREATE admission and the batch-tail read
/// gate require, spelled once.
pub(super) fn compact_pattern(pattern: &PathPattern) -> bool {
    !pattern.has_path_name() && !pattern.needs_gpm()
}

/// Whether a `CREATE` node pattern is a *fresh* entity (created here, or
/// anonymous) rather than a reference to a variable bound by the preceding
/// reads. `bound_by_reads` is the read patterns' variable set — not every
/// statement binding: a node naming a matched variable is a reuse, and the
/// planner's `bind_create_path` rejects every other rebind of an existing
/// variable, which is what makes the read-bound set the whole answer here.
/// The one classification the admission below and its callers'
/// created-entity walks both read — a node the admission keeps is skipped
/// by the walk.
pub(super) fn create_endpoint_is_fresh(
    node: &NodePattern,
    bound_by_reads: &HashSet<String>,
) -> bool {
    !node
        .variable
        .as_ref()
        .is_some_and(|var| var.with_str(|text| bound_by_reads.contains(text)))
}

/// Whether the `CREATE` patterns are per-row independent: no graph-sensitive
/// expressions, compact (non-GPM) patterns, matched variables appear only as
/// bare node endpoints, and property expressions reference only matched
/// variables.
pub(super) fn create_patterns_eligible(
    patterns: &[PathPattern],
    bound_by_reads: &HashSet<String>,
) -> bool {
    !patterns.is_empty()
        && !super::create::patterns_have_graph_sensitive_expr(patterns)
        && patterns.iter().all(|pattern| {
            compact_pattern(pattern)
                && pattern
                    .nodes
                    .iter()
                    .all(|node| node_eligible(node, bound_by_reads))
                && pattern
                    .rels
                    .iter()
                    .all(|rel| rel_eligible(rel, bound_by_reads))
        })
}

/// A `CREATE` node pattern's per-row-independence rule: the
/// freshness-independent conjunct first, then the arm that follows from the
/// classification.
fn node_eligible(node: &NodePattern, bound_by_reads: &HashSet<String>) -> bool {
    node.where_clause.is_none()
        && if create_endpoint_is_fresh(node, bound_by_reads) {
            // A fresh (or anonymous) variable: created here.
            node.properties
                .as_ref()
                .is_none_or(|p| refs_within(p, bound_by_reads))
                && label_expr_ok(&node.label_expr, &node.labels)
        } else {
            // A matched variable is a bare endpoint: no labels, properties,
            // or predicates of its own.
            node.labels.is_empty() && node.label_expr.is_none() && node.properties.is_none()
        }
}

/// A `CREATE` relationship pattern's per-row-independence rule.
fn rel_eligible(rel: &RelPattern, bound_by_reads: &HashSet<String>) -> bool {
    !rel.var_length
        && !rel.quantified
        && label_expr_ok(&rel.type_expr, &rel.types)
        && rel.where_clause.is_none()
        && rel
            .properties
            .as_ref()
            .is_none_or(|p| refs_within(p, bound_by_reads))
}
