//! The exec engine's node-match residency: the relationship twin of the
//! node id seek ([`rel_seek_expr`], the LABS-161 decode), the node
//! admission ([`node_ok`]), and the shared SEARCH scan contract — index
//! resolution, survivor materialization, the ranked ordering and the
//! bounded [`TopWindow`], the one spelling every ranked scan (vector,
//! fulltext, WRRF fusion) calls — which lives here (stellar round 3,
//! std-6), not in either feature module. The node-candidate funnel
//! itself — which index, if any, answers a node pattern's residual
//! predicates, and the label/property scan it falls back to — is not
//! this engine's: both walk engines read it, so it lives at the shared
//! walk tier ([`crate::pattern_walk::node_candidates`]). Relationship
//! expansion lives in `expand`.

use std::cmp::Ordering as CmpOrdering;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::sync::Arc;

use relate_core::{Error, Node, NodeId, PropertyMap, RelId, Result, Value};
use relate_ir::{BinaryOp, Direction, Expr, NodePattern, RelPattern};
use relate_model::EntityKind;
use relate_names::{NamedToken, SmallName, Token};

use crate::eval::{eval, label_expr_matches};
use crate::overlay::ExecGraph;
use crate::pattern_walk::{
    id_fn_conversion, pattern_match_props, static_rel_type_ok, IdConversion,
};
use crate::redo::{IndexDetailSpec, IndexKindSpec};
use crate::row::Row;
use crate::store::RelTypes;

/// The relationship twin of the node ids seek (`crate::pattern_walk`'s
/// `ids_seek_expr`, LABS-161): `id(r) = <expr>`
/// (integer) or `elementId(r) = <expr>` (the `5:relate-neo4j:r<id>` string
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

/// One FULLTEXT/VECTOR index's entity population: every live entity the
/// index covers as `(id, properties)` — the population half of the shared
/// scan contract (the vector and fulltext scans both read it). It carries
/// the two per-kind enumeration policies:
///
/// * node: the exact per-label enumeration (`live_node_ids_with_label`;
///   LABS-442 — a sparse label skips the other labels' rows and lookups
///   instead of sweeping every id and label-checking each record). The
///   multi-label index is the per-label union; no labels enumerates
///   nothing, as a sweep's label check did.
/// * relationship: the sweep, because the base has no relationship-by-type
///   postings (`live_rel_ids_of_type` is itself an O(R) walk, so a per-type
///   enumeration would pay k whole-table walks). The type filter runs
///   before the record clone.
///
/// Both arms hand back an enumerated record via
/// [`expect_enumerated_candidate`] — the type read included — so a miss
/// anywhere is a catalog/visibility invariant violation, never a silently
/// dropped candidate.
pub(crate) fn index_population(
    graph: &ExecGraph<'_>,
    detail: &IndexDetailSpec,
) -> Vec<(i64, Arc<PropertyMap>)> {
    if detail.relationship {
        graph
            .all_rel_ids()
            .into_iter()
            .filter_map(|id| {
                let rel_type = expect_enumerated_candidate(graph.rel_type(id), id);
                if !detail.labels.iter().any(|label| label == &rel_type) {
                    return None;
                }
                let rel = expect_enumerated_candidate(graph.rel(id), id);
                Some((id, Arc::clone(&rel.properties)))
            })
            .collect()
    } else {
        let mut ids: Vec<NodeId> = detail
            .labels
            .iter()
            .flat_map(|label| graph.live_node_ids_with_label(label))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .map(|id| {
                let node = expect_enumerated_candidate(graph.node(id), id);
                (id, Arc::clone(&node.properties))
            })
            .collect()
    }
}

/// The one canonical CIP-225 candidate ordering: `a` is `Greater` (better)
/// than `b` when its score is higher, or the scores tie and its element id
/// is lower. Scores order by `f64::total_cmp` — the crate's canonical
/// float order (the aggregate sort's), total and transitive where
/// `partial_cmp` + tie is not (a pathological NaN score now orders above
/// every finite one instead of tying intransitively with each). Shared by
/// every ranked scan (vector, fulltext, WRRF fusion).
pub(crate) fn ranked_better_cmp(score_a: f64, id_a: i64, score_b: f64, id_b: i64) -> CmpOrdering {
    score_a.total_cmp(&score_b).then_with(|| id_b.cmp(&id_a))
}

/// A scan candidate ordered by [`ranked_better_cmp`]: `Greater` is better.
/// `PartialEq` agrees with `Ord` by construction (stellar round 1, std-5):
/// the derived-eq disagreement on NaN ties is gone.
#[derive(Clone, Copy)]
struct Ranked {
    score: f64,
    id: i64,
}

// Total under `ranked_better_cmp` (`total_cmp` scores), so `Ord` applies
// even though the bare `f64` score is not `Eq`.
impl Eq for Ranked {}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        ranked_better_cmp(self.score, self.id, other.score, other.id)
    }
}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

/// ADR-0256: the bounded top-k collector for the indexed scans — the one
/// CIP-225 result window (score descending, element id ascending,
/// `SKIP`/`LIMIT`). Retains only the best `offset + limit` candidates —
/// worst at the heap root — so the scan costs `O(n log window)` with no
/// full-length collect, and materializes the element `Value` only for the
/// survivors. Owns `(offset, limit)` itself so no caller can let the
/// retained window and the windowed rows drift (stellar round 1, std-5).
pub(crate) struct TopWindow {
    worst_first: BinaryHeap<Reverse<Ranked>>,
    offset: usize,
    limit: usize,
    /// `offset + limit`, computed once: the heap's retention bound and the
    /// per-push comparison, never re-derived on the scan's hot path.
    window: usize,
}

impl TopWindow {
    pub(crate) fn new(offset: usize, limit: usize) -> Self {
        // `offset + limit`, computed once here — the only place the
        // retention bound is derived.
        let window = offset.saturating_add(limit);
        Self {
            // The heap grows on demand; the clamp only avoids reserving an
            // unbounded window's capacity up front.
            worst_first: BinaryHeap::with_capacity(window.min(1024)),
            offset,
            limit,
            window,
        }
    }

    /// The whole ranking, unbounded — the `db.index.fulltext.query*`
    /// procedures' scan, which wants every hit rather than a page. Keeping
    /// this beside [`Self::new`] means the unbounded intent is stated here,
    /// not as a `usize::MAX` sentinel at the call site.
    pub(crate) fn ranking_all() -> Self {
        Self::new(0, usize::MAX)
    }

    /// Retain `candidate` if it beats the current worst retained entry.
    /// Equal (score, id) pairs never displace each other; ids are unique per
    /// candidate so an equal-ranked push is always the same candidate.
    pub(crate) fn push(&mut self, score: f64, id: i64) {
        if self.window == 0 {
            return;
        }
        let candidate = Ranked { score, id };
        if self.worst_first.len() < self.window {
            self.worst_first.push(Reverse(candidate));
        } else {
            // A retained entry exists exactly when the window is full.
            let mut worst = self
                .worst_first
                .peek_mut()
                .expect("a full window has a worst entry");
            if candidate > worst.0 {
                *worst = Reverse(candidate);
            }
        }
    }

    /// The retained candidates best-first, windowed by the constructor's
    /// `offset`/`limit` — the heap emits best-first already
    /// (`Reverse<Ranked>` ascending is `Ranked` best-first).
    pub(crate) fn into_sorted_rows(self) -> Vec<(i64, f64)> {
        self.worst_first
            .into_sorted_vec()
            .into_iter()
            .map(|Reverse(r)| (r.id, r.score))
            .skip(self.offset)
            .take(self.limit)
            .collect()
    }
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
            if !np
                .labels
                .iter()
                .all(|(want, token)| view.has_label(graph, id, want, *token))
            {
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
    if np.properties.is_some() {
        let want = pattern_match_props(
            graph,
            row,
            np.properties.as_ref(),
            &np.prop_keys,
            params,
            cypher_version,
        )?;
        for (key, v) in want {
            match view.prop(graph, id, &key) {
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
    fn has_label(&self, graph: &ExecGraph<'_>, id: NodeId, want: &SmallName, token: Token) -> bool {
        match self {
            Self::Tombstone(node) => node.labels.iter().any(|have| want == have.as_str()),
            // Plan-resolved ids skip the lookup; an unresolved plan keeps the
            // string check. A name the store had not interned at planning
            // fails without a store read unless a write overlay may carry
            // it (its labels stay strings until commit).
            Self::Store => {
                let seen = want.with_str(|text| match token {
                    Token::Id(tid) => graph.node_has_label_id(id, tid.0, text),
                    Token::Unresolved => graph.node_has_label(id, text),
                    Token::Absent => graph.node_has_uninterned_label(id, text),
                });
                seen == Some(true)
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
    /// plan-resolved read.
    fn prop(&self, graph: &ExecGraph<'_>, id: NodeId, key: &NamedToken) -> Option<Value> {
        match self {
            Self::Tombstone(node) => key.0.with_str(|text| node.properties.get(text).cloned()),
            Self::Store => graph.node_prop_resolved(id, key),
        }
    }
}
