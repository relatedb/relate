//! The LABS-161 first-hop relationship seek (extracted from `expand.rs`,
//! LABS-805's round-2 split): the `RelSeek` vocabulary and the
//! [`first_rel_seek`] policy — how an `elementId(r)`/`id(r)` equality (or
//! a row-bound `r`) on the first rel's variable narrows the walk's first
//! step. The walk itself stays in [`super::expand`]; this module is the
//! seek's self-contained decode, beside its twin's home (the node id
//! seek's decode core lives in `exec/candidates`).

use std::collections::BTreeMap;

use relate_core::{NodeId, RelId, Value};
use relate_ir::PathPattern;

use crate::overlay::ExecGraph;
use crate::pattern_walk::{binding, Binding};
use crate::row::Row;
use crate::store::RelTypes;

use super::candidates::{oriented_start, resolve_rel_seek, SeekedRel};
use super::pushdown::Residual;

/// The outcome of the LABS-161 first-hop relationship seek: how an
/// `elementId(r) = …` / `id(r) = …` equality (or a row-bound `r`) on the
/// first rel's variable narrows the walk's first step. The walk anchors
/// through [`first_rel_seek`]; the one-hop aggregate fold decodes the
/// shared [`SeekedRel`] into the same vocabulary, so each caller's
/// gone-conditions are explicit at its own boundary. The four outcomes
/// are one flat vocabulary — the fold's `FoldSeek` shape — so no
/// consumer nests a `Some`/`None` decode over the variant match.
pub(crate) enum RelSeek {
    /// No seek applies: no first rel, the decode declined, the first
    /// node's binding mismatched (the walk answers zero rows without
    /// the seek), or the pattern is undirected with a free first node.
    /// The adjacency scan runs.
    Decline,
    /// Seed `match_node` at this endpoint of the rel (the first node
    /// pattern is free) and force the first hop to the rel.
    Anchored(NodeId, RelId),
    /// The first node keeps its own candidacy (it is row-bound); only
    /// the first hop is forced to the rel.
    Forced(RelId),
    /// The resolved rel cannot match: not visible (deleted or never
    /// existed), rejected by the pattern's static type filter, or —
    /// through the fold's decode — not incident to the row-bound start
    /// node. The pattern matches nothing.
    Gone,
}

/// LABS-161, the rel twin of the node id seek: relate element ids are
/// transparent (`5:rl-id:r<id>` self-encodes the RelId), so
/// `elementId(r) = <expr>` / `id(r) = <expr>` on the first rel's variable
/// — or `r` already bound in the row — resolves the RelId with a parse,
/// no lookup structure and nothing to invalidate. The walk then seeds
/// from that rel's endpoint ([`RelSeek::Anchored`], the analog of
/// `forced: Option<NodeId>` in `expand::match_node`) instead of scanning
/// every node and walking each adjacency per row. The decode itself is
/// [`resolve_rel_seek`]; the walk's own policy is the first-node binding:
/// a row-bound node keeps its candidacy and only the hop is forced, and
/// an undirected pattern with a free first node declines — the one
/// [`oriented_start`] spelling.
pub(crate) fn first_rel_seek(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    types: &RelTypes,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    residual: &Residual<'_>,
) -> RelSeek {
    let Some(rp) = path.rels.first() else {
        return RelSeek::Decline;
    };
    let (start, end, rid) = match resolve_rel_seek(
        graph,
        rp,
        types,
        residual.conjuncts,
        row,
        params,
        cypher_version,
    ) {
        SeekedRel::Decline => return RelSeek::Decline,
        SeekedRel::Gone => return RelSeek::Gone,
        SeekedRel::Resolved { start, end, rid } => (start, end, rid),
    };
    match binding(&path.nodes[0], row) {
        Binding::Bound(_) => RelSeek::Forced(rid),
        // A mismatching binding matches nothing either way; the walk
        // answers zero rows without the seek.
        Binding::Mismatch => RelSeek::Decline,
        Binding::Unbound(_) | Binding::Anonymous => match oriented_start(rp, start, end) {
            Some(anchor) => RelSeek::Anchored(anchor, rid),
            None => RelSeek::Decline,
        },
    }
}
