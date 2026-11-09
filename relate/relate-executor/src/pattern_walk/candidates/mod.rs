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
//! that ranks them, the OR-union pass, the served-set narrowing, and the
//! scan tail.

use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, Result, Value};
use relate_ir::{BinaryOp, Expr, NodePattern};
use relate_names::SmallName;

use crate::eval::expr_vars;
use crate::overlay::{intersect_ids, union_ids, ExecGraph};
use crate::pattern_walk::PatternMapRole;
use crate::pattern_walk::{binding, pattern_props, Binding, NodeBinding};
use crate::row::Row;

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
    cypher_version: Option<u32>,
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
    let Some(seek) = leaf_seek(
        var,
        expr,
        ctx.graph,
        ctx.row,
        ctx.params,
        ctx.cypher_version,
    ) else {
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

pub(crate) fn node_candidates(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    conjuncts: &[Expr],
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
    let candidates = crate::ctx::phase_seek(|| {
        binding_arm(
            binding(np, row),
            np,
            conjuncts,
            graph,
            row,
            params,
            cypher_version,
        )
    })?;
    crate::ctx::count_node_reads(candidates.len() as u64);
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
    conjuncts: &[Expr],
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<NodeId>> {
    // The pattern's property map, lazily and exactly once per arm that
    // needs it (binding the thunk itself derives nothing).
    let props = || {
        pattern_props(
            graph,
            row,
            np.properties.as_ref(),
            PatternMapRole::Match,
            params,
            cypher_version,
        )
    };
    let v = match class {
        Binding::Bound(id) => return Ok(vec![id]),
        Binding::Mismatch => return Ok(Vec::new()),
        Binding::Anonymous => return scan_ids(graph, np, props),
        Binding::Unbound(v) => v,
    };
    if np.force_scan {
        // CIP-79 `USING SCAN`: no seek may answer this variable. The
        // scan policy ([`scan_ids`]) with no property map IS the scan;
        // `node_ok` and the residual conjuncts then apply `=` to every
        // candidate — for a zoned-datetime DST overlap that matches both
        // halves, which an index seek (instant plus zone) does not.
        return scan_ids(graph, np, || Ok(BTreeMap::new()));
    }
    let label_seekable = label_index_seekable(np);
    // Every conjunct that is a seek on this variable, cheapest first.
    // The first one an index serves answers the pattern; an unserved
    // seek (no index, empty CONTAINS needle) yields to the next. The
    // residual still applies every predicate.
    let mut seeks: Vec<Seek> = conjuncts
        .iter()
        .filter_map(|c| leaf_seek(v, c, graph, row, params, cypher_version))
        .collect();
    seeks.sort_by_key(Seek::rank);
    // Map-bridge: the graph's label lookups stay String-keyed; the
    // pattern's labels render once per candidate selection (not per row).
    let labels: Vec<String> = np.label_names().map(|label| label.to_string()).collect();
    for seek in &seeks {
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
        cypher_version,
        labels: &labels,
        label_seekable,
    };
    if let Some(served) = or_union_candidates(v, conjuncts, &ctx) {
        return served.narrowed(graph, &labels, props);
    }
    // No seek served the variable: the scan policy answers it.
    scan_ids(graph, np, props)
}

/// The one scan policy, every site that answers a node pattern with a
/// scan: the label/property lookup a static label conjunction allows
/// (the caller's property map — the binding arm's nameless arm and
/// unserved-seek fallthrough pass the pattern's, the `USING SCAN` arm
/// passes none), then the all-nodes scan when the lookup declines. The
/// property map stays a lazy thunk so a caller whose labels are not
/// seekable never evaluates it — the error surface is unchanged.
fn scan_ids(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    props: impl FnOnce() -> Result<BTreeMap<String, Value>>,
) -> Result<Vec<NodeId>> {
    if label_index_seekable(np) {
        // Same render-once bridge as the seek loop above.
        let labels: Vec<String> = np.label_names().map(|label| label.to_string()).collect();
        if let Some(ids) = graph.lookup_nodes(&labels, &props()?) {
            return Ok(ids);
        }
    }
    Ok(graph.all_node_ids())
}
