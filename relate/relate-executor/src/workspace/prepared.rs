//! The prepared-CREATE pattern accounting — the retained-bytes walk of
//! `exec_create`'s per-driving-row resolved pattern sets — split from
//! `workspace.rs` beside the `retained.rs` split it mirrors: the pattern
//! walk bills IR structures, the retained walk bills landed rows, and
//! both ride the same buffer and value measures.

use std::mem::size_of;

use relate_core::str_logical_bytes;
use relate_ir::{
    Expr, LabelExpr, NodePattern, PathPattern, PatternElement, QuantifiedPathPattern, RelPattern,
};
use relate_names::NamedToken;

use super::{buffer_bytes_saturating, owned_heap};

/// The prepared CREATE pattern buffer's retained bytes —
/// `exec_create`'s per-driving-row resolved pattern sets. Each set is a
/// deep clone of the clause's pattern with its graph-sensitive property
/// expressions replaced by their resolved values, so the walk bills the
/// clone's own footprint the way [`super::row_buffer_bytes`] bills rows: the
/// vector buffers of the pattern's three element tables and their name
/// lists (the lossless GPM sequence and the compact node/rel pair table
/// carry the same patterns twice, so both tables bill), every
/// expression tree node's slot and direct allocations, and each resolved
/// payload through [`owned_heap`] — the one value measure.
pub(crate) fn prepared_pattern_bytes(prepared: &[Vec<PathPattern>]) -> u64 {
    buffer_bytes_saturating(prepared.len(), size_of::<Vec<PathPattern>>() as u64)
        + prepared
            .iter()
            .map(|patterns| pattern_set_bytes(patterns))
            .sum::<u64>()
}

/// One driving row's prepared pattern set.
fn pattern_set_bytes(patterns: &[PathPattern]) -> u64 {
    buffer_bytes_saturating(patterns.len(), size_of::<PathPattern>() as u64)
        + patterns.iter().map(pattern_bytes).sum::<u64>()
}

/// One cloned pattern: its element tables' buffers, every node's and
/// rel's own footprint (inside both tables), the quantified groups'
/// nested sequences, and the path's own WHERE tree.
fn pattern_bytes(pattern: &PathPattern) -> u64 {
    let mut bytes =
        buffer_bytes_saturating(pattern.elements.len(), size_of::<PatternElement>() as u64)
            + buffer_bytes_saturating(pattern.nodes.len(), size_of::<NodePattern>() as u64)
            + buffer_bytes_saturating(pattern.rels.len(), size_of::<RelPattern>() as u64);
    for element in &pattern.elements {
        bytes += element_bytes(element);
    }
    for node in &pattern.nodes {
        bytes += node_pattern_bytes(node);
    }
    for rel in &pattern.rels {
        bytes += rel_pattern_bytes(rel);
    }
    if let Some(expr) = &pattern.where_clause {
        bytes += expr_tree_heap(expr);
    }
    bytes
}

/// One GPM sequence element: a node or rel pattern's own footprint, or a
/// quantified group's nested sequence.
fn element_bytes(element: &PatternElement) -> u64 {
    match element {
        PatternElement::Node(node) => node_pattern_bytes(node),
        PatternElement::Relationship(rel) => rel_pattern_bytes(rel),
        PatternElement::Quantified(group) => quantified_group_bytes(group),
    }
}

/// One quantified group: its element sequence's buffer, the nested
/// elements (recursively), and the group's own WHERE tree.
fn quantified_group_bytes(group: &QuantifiedPathPattern) -> u64 {
    buffer_bytes_saturating(group.elements.len(), size_of::<PatternElement>() as u64)
        + group.elements.iter().map(element_bytes).sum::<u64>()
        + group.where_clause.as_ref().map_or(0, expr_tree_heap)
}

/// One node pattern's own footprint: its name-list vectors, its label
/// expression, and its property and WHERE trees (the resolved property
/// payload bills through [`owned_heap`] inside the tree walk).
fn node_pattern_bytes(node: &NodePattern) -> u64 {
    buffer_bytes_saturating(node.labels.len(), size_of::<NamedToken>() as u64)
        + buffer_bytes_saturating(node.prop_keys.len(), size_of::<NamedToken>() as u64)
        + node.label_expr.as_ref().map_or(0, label_expr_heap)
        + node.properties.as_ref().map_or(0, expr_tree_heap)
        + node.where_clause.as_ref().map_or(0, expr_tree_heap)
}

/// One rel pattern's own footprint — the rel twin of
/// [`node_pattern_bytes`].
fn rel_pattern_bytes(rel: &RelPattern) -> u64 {
    buffer_bytes_saturating(rel.types.len(), size_of::<NamedToken>() as u64)
        + buffer_bytes_saturating(rel.prop_keys.len(), size_of::<NamedToken>() as u64)
        + rel.type_expr.as_ref().map_or(0, label_expr_heap)
        + rel.properties.as_ref().map_or(0, expr_tree_heap)
        + rel.where_clause.as_ref().map_or(0, expr_tree_heap)
}

/// The heap an IR expression tree retains, one node at a time: the
/// pre-order [`Expr::visit`] walk reaches every node, so each bills its
/// own footprint — its storage slot behind the parent (a box or a vector
/// element; the root sits inline in its pattern) plus its direct
/// allocations (a string's bytes, a map's key strings and their entry
/// slots, a resolved payload through [`owned_heap`]). Vector headers and
/// reserve slack are not walked: the figure tracks the terms that grow
/// with the statement, which is the guard's purpose.
fn expr_tree_heap(expr: &Expr) -> u64 {
    let mut nodes = 0u64;
    let mut bytes = 0u64;
    expr.visit(&mut |node| {
        nodes += 1;
        bytes += match node {
            Expr::RuntimeValue(value) => owned_heap(value),
            Expr::String(text) | Expr::Parameter(text) => str_logical_bytes(text),
            Expr::Map(entries) => {
                (entries.len() as u64) * size_of::<String>() as u64
                    + entries
                        .iter()
                        .map(|(key, _)| str_logical_bytes(key))
                        .sum::<u64>()
            }
            _ => 0,
        };
    });
    bytes + nodes.saturating_sub(1) * size_of::<Expr>() as u64
}

/// The heap a label/type expression retains: its `And`/`Or` item
/// vectors, its boxed `Not`, and the dynamic arm's expression tree.
fn label_expr_heap(expr: &LabelExpr) -> u64 {
    match expr {
        LabelExpr::Label(_) | LabelExpr::Wildcard => 0,
        LabelExpr::Dynamic { expr, .. } => size_of::<Box<Expr>>() as u64 + expr_tree_heap(expr),
        LabelExpr::Not(inner) => size_of::<Box<LabelExpr>>() as u64 + label_expr_heap(inner),
        LabelExpr::And(items) | LabelExpr::Or(items) => {
            buffer_bytes_saturating(items.len(), size_of::<LabelExpr>() as u64)
                + items.iter().map(label_expr_heap).sum::<u64>()
        }
    }
}
