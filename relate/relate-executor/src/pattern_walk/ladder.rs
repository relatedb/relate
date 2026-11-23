//! The binding-schema ladder ([`Selection`], [`WalkLadder`]) — the
//! `ladder.rs` arm of the pattern-walk vocabulary; `super`'s module doc
//! maps the whole family.

use relate_core::NodeId;
use relate_ir::PathPattern;

use super::binds::{node_bind_name, rel_bind_name, SeatPrefixes};
use crate::row::{push_bind_name, Row, RowRung};

/// The exec walker's chosen start: the bound pivot (ADR-0350 — the
/// element index and the row-bound node id the backward walker starts
/// from) or the pattern's cheapest free element (ADR-0351). `None` — no
/// selection — is the plain forward walk from node 0: the arm the eval
/// walker always takes and the exec walker takes when neither start
/// mechanism engaged. Produced by the exec walker's start machinery
/// (exec/pivot.rs) and read here by the ladder build ([`WalkLadder`]).
pub(crate) enum Selection {
    /// Pivot at element `k` onto the row-bound node.
    Pivot((usize, NodeId)),
    /// Cheapest free start at element `k`.
    Cheapest(usize),
}

/// A walk's precomputed binding-schema ladder: one rung per binding
/// point the chosen walk arm reaches, indexed by element — `node[i]`
/// the schema the row holds after the walk binds node `i` (whichever
/// direction it reached it from), `rel[j]` the schema after it binds
/// rel `j`, plus `complete`, the finished row's schema (the last
/// element's names plus the pattern's path names). Built once per walk,
/// after the start selection and before the candidate loop it
/// dispatches to; every per-candidate bind stamps a rung
/// ([`Row::stamp`]) instead of rebuilding a schema per row. The one
/// ladder type both walk engines build — the classic walker through its
/// start selections, the eval walker through the forward arm — and
/// total over every pattern shape: the anonymous `__trail{idx}` seat
/// binds its empty list at a zero-hop stop exactly as the named arm
/// does ([`super::binds::bind_trail`]), so every stop's appended names
/// follow from the entry schema and the pattern alone, no shape keeps
/// an insert path, and no caller threads an `Option` around the ladder
/// or a rung.
pub(crate) struct WalkLadder {
    pub(crate) node: Vec<RowRung>,
    pub(crate) rel: Vec<RowRung>,
    pub(crate) complete: RowRung,
}

impl WalkLadder {
    /// Build the ladder for the walk `selection` chose:
    ///
    /// - **Forward** (no selection — normalized onto **Cheapest `0`**,
    ///   the k=0 arm): elements bind left to right from node 0, every
    ///   rung a prefix schema. The eval walker always takes this arm;
    ///   the classic walker takes it when neither start mechanism
    ///   engaged.
    /// - **Pivot `k`**: the backward prefix binds rel `j` + node `j`
    ///   from `k-1` down to 0 (node `k` already row-bound), then the
    ///   forward suffix from rel `k` — the suffix's rungs are the
    ///   forward walk's, because the completed prefix bound exactly the
    ///   prefix elements.
    /// - **Cheapest `k`**: as the pivot arm, with node `k` bound first
    ///   from the entry row — `node[k]` is that start bind's rung.
    ///   `k=0` is the forward walk above.
    ///
    /// `named` is the node-named policy: whether an anonymous node
    /// element's bind carries the [`super::binds::anon_node_name`]
    /// stub. The classic walker passes `path.has_path_name()`; the
    /// eval walker passes `false` — it binds no anonymous
    /// `__node{idx}` stubs, so an anonymous node binds nothing in its
    /// rows. The rel rungs always follow the path's own namedness —
    /// both walkers bind the [`super::binds::anon_rel_name`] stub on
    /// a named path.
    ///
    /// The builder encodes its preconditions: a **Pivot `k == 0`**
    /// normalizes onto the forward/Cheapest(0) arm (a pivot at element
    /// 0 has no prefix to bind backward and node 0 row-bound, which the
    /// k=0 arm's start bind encodes exactly — a row-bound name appends
    /// nothing), and the compact-shape indexing invariant — the rungs
    /// are indexed by element, so `nodes` and `rels` must alternate
    /// node, rel, node — is pinned once at the top, before any arm
    /// indexes into them.
    pub(crate) fn new(
        seats: &SeatPrefixes,
        entry: &Row,
        path: &PathPattern,
        selection: Option<&Selection>,
        named: bool,
    ) -> Self {
        debug_assert_eq!(
            path.nodes.len(),
            path.rels.len() + 1,
            "the compact projections alternate node, rel, node — the shape this ladder indexes"
        );
        let mut names: Vec<String> = entry.keys().cloned().collect();
        let mut node: Vec<RowRung> = Vec::with_capacity(path.nodes.len());
        let mut rel: Vec<RowRung> = Vec::with_capacity(path.rels.len());
        // No selection is the forward walk from node 0, and so is a
        // pivot at element 0 — `Cheapest(0)` is exactly that walk (a
        // cheapest start at element 0 has no prefix to bind backward,
        // and a pivot's row-bound node 0 appends nothing to the entry
        // schema): both normalize onto the k=0 arm here instead of the
        // match duplicating it.
        let forward = Selection::Cheapest(0);
        let selection = match selection.unwrap_or(&forward) {
            Selection::Pivot((0, _)) => &forward,
            other => other,
        };
        match selection {
            Selection::Cheapest(k) => {
                // k=0 is the forward walk: node 0, then rel j + node
                // j+1 onward (the backward prefix binds nothing).
                // Otherwise the cheapest start binds node k from the
                // entry row first; its rung is node[k] (the suffix's
                // match_node rebinds nothing — the start bind owns
                // the name).
                push_bind_name(
                    &mut names,
                    node_bind_name(seats, &path.nodes[*k], *k, named, |n| n.to_string()).as_deref(),
                );
                let start = RowRung::from_names(&names);
                backward_prefix(seats, &mut names, &mut node, &mut rel, path, named, *k);
                node.push(start);
                forward_suffix(seats, &mut names, &mut node, &mut rel, path, named, *k);
            }
            Selection::Pivot((k, _)) => {
                backward_prefix(seats, &mut names, &mut node, &mut rel, path, named, *k);
                // The suffix hand-off: match_node(pivot) rebinds nothing
                // (node k is the row-bound pivot), so its schema is the
                // completed prefix's — node rung 0's content, one shared
                // Arc with the rung the last backward bind stamped.
                node.push(node[0].clone());
                forward_suffix(seats, &mut names, &mut node, &mut rel, path, named, *k);
            }
        }
        // The finished row: the walk's last schema plus the pattern's
        // fresh path names.
        for name in path.path_names() {
            name.with_str(|text| push_bind_name(&mut names, Some(text)));
        }
        let complete = RowRung::from_names(&names);
        Self {
            node,
            rel,
            complete,
        }
    }
}

/// The backward prefix both alternative arms share: rel `j` + node `j`
/// bind from `k-1` down to 0, so the rungs are collected in binding
/// order and reversed into element-index order. On return `names` holds
/// the completed prefix's names — the forward suffix's base.
fn backward_prefix(
    seats: &SeatPrefixes,
    names: &mut Vec<String>,
    node: &mut Vec<RowRung>,
    rel: &mut Vec<RowRung>,
    path: &PathPattern,
    named: bool,
    k: usize,
) {
    let rel_named = path.has_path_name();
    let mut back_rel = Vec::with_capacity(k);
    let mut back_node = Vec::with_capacity(k);
    for j in (0..k).rev() {
        push_bind_name(
            names,
            rel_bind_name(seats, &path.rels[j], j, rel_named, |n| n.to_string()).as_deref(),
        );
        back_rel.push(RowRung::from_names(names));
        push_bind_name(
            names,
            node_bind_name(seats, &path.nodes[j], j, named, |n| n.to_string()).as_deref(),
        );
        back_node.push(RowRung::from_names(names));
    }
    // The binding order was k-1 … 0; the walk indexes rungs by element.
    back_rel.reverse();
    back_node.reverse();
    rel.extend(back_rel);
    node.extend(back_node);
}

/// The forward suffix: rel `k`, node `k+1`, … — the same rungs the
/// forward walk's ladder holds from `k` on (the backward prefix bound
/// exactly the prefix elements, so the accumulation continues). The
/// compact shape's alternation (pinned by the `debug_assert` in
/// [`WalkLadder::new`]) makes `path.nodes[j + 1]` exist for every
/// `j < path.rels.len()`.
fn forward_suffix(
    seats: &SeatPrefixes,
    names: &mut Vec<String>,
    node: &mut Vec<RowRung>,
    rel: &mut Vec<RowRung>,
    path: &PathPattern,
    named: bool,
    k: usize,
) {
    let rel_named = path.has_path_name();
    for j in k..path.rels.len() {
        push_bind_name(
            names,
            rel_bind_name(seats, &path.rels[j], j, rel_named, |n| n.to_string()).as_deref(),
        );
        rel.push(RowRung::from_names(names));
        let np = &path.nodes[j + 1];
        push_bind_name(
            names,
            node_bind_name(seats, np, j + 1, named, |n| n.to_string()).as_deref(),
        );
        node.push(RowRung::from_names(names));
    }
}
