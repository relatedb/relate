//! The classic walker's terminal named-path binder (extracted from
//! `expand.rs`, LABS-805's round-2 split): [`bind_named_path`], with its
//! [`fill_end_node`] chunk hook — one caller, the walk's last-element arm
//! in `expand::match_node`.

use std::sync::Arc;

use relate_core::Value;
use relate_ir::PathPattern;

use crate::overlay::ExecGraph;
use crate::pattern_walk::{bind_reconstructed_path, node_bind_name};
use crate::row::{Row, RowRung};

/// A row's node binding by name — the one read the binder's hooks
/// share; each hook keeps only its own selection guard (the seat name,
/// the last named variable, the next seat's variable plus the id
/// dedupe).
fn row_node<'r>(row: &'r Row, name: &str) -> Option<&'r Arc<relate_core::Node>> {
    match row.get(name) {
        Some(Value::Node(n)) => Some(n),
        _ => None,
    }
}

pub(crate) fn bind_named_path(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    row: &mut Row,
    rung: &RowRung,
) {
    // The classic walker's terminal binder — a thin call over the one
    // orchestration skeleton both walkers share (`bind_reconstructed_path`,
    // pattern_walk/binds.rs) with the classic walker's three deliberate
    // deltas: the first-node read consults the variable or the exec-only
    // `__node0` seat and keeps the binding's own snapshot (no
    // re-materialization — the eval walker's delta re-materializes), the
    // empty-chunk hook rides the fill-end-node addition, and the
    // last-node fallback keeps a named path whose every chunk came back
    // empty carrying its last named node.
    bind_reconstructed_path(
        graph,
        path,
        row,
        rung,
        |row, nodes| {
            // The variable-else-seat selection through the one spelling
            // (`node_bind_name`, the same one the ladder build and the
            // bind arm read): the first node's variable, else the
            // exec-only `__node0` seat on a named path.
            if let Some(n) = node_bind_name(
                &graph.seat_prefixes(),
                &path.nodes[0],
                0,
                path.has_path_name(),
                |name| row_node(row, name),
            )
            .flatten()
            {
                nodes.push((**n).clone());
            }
        },
        |i, row, tail| fill_end_node(path, i, row, tail),
        |row, nodes| {
            if nodes.is_empty() {
                if let Some(n) = path
                    .nodes
                    .last()
                    .and_then(|n| n.variable.as_ref())
                    .and_then(|v| v.with_str(|text| row_node(row, text)))
                {
                    nodes.push((**n).clone());
                }
            }
        },
    );
}

/// The empty-chunk hook [`bind_named_path`] rides: a chunk that matched
/// nothing still contributes its end node to the reconstructed path when
/// the row binds it. Fires per rel of a non-empty compact `rels` view,
/// so `rel_idx + 1` always names a node — the alternation invariant is
/// the compact view's stated contract at `PathPattern`/lowering, not a
/// per-site branch.
fn fill_end_node(
    path: &PathPattern,
    rel_idx: usize,
    row: &Row,
    nodes: &mut Vec<relate_core::Node>,
) {
    let Some(v) = &path.nodes[rel_idx + 1].variable else {
        return;
    };
    if let Some(n) = v.with_str(|text| row_node(row, text)) {
        if nodes.last().map(|x| x.id) != Some(n.id) {
            nodes.push((**n).clone());
        }
    }
}
