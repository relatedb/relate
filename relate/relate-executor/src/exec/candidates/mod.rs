//! The exec engine's node-match residency: the relationship twin of the
//! node id seek ([`rel_seek_expr`], the LABS-161 decode), the node
//! admission ([`node_ok`]), and the shared SEARCH scan contract — index
//! resolution and survivor materialization, the one spelling both SEARCH
//! paths (vector, fulltext) call — which lives here (stellar round 3,
//! std-6), not in either feature module. The node-candidate funnel
//! itself — which index, if any, answers a node pattern's residual
//! predicates, and the label/property scan it falls back to — is not
//! this engine's: both walk engines read it, so it lives at the shared
//! walk tier ([`crate::pattern_walk::node_candidates`]). Relationship
//! expansion lives in `expand`.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Node, NodeId, RelId, Result, Value};
use relate_ir::{BinaryOp, Direction, Expr, NodePattern, RelPattern};
use relate_model::EntityKind;
use relate_names::SmallName;

use crate::eval::{eval, label_expr_matches};
use crate::overlay::ExecGraph;
use crate::pattern_walk::PatternMapRole;
use crate::pattern_walk::{id_fn_conversion, map_from_expr, static_rel_type_ok, IdConversion};
use crate::redo::{IndexDetailSpec, IndexKindSpec};
use crate::row::Row;
use crate::store::RelTypes;

/// The relationship twin of the node ids seek (`crate::pattern_walk`'s
/// `ids_seek_expr`, LABS-161): `id(r) = <expr>`
/// (integer) or `elementId(r) = <expr>` (the `5:relate:r<id>` string
/// form, which self-encodes the RelId) on the relationship variable `var`
/// resolves the RelId directly, so the walk can seek the id-keyed
/// relationship storage instead of walking every node's adjacency. Only
/// equality — an `IN` list stays a walk (the residual re-evaluates the
/// predicate either way, so a declined seek never changes the result).
pub(crate) fn rel_seek_expr(
    var: &SmallName,
    expr: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Option<RelId> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    if !matches!(op, BinaryOp::Eq) {
        return None;
    }
    let to_id: IdConversion = id_fn_conversion(var, left, relate_core::parse_rel_element_id)?;
    let val = eval(graph, row, right, params, cypher_version).ok()?;
    to_id.apply(&val)
}

/// Outcome of the shared LABS-161 seek decode ([`resolve_rel_seek`]),
/// before the caller applies its own anchoring policy.
pub(crate) enum SeekedRel {
    /// No seek applies: the caller enumerates the adjacency.
    Decline,
    /// The resolved rel id cannot match: not visible (`rel_ends` misses),
    /// or rejected by the pattern's static type filter.
    Gone,
    /// The rel id with both raw endpoints; orientation is the caller's.
    Resolved {
        start: NodeId,
        end: NodeId,
        rid: RelId,
    },
}

/// The decode core shared by the walk's first-hop seek (`first_rel_seek`
/// in `expand`) and the one-hop aggregate fold: resolve the row-bound rel
/// or the `elementId(r) = …` / `id(r) = …` equality conjunct to a RelId,
/// check visibility via `rel_ends`, and apply the pattern's static type
/// filter (the adjacency enumeration applies it while scanning, so the
/// seek applies it in place). Declines for a var-length or anonymous rel,
/// a null or non-relationship row binding (the pre-bound reconciliation
/// keeps its existing behavior), or no equality conjunct. The residual
/// re-evaluates the predicate either way, so the decode narrows the
/// search, never the result.
pub(crate) fn resolve_rel_seek(
    graph: &ExecGraph<'_>,
    rp: &RelPattern,
    types: &RelTypes,
    conjuncts: &[Expr],
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> SeekedRel {
    if rp.var_length {
        return SeekedRel::Decline;
    }
    let Some(v) = rp.variable.as_ref() else {
        return SeekedRel::Decline;
    };
    let rid = match row.get_name(v) {
        Some(Value::Relationship(r)) => r.id,
        Some(_) => return SeekedRel::Decline,
        None => match conjuncts
            .iter()
            .find_map(|c| rel_seek_expr(v, c, graph, row, params, cypher_version))
        {
            Some(rid) => rid,
            None => return SeekedRel::Decline,
        },
    };
    let Some((start, end)) = graph.rel_ends(rid) else {
        return SeekedRel::Gone;
    };
    // The caller resolved the pattern's type filter once for the walk
    // (LABS-801); the seek applies it in place of the adjacency
    // enumeration's.
    if !static_rel_type_ok(graph, rid, types) {
        return SeekedRel::Gone;
    }
    SeekedRel::Resolved { start, end, rid }
}

/// The direction-oriented start of a resolved seek rel; `None` on
/// `Direction::Both` — the undirected decline, decided once here: an
/// undirected pattern matches each rel in both orientations, and one
/// anchor would drop the reverse hop, so the scan keeps serving it. The
/// one spelling both seek consumers read (the walk's `first_rel_seek`,
/// the one-hop fold's `fold_rel_seek`).
pub(crate) fn oriented_start(rp: &RelPattern, start: NodeId, end: NodeId) -> Option<NodeId> {
    match rp.direction {
        Direction::Outgoing => Some(start),
        Direction::Incoming => Some(end),
        Direction::Both => None,
    }
}

/// Fetch a candidate the enumeration claimed is present; a miss is an
/// invariant violation, not a skip. One spelling for every read of an
/// enumeration-filtered candidate — the vector and fulltext scans'
/// materialization (`exec/vector.rs`) and the GPM walk's id-slice reads
/// (`exec/gpm/mod.rs`'s `materialize_path` and the selector's rel-type
/// read — every id there was claimed by the walk's neighbor enumeration,
/// which yields only rels that exist, or passed its node visibility
/// read). A caller whose candidate set is NOT
/// enumeration-filtered (the LABS-161 rel seek, which can point at a
/// since-deleted rel) spells its own miss handling — `SeekedRel::Gone`.
pub(crate) fn expect_enumerated_candidate<T>(candidate: Option<T>, id: i64) -> T {
    candidate.unwrap_or_else(|| panic!("enumerated candidate {id} vanished from the read snapshot"))
}

/// The one SEARCH index resolution (stellar round 3, std-6): the schema
/// read, the missing-index policy, and the wrong-kind rejection — one
/// spelling both SEARCH paths (vector, fulltext) call instead of
/// hand-building the errors a second time. The two catalog maps are
/// installed by the same redo op under the same name (apply_redo.rs,
/// theater.rs; drops remove both), so either miss is a missing index.
pub(crate) fn resolve_search_index(
    graph: &ExecGraph<'_>,
    index_name: &str,
    expected_kind: IndexKindSpec,
) -> Result<(IndexDetailSpec, BTreeMap<String, Value>)> {
    let theater = graph.show_schema()?;
    let (detail, options) = match (
        theater.data.index_details.get(index_name),
        theater.data.index_options.get(index_name),
    ) {
        (Some(detail), Some(options)) => (detail.clone(), options.clone()),
        _ => return Err(Error::execution(format!("IndexDoesNotExist: {index_name}"))),
    };
    if detail.kind != expected_kind {
        return Err(Error::execution(format!("WrongIndexType: {index_name}")));
    }
    Ok((detail, options))
}

/// Materialize one scored survivor (stellar round 2, std-6): the id was
/// just read from this same read-only graph, so a failed re-fetch is an
/// invariant violation — a panic, not a silently short result set. The
/// shared scan contract's kind-dispatching composite (stellar round 3,
/// std-6): both SEARCH paths (vector, fulltext) read it from here, not
/// from the vector feature module.
pub(crate) fn materialize_candidate(graph: &ExecGraph<'_>, kind: EntityKind, id: i64) -> Value {
    let candidate = if kind.is_relationship() {
        graph.rel(id).map(Value::from_relationship)
    } else {
        graph.node(id).map(Value::from_node)
    };
    expect_enumerated_candidate(candidate, id)
}

pub(crate) fn node_ok(
    graph: &ExecGraph<'_>,
    id: NodeId,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<bool> {
    // Cypher keeps a deleted-but-bound entity addressable for the rest of
    // the query: when the row already binds this candidate's variable to
    // the node a DELETE removed, the checks below read that binding's
    // tombstone (`refresh_rows` writes it: the same id, no labels, no
    // properties) instead of the store, which no longer has the node. So
    // `MATCH (u) DELETE u WITH u MATCH (u)` still matches, while a labelled
    // re-match fails like it does against the tombstone's empty label set.
    // Nothing else may see a deleted node — a pattern over an unbound
    // variable still matches nothing.
    let snapshot: Option<Arc<Node>> =
        np.variable
            .as_ref()
            .and_then(|v| row.get_name(v))
            .and_then(|value| match value {
                Value::Node(n) if n.id == id && graph.is_deleted_node(id) => Some(Arc::clone(n)),
                _ => None,
            });
    // The tombstone-vs-store fork, decided once per candidate (F-1-8):
    // every read below — label, label list, bare visibility, property —
    // goes through the view's one spelling of it, never four inline
    // matches each deciding visibility differently.
    let view = match snapshot.as_deref() {
        Some(node) => ReadView::Tombstone(node),
        None => ReadView::Store,
    };
    // One visibility read per path, and label checks without
    // materializing the label list (LABS-347, stellar F-1-17/F-1-36).
    match &np.label_expr {
        // The common case: even a plain `MATCH (n:Person)` carries a
        // synthesized static conjunction. Checking each label by name
        // pays one interner lookup per (candidate, label) — no
        // `Vec<String>` materialization, the q19 hot-path cost.
        // A static conjunction implies a non-empty label list at every
        // construction site (the planner's `add_labels` rebuilds
        // `label_expr` as `And(...)` over exactly `n.labels`; nothing
        // builds `And([])`), so the guard needs no labels cross-check —
        // the same single criterion `label_index_seekable` uses.
        Some(expr) if expr.is_static_conjunction() => {
            if !np.labels.iter().all(|want| view.has_label(graph, id, want)) {
                return Ok(false);
            }
        }
        // Genuinely dynamic label expressions need the labels as
        // strings; `node_labels` returning None is this path's
        // visibility read (invisible nodes have no labels).
        Some(expr) => {
            let Some(labels) = view.labels(graph, id) else {
                return Ok(false);
            };
            if !label_expr_matches(graph, row, params, expr, &labels, false, cypher_version)? {
                return Ok(false);
            }
        }
        // No label expression at all: bare visibility.
        None => {
            if !view.visible(graph, id) {
                return Ok(false);
            }
        }
    }
    if let Some(p) = &np.properties {
        let want = map_from_expr(graph, row, p, PatternMapRole::Match, params, cypher_version)?;
        for (k, v) in want {
            // The string-keyed read: `node_prop` runs one interner
            // lookup per (candidate, key) — the same count any wrapper
            // of it pays — and the scratch (uncommitted) map it reads is
            // string-keyed by construction.
            let have = view.prop(graph, id, &k);
            match have {
                Some(have) if have.eq_non_null(&v) => {}
                _ => return Ok(false),
            }
        }
    }
    Ok(true)
}

/// The candidate's one read view (F-1-8): the row's tombstone for a
/// deleted-but-bound node, or the store for every live candidate. The
/// fork `node_ok` once re-decided at four reads — label, label list,
/// bare visibility, property — each spelling visibility differently; it
/// is spelled once here, as this type's reads.
#[derive(Clone, Copy)]
enum ReadView<'a> {
    Tombstone(&'a Node),
    Store,
}

impl ReadView<'_> {
    /// One label check: the tombstone's own string list, or the
    /// interning check — whose `None` (an invisible node) fails the
    /// check exactly like an absent label.
    fn has_label(&self, graph: &ExecGraph<'_>, id: NodeId, want: &SmallName) -> bool {
        match self {
            Self::Tombstone(node) => node.labels.iter().any(|have| want == have.as_str()),
            Self::Store => {
                matches!(
                    want.with_str(|text| graph.node_has_label(id, text)),
                    Some(true)
                )
            }
        }
    }

    /// The whole label list a dynamic expression matches against: the
    /// tombstone's own, or the store's — whose `None` (an invisible
    /// node) the caller reads as "matches nothing".
    fn labels(&self, graph: &ExecGraph<'_>, id: NodeId) -> Option<Vec<String>> {
        match self {
            Self::Tombstone(node) => Some((*node.labels).clone()),
            Self::Store => graph.node_labels(id),
        }
    }

    /// Bare visibility: a bound tombstone stays addressable; a store
    /// candidate must exist.
    fn visible(&self, graph: &ExecGraph<'_>, id: NodeId) -> bool {
        match self {
            Self::Tombstone(_) => true,
            Self::Store => graph.node_exists(id),
        }
    }

    /// One property read: the tombstone's own map, or the store's
    /// string-keyed read.
    fn prop(&self, graph: &ExecGraph<'_>, id: NodeId, key: &str) -> Option<Value> {
        match self {
            Self::Tombstone(node) => node.properties.get(key).cloned(),
            Self::Store => graph.node_prop(id, key),
        }
    }
}
