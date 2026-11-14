//! The counted node-candidate funnel and its seek family — the
//! `candidates/` arm of the pattern-walk vocabulary; `super`'s module doc
//! maps the whole tier. Which index, if any, answers a node pattern's
//! residual predicates, and the label/property scan it falls back to:
//! every node-match path of both engines reads this one funnel, so it
//! lives at the walk tier rather than inside the exec engine that first
//! grew it. The tier's own imports stay downward — the expression core,
//! the graph view, the row schema, the PROFILE phase clock — and the
//! residual's scope never travels here: a caller passes the bare
//! conjunct slice (`exec::pushdown::Residual`'s `conjuncts`), keeping
//! exec's residual pushdown in exec.
//!
//! `seek.rs` beside this module keeps the seek vocabulary and its
//! recognizers (the single-index lookups — TEXT contains/prefix, RANGE
//! bounds, SPATIAL, `id()`/`elementId()`); this module keeps the funnel
//! that ranks them, the OR-union pass, the served-set narrowing, the
//! label-disjunction union arm (LABS-1083), and the scan tail.

use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, Result, Value};
use relate_ir::{BinaryOp, DisjunctionArms, Expr, NodePattern};
use relate_names::SmallName;

use crate::eval::expr_vars;
use crate::overlay::{intersect_ids, union_ids, ExecGraph};
use crate::pattern_walk::PatternMapRole;
use crate::pattern_walk::{binding, pattern_props, Binding, NodeBinding};
use crate::row::Row;
use crate::tiered_set::{TieredWordSet, WordHash};

mod seek;
#[cfg(test)]
mod tests;

pub(crate) use seek::{id_fn_conversion, leaf_seek, IdConversion, Seek, ServedIds};

/// The label criterion is static (then `np.labels` is its exact operand
/// list) or dynamic: one spelling, consulted by the seek funnel and the
/// fallback scan alike.
pub(crate) fn label_index_seekable(node: &NodePattern) -> bool {
    node.label_expr
        .as_ref()
        .is_none_or(|expr| expr.is_static_conjunction())
}

/// Whether the funnel's scan answers `node` with its exact matches: a
/// static label conjunction or a disjunction fold both key the label
/// scan on the property map. Any other label expression (a negation, a
/// dynamic label) falls to the all-nodes tail, a superset the per-candidate
/// check filters later — its length says nothing about how many nodes
/// match, so a capped caller must not read it as a match count.
pub(crate) fn scan_answers_exactly(node: &NodePattern) -> bool {
    label_index_seekable(node) || node.static_disjunction_arms().is_some()
}

/// The disjunction fold's union serve (LABS-1083): every arm's label set
/// is a static conjunction, so the seek answers as the union of the
/// per-arm keyed postings — consult each disjunct arm's index in turn,
/// accumulate into one id set deduplicated by `NodeId` through
/// [`union_dedup`] (a node carrying two disjunct labels yields ONE
/// candidate; the `Or` predicate yields each node once). Every arm must
/// pass the seek eligibility and be served by its index — one declined
/// arm (a missing index, ADR-0493's crossover-declined window) declines
/// the whole union, the OR-union pass's every-arm-served rule, and the
/// funnel falls to the next seek or the scan tail. The union is a
/// superset exactly as each arm's serve is (an indexed serve can
/// over-answer); the per-candidate check — the label expression and every
/// predicate re-applied — stays the exactness net, so the union arm
/// bypasses [`ServedIds::narrowed`] whose label/property narrowing would
/// key against the pattern's own (empty) label list and scan every node.
fn seek_union(
    graph: &ExecGraph<'_>,
    seek: &Seek,
    arms: &DisjunctionArms<'_>,
) -> Result<Option<Vec<NodeId>>> {
    let mut served: Vec<Vec<NodeId>> = Vec::with_capacity(arms.len());
    for arm in arms.arms() {
        // Map-bridge: the graph's label lookups stay String-keyed; one
        // arm's labels render once per candidate selection, at the graph
        // call itself — the arms stay the classifier's borrowed views.
        let labels: Vec<String> = arm.map(|label| label.to_string()).collect();
        // Each arm IS a static conjunction by construction of the arm
        // classifier, so the arm's own label criterion passes; the
        // crossover prices the arm's own scan population.
        if !seek.servable(graph, &labels, true) {
            return Ok(None);
        }
        let Some(ids) = seek.serve(graph, &labels) else {
            return Ok(None);
        };
        served.push(ids.into_ids());
    }
    union_dedup(served.into_iter()).map(Some)
}

/// The union of the per-arm served answers, deduplicated by node id and
/// ascending — the one per-arm union spelling every disjunction serve
/// reads (the keyed seek union and the scan tail's per-arm property
/// lookup alike). The dedup set is the tiered word set (ADR-0522): its
/// inline tier holds the common keyed case's handful of ids
/// allocation-free. `NodeId` is `i64` and the set knows raw `u64` words:
/// the bit-pattern cast is injective over the whole `i64` range, so
/// distinct ids stay distinct words whatever their sign.
fn union_dedup(served: impl Iterator<Item = Vec<NodeId>>) -> Result<Vec<NodeId>> {
    let mut seen = TieredWordSet::new(WordHash::Id);
    let mut ids = Vec::new();
    for arm_ids in served {
        for id in arm_ids {
            if seen.insert(id as u64)? {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

/// Outcome of trying to answer one boolean node with indexes.
///
/// `Served` is a candidate list (possibly empty), tagged
/// exact-or-superset by the serving seek ([`ServedIds`]). `Unserved` is
/// a recognized seek the funnel does not serve — its index declined
/// (`None`), or [`Seek::servable`] gated it out (an indexed seek under
/// an unseekable label conjunction, or ADR-0493's crossover-declined
/// window) — which an `AND` may skip and an `OR` may not. `Decline` abandons the whole union: the
/// leaf is not a seek, or it names a variable other than the node being
/// expanded.
enum IndexArm {
    Served(ServedIds),
    Unserved,
    Decline,
}

/// The OR-pass chain's shared context — the evaluation inputs (graph,
/// row, params, cypher version) plus the two pattern facts the chain
/// adds, threading through `or_union_candidates` → `index_arm` →
/// `index_leaf` as one argument (the label fact rides with the rest,
/// no literal standing in for a gate the caller holds).
struct OrCtx<'a> {
    graph: &'a ExecGraph<'a>,
    row: &'a Row,
    params: &'a BTreeMap<String, Value>,
    labels: &'a [String],
    label_seekable: bool,
}

fn or_union_candidates(var: &SmallName, conjuncts: &[Expr], ctx: &OrCtx<'_>) -> Option<ServedIds> {
    // Several served OR conjuncts are AND factors, so their answers
    // intersect — every fold through the one exactness-composition
    // spelling ([`ServedIds::folded`]: a composite is exact only when
    // every arm is). `Iterator::reduce` is the stdlib spelling of the
    // chain: the first served conjunct seeds, every later one folds in
    // conjunct order, so evaluation order and tag composition are
    // exactly the per-conjunct loop's. No closing normalization: every
    // serve ([`Seek::serve`]) and every id-set fold (`union_ids`,
    // `intersect_ids`) already returns sorted, deduped ids.
    conjuncts
        .iter()
        .filter(|c| {
            matches!(
                c,
                Expr::Binary {
                    op: BinaryOp::Or,
                    ..
                }
            )
        })
        .filter_map(|c| match index_arm(var, c, ctx) {
            IndexArm::Served(served) => Some(served),
            IndexArm::Unserved | IndexArm::Decline => None,
        })
        .reduce(|previous, served| previous.folded(served, intersect_ids))
}

fn index_arm(var: &SmallName, expr: &Expr, ctx: &OrCtx<'_>) -> IndexArm {
    match expr {
        Expr::Binary {
            op: BinaryOp::Or,
            left,
            right,
        } => match (index_arm(var, left, ctx), index_arm(var, right, ctx)) {
            (IndexArm::Decline, _) | (_, IndexArm::Decline) => IndexArm::Decline,
            (IndexArm::Served(a), IndexArm::Served(b)) => IndexArm::Served(a.folded(b, union_ids)),
            _ => IndexArm::Unserved,
        },
        Expr::Binary {
            op: BinaryOp::And,
            left,
            right,
        } => match (index_arm(var, left, ctx), index_arm(var, right, ctx)) {
            (IndexArm::Decline, _) | (_, IndexArm::Decline) => IndexArm::Decline,
            (IndexArm::Served(a), IndexArm::Served(b)) => {
                IndexArm::Served(a.folded(b, intersect_ids))
            }
            // The unserved arm's predicate is still pending per
            // candidate, so the composite can only be a superset.
            (IndexArm::Served(a), IndexArm::Unserved)
            | (IndexArm::Unserved, IndexArm::Served(a)) => {
                IndexArm::Served(ServedIds::Superset(a.into_ids()))
            }
            (IndexArm::Unserved, IndexArm::Unserved) => IndexArm::Unserved,
        },
        other => index_leaf(var, other, ctx),
    }
}

fn index_leaf(var: &SmallName, expr: &Expr, ctx: &OrCtx<'_>) -> IndexArm {
    let mut vars = HashSet::new();
    expr_vars(expr, &mut vars);
    // The expr_vars walk's String sink is probed through the free
    // text bridge.
    if vars.len() != 1 || !var.with_str(|text| vars.contains(text)) {
        return IndexArm::Decline;
    }
    // ENDS WITH has no reverse-key index. `string_seek_expr` maps it to
    // `StringSeek::Contains`: every string that ends with the suffix contains
    // it, and the residual applies the real ENDS WITH. No text index, or an
    // empty suffix, is Unserved so an AND sibling can still seed the arm.
    let Some(seek) = leaf_seek(var, expr, ctx.graph, ctx.row, ctx.params) else {
        return IndexArm::Decline;
    };
    // The caller's real label fact, threaded from binding_arm — the
    // one eligibility call, both criteria, no literal standing in for
    // a gate two frames up. A gated window abandons the conjunct to
    // the scan tail exactly like an unserved arm.
    if !seek.servable(ctx.graph, ctx.labels, ctx.label_seekable) {
        return IndexArm::Unserved;
    }
    seek.serve(ctx.graph, ctx.labels)
        .map_or(IndexArm::Unserved, IndexArm::Served)
}

/// Whether the funnel would answer `np` from a point seek (`=` / `IN` /
/// id) one of `conjuncts` places on its variable: the seek recognizes,
/// is eligible (label shape, crossover) and an index (or storage, for ids)
/// actually serves it. The shortest route asks it before listing an
/// endpoint from the residual, so an endpoint no index serves keeps its
/// scan-territory handling instead of being sized from a label scan the
/// residual never narrowed. A label-disjunction fold declines: its union
/// serve may yield to the scan tail, which this probe does not mirror.
pub(crate) fn point_seek_served(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    conjuncts: &[Expr],
) -> bool {
    let Some(v) = np.variable.as_ref() else {
        return false;
    };
    if np.force_scan {
        return false;
    }
    let label_seekable = label_index_seekable(np);
    let disjunction = np.static_disjunction_arms().is_some();
    let labels: Vec<String> = np.label_names().map(|label| label.to_string()).collect();
    // The funnel serves the first seek, cheapest rank first, that an index
    // answers — so the probe mirrors that order and asks whether THAT seek
    // is a point lookup: a comparison window ranked ahead of the point
    // conjunct would be the one materialized.
    let mut seeks: Vec<Seek> = conjuncts
        .iter()
        .filter_map(|c| leaf_seek(v, c, graph, row, params))
        .collect();
    seeks.sort_by_key(Seek::rank);
    seeks
        .iter()
        .filter(|seek| !(disjunction && seek.indexed()))
        .find(|seek| {
            seek.servable(graph, &labels, label_seekable) && seek.serve(graph, &labels).is_some()
        })
        .is_some_and(Seek::is_point)
}

pub(crate) fn node_candidates(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    conjuncts: &[Expr],
) -> Result<Vec<NodeId>> {
    funnel(
        graph,
        np,
        row,
        params,
        Ask {
            conjuncts,
            cap: None,
        },
    )
}

/// What one funnel call asks of its seek strategy: the residual's bare
/// conjuncts, and the most candidates the caller will use (`None`: all).
#[derive(Clone, Copy)]
struct Ask<'a> {
    conjuncts: &'a [Expr],
    cap: Option<usize>,
}

/// A capped funnel answer: the complete candidate list when at most
/// `cap` nodes match, or `Over` — more than `cap` match — with no list,
/// carrying the first count past the cap (`cap + 1`) so a caller that
/// refuses reads it instead of rebuilding it.
pub(crate) enum Capped {
    Within(Vec<NodeId>),
    Over(u64),
}

/// [`node_candidates`] for a caller that only needs the candidates when
/// there are at most `cap` of them (the shortest route's pair bound): the
/// residual scan stops at `cap + 1` matches instead of listing every
/// match first. Counted and phase-stamped exactly like the uncapped
/// funnel — the node reads are the ones the scan returned.
pub(crate) fn node_candidates_capped(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    conjuncts: &[Expr],
    cap: usize,
) -> Result<Capped> {
    let ask = Ask {
        conjuncts,
        cap: Some(cap),
    };
    let ids = funnel(graph, np, row, params, ask)?;
    Ok(if ids.len() > cap {
        Capped::Over(cap as u64 + 1)
    } else {
        Capped::Within(ids)
    })
}

fn funnel(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    ask: Ask<'_>,
) -> Result<Vec<NodeId>> {
    // One node-store read per candidate the match examines (LABS-375):
    // counted HERE, the one funnel every node-match path reads (match_node,
    // the cheapest-start expansion, gpm start nodes, the agg folds, the
    // shortest-route and eval paths), so a scan or index seek reports its
    // reads in PROFILE Db Hits wherever it runs. LABS-448's seek span
    // lives here too, beside the read count it pairs with (the funnel
    // inversion of the per-walker stamping): every path that bumps the
    // `Nodes` counter carries seek time, and no caller re-stamps — the
    // stamp is the funnel's, exactly once. The uncounted funnel itself
    // is [`binding_arm`] — the classification dispatch, the unbound seek
    // strategy, and the scan tail, one exhaustive match off the received
    // classification per the rule its doc comment states.
    let reads_before = crate::ctx::node_reads();
    let candidates =
        crate::ctx::phase_seek(|| binding_arm(binding(np, row), np, ask, graph, row, params))?;
    // A scan counts the nodes it examined as it reads them; an answer
    // served whole from postings has examined exactly what it returns.
    if crate::ctx::node_reads() == reads_before {
        crate::ctx::count_node_reads(candidates.len() as u64);
    }
    Ok(candidates)
}

/// The funnel's binding arm — the classification dispatch, the unbound
/// variable's seek strategy, and the scan tail — and the home of
/// LABS-811's decisive resolution for the pass-the-payload vs
/// derive-inside flip-flop, which the caller cites too: a consumer
/// that needs a fact the classification carries (the
/// [`Unbound`](Binding::Unbound) variable's name, the
/// [`Bound`](Binding::Bound) / [`Mismatch`](Binding::Mismatch)
/// admission) receives the classified [`NodeBinding`] and matches its
/// variants directly — it never re-derives the variable from the
/// pattern (no invariant `expect` restating `classify`'s
/// named-variable contract) and never re-spells the admission table
/// beside the vocabulary. [`Binding::accepts`] stays the canonical
/// predicate for the boolean-only per-candidate consumers (the agg
/// folds, the trail gate); this exhaustive match is the one
/// shape-returning spelling of the same table.
///
/// The arms: a row-bound variable leaves the funnel exactly its one
/// candidate; a null or non-node binding matches nothing (the walker's
/// binding check — `bind_pattern_node` — rejects them all;
/// short-circuit the seek work); a nameless element has no variable
/// for a seek to target and takes the scan policy ([`scan_ids`])
/// directly. Only the `Unbound` arm runs the seek strategy, the
/// variable's name read straight from the payload: `USING SCAN` (the
/// scan IS the answer), the ranked seek loop over the residual's
/// conjuncts (cheapest first, an unserved seek yields to the next),
/// and the OR-union pass (a candidate set only when every arm is
/// served) — every served answer narrowed at the one site,
/// [`ServedIds::narrowed`]; when no seek serves it, the scan policy
/// answers. The
/// pattern-only facts (the label seekability, the lazy property map
/// via [`pattern_props`]) are derived inside from `np` — only the
/// classification is received, never re-derived.
fn binding_arm(
    class: NodeBinding<'_>,
    np: &NodePattern,
    ask: Ask<'_>,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<NodeId>> {
    let Ask { conjuncts, cap } = ask;
    // The pattern's property map, lazily and exactly once per arm that
    // needs it (binding the thunk itself derives nothing).
    let props = || {
        pattern_props(
            graph,
            row,
            np.properties.as_ref(),
            PatternMapRole::Match,
            params,
        )
    };
    let v = match class {
        Binding::Bound(id) => return Ok(vec![id]),
        Binding::Mismatch => return Ok(Vec::new()),
        Binding::Anonymous => return scan_ids(graph, np, props, cap),
        Binding::Unbound(v) => v,
    };
    if np.force_scan {
        // `USING SCAN`: no seek may answer this variable. The
        // scan policy ([`scan_ids`]) with no property map IS the scan;
        // `node_ok` and the residual conjuncts then apply `=` to every
        // candidate — for a zoned-datetime DST overlap that matches both
        // halves, which an index seek (instant plus zone) does not.
        return scan_ids(graph, np, || Ok(BTreeMap::new()), cap);
    }
    let label_seekable = label_index_seekable(np);
    // The disjunction fold's per-arm views (LABS-1083): `None` for every
    // shape the single keyed arm or the scan tail already serves.
    let disjunction_arms = np.static_disjunction_arms();
    // Every conjunct that is a seek on this variable, cheapest first.
    // The first one an index serves answers the pattern; an unserved
    // seek (no index, empty CONTAINS needle) yields to the next. The
    // residual still applies every predicate.
    let mut seeks: Vec<Seek> = conjuncts
        .iter()
        .filter_map(|c| leaf_seek(v, c, graph, row, params))
        .collect();
    seeks.sort_by_key(Seek::rank);
    // Map-bridge: the graph's label lookups stay String-keyed; the
    // pattern's labels render once per candidate selection (not per row).
    let labels: Vec<String> = np.label_names().map(|label| label.to_string()).collect();
    for seek in &seeks {
        // The disjunction fold: an indexed seek serves as the union of
        // the per-arm postings. The ids seek never takes the union —
        // it is label-blind (its serve never reads the labels), so the
        // single serve below answers it unchanged.
        if let Some(arms) = &disjunction_arms {
            if seek.indexed() {
                if let Some(ids) = seek_union(graph, seek, arms)? {
                    return Ok(ids);
                }
                continue;
            }
        }
        // The one per-seek eligibility call: the label criterion and
        // ADR-0493's crossover, folded into `servable`.
        if !seek.servable(graph, &labels, label_seekable) {
            continue;
        }
        if let Some(served) = seek.serve(graph, &labels) {
            return served.narrowed(graph, &labels, props);
        }
    }
    // An OR conjunct is a candidate set only when every arm is served.
    // One declined arm (missing index, empty CONTAINS needle, NOT, XOR,
    // or a leaf that is not exactly this variable) abandons that
    // conjunct — including, structurally, an unseekable label
    // conjunction: `servable` declines every indexed seek under a
    // false label fact, the same call the ranked loop above makes.
    // ENDS WITH is served by a text index as a contains-superset.
    // Several served OR conjuncts are AND factors, so their id lists
    // are intersected — the composite tagged exact only when every
    // served conjunct is.
    let ctx = OrCtx {
        graph,
        row,
        params,
        labels: &labels,
        label_seekable,
    };
    if let Some(served) = or_union_candidates(v, conjuncts, &ctx) {
        return served.narrowed(graph, &labels, props);
    }
    // No seek served the variable: the scan policy answers it.
    scan_ids(graph, np, props, cap)
}

/// The one scan policy, every site that answers a node pattern with a
/// scan: the label/property lookup a static label conjunction allows
/// (the caller's property map — the binding arm's nameless arm and
/// unserved-seek fallthrough pass the pattern's, the `USING SCAN` arm
/// passes none), then the all-nodes scan when the lookup declines. The
/// property map stays a lazy thunk so a caller whose labels are not
/// seekable never evaluates it — the error surface is unchanged. `cap`
/// is [`ExecGraph::lookup_nodes_upto`]'s: a residual scan may stop at
/// `cap + 1` matches. The disjunction fold's shapes (LABS-1083) answer
/// through their own helper ([`scan_disjunction_arms`]), never a
/// whole-store scan of their own spelling.
fn scan_ids(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    props: impl FnOnce() -> Result<BTreeMap<String, Value>>,
    cap: Option<usize>,
) -> Result<Vec<NodeId>> {
    // The two label shapes are exclusive — a static conjunction never
    // carries the `Or` the disjunction arms read, and a disjunction is
    // not a static conjunction — so the `FnOnce` map evaluates at most
    // once, on the path its pattern's shape takes.
    if label_index_seekable(np) {
        // Same render-once bridge as the seek loop above.
        let labels: Vec<String> = np.label_names().map(|label| label.to_string()).collect();
        if let Some(ids) = graph.lookup_nodes_upto(&labels, &props()?, cap) {
            return Ok(ids);
        }
        // The lookup's one decline is the both-empty shape (no labels, no
        // equalities) — a pattern that carries no disjunction arms either,
        // so the all-nodes scan below answers it.
    } else if let Some(arms) = np.static_disjunction_arms() {
        if let Some(ids) = scan_disjunction_arms(graph, &arms, &props()?, cap)? {
            return Ok(ids);
        }
    }
    Ok(graph.all_node_ids())
}

/// The disjunction fold's scan answer (LABS-1083) — the one spelling of
/// the shape's scan policy, read by [`scan_ids`]'s dispatch: a pattern
/// with property equalities answers as the union of the per-arm
/// label/property lookups (each arm's `(label, property)` index
/// consulted with the map's folded value, the per-arm analogue of the
/// conjunction path's one `lookup_nodes` call; an arm whose pair no
/// index serves falls to that arm's label scan inside the lookup,
/// exactly the single-label path's decline — and the map evaluates at
/// the funnel for these shapes, the single-label seekable path's own
/// policy: the same typed [`pattern_props`] error, raised before the
/// first candidate instead of at it). A pattern without equalities
/// (`And([C, Or([A, B])])`, no map) scans the conjunction base — the
/// pre-fold `:C` label scan, the `Or` arms left to the per-candidate
/// check. `Ok(None)` is the all-nodes fallthrough: a bare `Or` over no
/// conjunction labels, whose pre-fold form scanned the whole store.
/// `cap` rides every lookup — [`ExecGraph::lookup_nodes_upto`]'s
/// contract, per arm and per base scan alike.
fn scan_disjunction_arms(
    graph: &ExecGraph<'_>,
    arms: &DisjunctionArms<'_>,
    props: &BTreeMap<String, Value>,
    cap: Option<usize>,
) -> Result<Option<Vec<NodeId>>> {
    if !props.is_empty() {
        // The per-arm serve of the map's equalities: one arm's labels
        // render once per arm (the map-bridge), each arm's lookup
        // always serving — an arm's labels are non-empty, and a
        // non-empty seek is the lookup's one `Some` shape.
        return union_dedup(arms.arms().map(|arm| {
            let labels: Vec<String> = arm.map(|label| label.to_string()).collect();
            graph
                .lookup_nodes_upto(&labels, props, cap)
                .expect("an arm's labels are non-empty: lookup_nodes always serves")
        }))
        .map(Some);
    }
    // No equalities to key on: the fold's And-with-arms shape scans
    // its conjunction base (the pre-fold label scan), the `Or` arms
    // filtering per candidate — the base labels are non-empty, so
    // the lookup's one decline (the both-empty shape) cannot fire:
    // the same always-served expect the per-arm serve above states.
    if !arms.base().is_empty() {
        let labels: Vec<String> = arms.base().iter().map(|label| label.to_string()).collect();
        return Ok(Some(graph.lookup_nodes_upto(&labels, props, cap).expect(
            "the base labels are non-empty: lookup_nodes always serves",
        )));
    }
    Ok(None)
}
