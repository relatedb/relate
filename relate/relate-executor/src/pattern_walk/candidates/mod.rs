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
fn scan_answers_exactly(node: &NodePattern) -> bool {
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
    let mut per_arm: Vec<Vec<NodeId>> = Vec::with_capacity(arms.len());
    for arm in arms.arms() {
        // Map-bridge: the graph's label lookups stay String-keyed; one
        // arm's labels render once per candidate selection, at the graph
        // call itself — the arms stay the classifier's borrowed views.
        let labels: Vec<String> = arm.map(|label| label.to_string()).collect();
        // Each arm IS a static conjunction by construction of the arm
        // classifier, so the arm's own label criterion passes; the
        // crossover prices the arm's own scan population. The union is
        // all-or-nothing: one unserved arm declines the whole conjunct.
        let Some(ids) = served(seek, graph, &labels, true) else {
            return Ok(None);
        };
        per_arm.push(ids.into_ids());
    }
    union_dedup(per_arm.into_iter()).map(Some)
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
            if seen.insert(id as u64, "union")? {
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

/// The plain serve path's one spelling: an index answers this seek when
/// [`Seek::servable`] (the label criterion and the crossover, the caller's
/// real facts — no literal standing in for a gate two frames up) and
/// [`Seek::serve`] agree. A gated or unserved window is `None`.
fn served(
    seek: &Seek,
    graph: &ExecGraph<'_>,
    labels: &[String],
    label_seekable: bool,
) -> Option<ServedIds> {
    if !seek.servable(graph, labels, label_seekable) {
        return None;
    }
    seek.serve(graph, labels)
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
    served(&seek, ctx.graph, ctx.labels, ctx.label_seekable)
        .map_or(IndexArm::Unserved, IndexArm::Served)
}

/// Every residual conjunct that is a seek on `v`, cheapest rank first —
/// the one spelling of the funnel's seek collection.
fn ranked_seeks(
    v: &SmallName,
    conjuncts: &[Expr],
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Vec<Seek> {
    let mut seeks: Vec<Seek> = conjuncts
        .iter()
        .filter_map(|c| leaf_seek(v, c, graph, row, params))
        .collect();
    seeks.sort_by_key(Seek::rank);
    seeks
}

/// The pattern's labels in the graph lookups' String key space — rendered
/// once per candidate selection, never per row.
fn pattern_labels(np: &NodePattern) -> Vec<String> {
    np.label_names().map(|label| label.to_string()).collect()
}

/// One node pattern's seek-side preparation — the single derivation both
/// funnel entries read ([`binding_arm`]'s seek strategy and the point
/// ask's [`point_arm`]): the ranked seeks over the residual's conjuncts
/// on `v`, the eligibility inputs, and the label-disjunction fold's arm
/// views (LABS-1083; `None` for every shape the single keyed arm or the
/// scan tail already serves).
struct Prepared<'a> {
    seeks: Vec<Seek>,
    /// Map-bridge: the graph's label lookups stay String-keyed.
    labels: Vec<String>,
    label_seekable: bool,
    arms: Option<DisjunctionArms<'a>>,
}

/// The funnel's seek-side preparation, or `None` under `USING SCAN`: no
/// seek may answer the variable then — the funnel's own arm scans it — so
/// there is nothing to prepare, and no caller re-reads the fact.
fn prepared<'a>(
    v: &SmallName,
    np: &'a NodePattern,
    conjuncts: &[Expr],
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Option<Prepared<'a>> {
    if np.force_scan {
        return None;
    }
    Some(Prepared {
        seeks: ranked_seeks(v, conjuncts, graph, row, params),
        labels: pattern_labels(np),
        label_seekable: label_index_seekable(np),
        arms: np.static_disjunction_arms(),
    })
}

/// Which seek answers a variable's pattern: the disjunction fold's union
/// ids, or one seek's own served ids.
enum Answered {
    Union(Vec<NodeId>),
    Serve { ids: ServedIds },
}

impl Prepared<'_> {
    /// The fold's one rule (LABS-1083), the one spelling every walk over
    /// the prepared seeks reads: under a label-disjunction fold an indexed
    /// seek serves only as the union of the per-arm postings — the arms it
    /// serves from. `None` is the plain path: a pattern with no fold, or a
    /// label-blind ids seek (its serve never reads the labels, so it
    /// answers unchanged, which the `indexed` test already states).
    fn union_step(&self, seek: &Seek) -> Option<&DisjunctionArms<'_>> {
        self.arms.as_ref().filter(|_| seek.indexed())
    }

    /// The funnel's "which seek answers" walk over the prepared seeks, in
    /// rank order: the first seek an index serves. A union-only seek
    /// answers as the union of the per-arm postings; every other seek
    /// answers through [`served`].
    fn first_answered(&self, graph: &ExecGraph<'_>) -> Result<Option<Answered>> {
        for seek in &self.seeks {
            if let Some(arms) = self.union_step(seek) {
                if let Some(ids) = seek_union(graph, seek, arms)? {
                    return Ok(Some(Answered::Union(ids)));
                }
                continue;
            }
            if let Some(ids) = self.served_plain(seek, graph) {
                return Ok(Some(Answered::Serve { ids }));
            }
        }
        Ok(None)
    }

    /// One non-union seek's plain answer, the rule both walks read: a
    /// union-only seek never answers this way (`None` — its answer is the
    /// per-arm union, [`Self::first_answered`]'s own arm), every other
    /// seek answers through [`served`] when an index serves it.
    fn served_plain(&self, seek: &Seek, graph: &ExecGraph<'_>) -> Option<ServedIds> {
        if self.union_step(seek).is_some() {
            return None;
        }
        served(seek, graph, &self.labels, self.label_seekable)
    }

    /// The point-only walk ([`point_arm`]'s one question): the first seek
    /// an index serves, skipping the union-only ones — a point ask cannot
    /// use a union answer and pays no per-arm index lookups for one —
    /// answers, but only when it is itself a point lookup
    /// ([`Seek::is_point`]); the first served seek decides, so any other
    /// served seek answers the pattern without a bounded list and the ask
    /// declines. `None`: no point seek serves the pattern.
    fn first_point(&self, graph: &ExecGraph<'_>) -> Option<ServedIds> {
        for seek in &self.seeks {
            let Some(ids) = self.served_plain(seek, graph) else {
                continue;
            };
            return seek.is_point().then_some(ids);
        }
        None
    }
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
/// conjuncts and the most candidates the caller will use (`None`: all).
/// Whether the ask is point-only is not a knob — it is the other entry
/// ([`point_candidates_capped`]'s [`point_arm`]): the plain funnel has no
/// point mode, and no arm of it declines.
#[derive(Clone, Copy)]
struct Ask<'a> {
    conjuncts: &'a [Expr],
    cap: Option<usize>,
}

/// A capped funnel answer: the complete candidate list when at most
/// `cap` nodes match, or `Over` — more than `cap` match — with no list,
/// carrying the first count past the cap (`cap + 1`) so a caller that
/// refuses reads it instead of rebuilding it. Both capped entries answer
/// `Option<Capped>`: `None` is "no answer a caller may size a side by" —
/// a shape whose scan is a superset of its matches (the plain funnel
/// reports it before scanning), or a point ask no point seek served.
pub(crate) enum Capped {
    Within(Vec<NodeId>),
    Over(u64),
}

/// [`node_candidates`] for a caller that only needs the candidates when
/// there are at most `cap` of them (the shortest route's pair bound and
/// full listing): the residual scan stops at `cap + 1` matches instead of
/// listing every match first. Counted and phase-stamped exactly like the
/// uncapped funnel — the node reads are the ones the scan returned.
pub(crate) fn node_candidates_capped(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    conjuncts: &[Expr],
    cap: usize,
) -> Result<Option<Capped>> {
    // A shape whose scan is a superset of its matches answers no count a
    // caller may trust ([`scan_answers_exactly`]) — reported before the
    // scan runs, so no caller has to know how the funnel would answer.
    if !scan_answers_exactly(np) {
        return Ok(None);
    }
    Ok(Some(cap_answer(
        funnel(
            graph,
            np,
            row,
            params,
            Ask {
                conjuncts,
                cap: Some(cap),
            },
        )?,
        cap,
    )))
}

/// [`node_candidates_capped`] as a point-only ask: a caller that sizes a
/// candidate list from the answer (the shortest route's endpoint seek)
/// wants it only when a point lookup — `id(n) = …`, `n IN [nodes…]`, a
/// property `=` / `IN` — served it: a point lookup's answer is bounded by
/// the matches it names, where a comparison window, a text scan, a
/// spatial region or a label scan is not. `Ok(None)`: no point seek
/// served the pattern — no list, and no scan ran.
pub(crate) fn point_candidates_capped(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    conjuncts: &[Expr],
    cap: usize,
) -> Result<Option<Capped>> {
    point_funnel(graph, np, row, params, conjuncts)
        .map(|answer| answer.map(|ids| cap_answer(ids, cap)))
}

/// The capped answer of a complete candidate list: `Over` — with the
/// first count past the cap, `cap + 1` — once more than `cap` nodes
/// match.
fn cap_answer(ids: Vec<NodeId>, cap: usize) -> Capped {
    if ids.len() > cap {
        Capped::Over(cap as u64 + 1)
    } else {
        Capped::Within(ids)
    }
}

/// The counting every funnel entry wraps its arm in (LABS-375): one
/// node-store read per candidate the match examines, counted HERE, the
/// one funnel every node-match path reads (match_node, the cheapest-start
/// expansion, gpm start nodes, the agg folds, the shortest-route and eval
/// paths), so a scan or index seek reports its reads in PROFILE Db Hits
/// wherever it runs. LABS-448's seek span lives here too, beside the read
/// count it pairs with (the funnel inversion of the per-walker stamping):
/// every path that bumps the `Nodes` counter carries seek time, and no
/// caller re-stamps — the stamp is the funnel's, exactly once. Each entry
/// names its own answered length: the plain funnel's list (always
/// answered — every classification arm ends in a list, the scan tail) and
/// the point ask's `Option` (a decline counts `0`).
fn counted<T>(arm: impl FnOnce() -> Result<T>, answered: impl Fn(&T) -> usize) -> Result<T> {
    let reads_before = crate::ctx::node_reads();
    let candidates = crate::ctx::phase_seek(arm)?;
    // A scan counts the nodes it examined as it reads them; an answer
    // served whole from postings has examined exactly what it returns.
    // A decline (the point ask's `None`) counts nothing — its zero.
    if crate::ctx::node_reads() == reads_before {
        crate::ctx::count_node_reads(answered(&candidates) as u64);
    }
    Ok(candidates)
}

fn funnel(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    ask: Ask<'_>,
) -> Result<Vec<NodeId>> {
    counted(
        || binding_arm(binding(np, row), np, ask, graph, row, params),
        |candidates: &Vec<NodeId>| candidates.len(),
    )
}

/// The point ask's entry ([`point_candidates_capped`]'s arm): the same
/// counting wrapper around [`point_arm`], whose `None` is the ask's
/// decline.
fn point_funnel(
    graph: &ExecGraph<'_>,
    np: &NodePattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
    conjuncts: &[Expr],
) -> Result<Option<Vec<NodeId>>> {
    counted(
        || point_arm(binding(np, row), np, conjuncts, graph, row, params),
        |answer: &Option<Vec<NodeId>>| answer.as_ref().map_or(0, Vec::len),
    )
}

/// The classification's one answer table, the shape every consumer of a
/// [`Binding`] starts from ([`binding_arm`] and [`point_arm`], each
/// applying its own policy to the `Scan` state; the shortest route's
/// endpoint probe, which maps the answers to its own sides): a row-bound
/// variable is its one candidate, a null or non-node binding matches
/// nothing, a nameless element has no variable a seek targets (scan
/// territory), and a named unbound variable is the seek strategy's input.
pub(crate) enum Admitted<'a> {
    /// A row-bound variable: exactly one candidate, no list to build.
    One(NodeId),
    /// A null or non-node binding: no candidate at all.
    Empty,
    /// A nameless element: only a scan answers it.
    Scan,
    /// A named unbound variable: the seek strategy's input.
    Seek(&'a SmallName),
}

pub(crate) fn admitted<'a>(class: NodeBinding<'a>) -> Admitted<'a> {
    match class {
        Binding::Bound(id) => Admitted::One(id),
        Binding::Mismatch => Admitted::Empty,
        Binding::Anonymous => Admitted::Scan,
        Binding::Unbound(v) => Admitted::Seek(v),
    }
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
/// folds, the trail gate); [`admitted`] is the one shape-returning
/// spelling of the same table, and this entry's match is its policy over
/// that table.
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
/// served) — every served answer narrowed through the one site both
/// entries share ([`ServedIds::narrowed`]); when no seek
/// serves it, the scan policy answers. The
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
    let props = map_thunk(graph, row, np, params);
    let v = match admitted(class) {
        Admitted::One(id) => return Ok(vec![id]),
        Admitted::Empty => return Ok(Vec::new()),
        // A nameless element has no variable a seek targets: only a scan
        // answers it.
        Admitted::Scan => return scan_ids(graph, np, props, cap),
        Admitted::Seek(v) => v,
    };
    // Every conjunct that is a seek on this variable, cheapest first.
    // The first one an index serves answers the pattern; an unserved
    // seek (no index, empty CONTAINS needle) yields to the next. The
    // residual still applies every predicate.
    let Some(p) = prepared(v, np, conjuncts, graph, row, params) else {
        // `USING SCAN`: no seek may answer this variable. The scan policy
        // ([`scan_ids`]) with no property map IS the scan; `node_ok` and
        // the residual conjuncts then apply `=` to every candidate — for a
        // zoned-datetime DST overlap that matches both halves, which an
        // index seek (instant plus zone) does not.
        return scan_ids(graph, np, || Ok(BTreeMap::new()), cap);
    };
    match p.first_answered(graph)? {
        Some(Answered::Union(ids)) => return Ok(ids),
        Some(Answered::Serve { ids }) => {
            return ids.narrowed(graph, &p.labels, props);
        }
        None => {}
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
        labels: &p.labels,
        label_seekable: p.label_seekable,
    };
    if let Some(served) = or_union_candidates(v, conjuncts, &ctx) {
        return served.narrowed(graph, &p.labels, props);
    }
    // No seek served the variable: the scan policy answers it.
    scan_ids(graph, np, props, cap)
}

/// The point ask's arm ([`point_candidates_capped`]): the classification
/// read once, then the same seek walk as [`binding_arm`]'s `Unbound` arm
/// reduced to its one question — does a point seek serve the pattern —
/// with no scan, no OR-union and no union answer: none of those is a
/// point lookup, so the ask declines (`None`) instead of running one it
/// would discard. A row-bound variable is its one candidate; a null or
/// non-node binding matches nothing; a nameless element and `USING SCAN`
/// have no point seek to serve them.
fn point_arm(
    class: NodeBinding<'_>,
    np: &NodePattern,
    conjuncts: &[Expr],
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<Option<Vec<NodeId>>> {
    let v = match admitted(class) {
        Admitted::One(id) => return Ok(Some(vec![id])),
        Admitted::Empty => return Ok(Some(Vec::new())),
        // A nameless element has no variable a seek targets, so no point
        // seek serves it.
        Admitted::Scan => return Ok(None),
        Admitted::Seek(v) => v,
    };
    let Some(p) = prepared(v, np, conjuncts, graph, row, params) else {
        // `USING SCAN`: no seek may answer this variable, a point seek
        // included.
        return Ok(None);
    };
    let Some(ids) = p.first_point(graph) else {
        return Ok(None);
    };
    let props = map_thunk(graph, row, np, params);
    ids.narrowed(graph, &p.labels, props).map(Some)
}

/// The pattern's match-role property-map thunk, the one derivation — the
/// binding arm's scan paths, every served answer's narrowing
/// ([`ServedIds::narrowed`]) and the point ask's answer all read it;
/// bound once per arm and passed where the arm's diverging paths need it.
fn map_thunk<'a>(
    graph: &'a ExecGraph<'_>,
    row: &'a Row,
    np: &'a NodePattern,
    params: &'a BTreeMap<String, Value>,
) -> impl Fn() -> Result<BTreeMap<String, Value>> + Copy + 'a {
    || {
        pattern_props(
            graph,
            row,
            np.properties.as_ref(),
            PatternMapRole::Match,
            params,
        )
    }
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
        let labels = pattern_labels(np);
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
