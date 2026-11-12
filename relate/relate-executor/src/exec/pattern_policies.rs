//! The pattern-policy family's exec-level home: the family-neutral
//! policies the GPM engine (`exec/gpm/`), the expand/match clause sites,
//! and the frontier family (`exec/kernels/`) all consult — hoisted out
//! of `gpm/` so the shared facts live in the lowest module every
//! consumer already reaches (and `expand`'s and `gpm`'s mutual imports
//! stop cycling). One spelling per policy — a move, or the merge of
//! spellings that had drifted, never a second spelling:
//!
//! - [`gpm_group_names`] and [`gpm_group_vocabulary`] — the
//!   rebinding-name vocabulary of a quantified group, the element-level
//!   spelling and the whole-group one that adds the group's own subpath
//!   variable;
//! - [`gpm_path_mode_allows`] and [`path_mode_requires_unique_rels`] —
//!   the per-path mode check table and its uniqueness half;
//! - [`inlined_all_reduces`] and [`InlinedAllReduce`] — the MATCH-WHERE
//!   allReduce inliner's extraction, the spec the mid-path pruning and
//!   the frontier matcher arm's hard decline both read;
//! - [`reduce_row`] — the per-row reduce tail every complete candidate
//!   runs (the reduce's deferred inline predicates, its WHERE, then its
//!   projection), the one spelling the GPM engine's materializing sink, its
//!   cap rider's Cap arm, the walk's own `CollectSink`, and the
//!   frontier family's rider wiring share — with its spec bundle
//!   [`MatchReduce`], the tail-as-one-state-model [`MatchTail`], and
//!   the matched-rows payload [`Matched`] (moved from `expand` beside
//!   it, the home every consumer already reaches).
//!
//! The candidate-filter policy (the inline element predicates plus the
//! WHERE gate) lives in its own one home, `eval/predicates.rs`
//! (`crate::eval`'s row-predicate family, LABS-668 slice 1) — never
//! duplicated here. GPM-only policies (the
//! node binding `gpm_bind_node`, the pinned-or-fresh classification
//! `prebound`, the unbounded DFS cap `unbounded_dfs_cap`) live in
//! `exec/gpm/mod.rs`, the parent every gpm consumer already reaches —
//! this home is for policies with consumers outside the GPM engine.
//!
//! The path-name binding family — `bind_path_names` with its rung pair
//! (`path_name_rung`, `memoized_path_rung`) — moved one tier down, to
//! `crate::pattern_walk`: both walk engines read it (the eval walker's
//! terminal `bind_eval_path` among the consumers), so its home is the
//! crate's lowest shared tier, not this exec-level one.

use crate::UsedSet;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, RelId, Result, Value};
use relate_ir::{Expr, PathMode, PathPattern, PatternElement, QuantifiedPathPattern};
use relate_names::SmallName;

use crate::eval::{candidate_passes, expr_vars};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::aggregate::Aggregation;
use super::match_cap::MatchCap;
use super::sink::{finish_match_row, SinkProjection};

/// The names a quantified group's inner elements bind: the variables of
/// its inner node and rel elements plus the nested groups' subpath
/// variables — the element-level half of the rebinding-name vocabulary.
/// The consumers whose vocabulary ends there read this spelling directly,
/// each for a stated reason: the allReduce inliner's group lists (the
/// group's own subpath variable is a constructed path, not an iteration
/// row binding, and is excluded as one) and the frontier emission's group
/// lists (a subpath variable declines upstream at the NFA compile). Every
/// consumer that needs the group's own subpath variable too reads
/// [`gpm_group_vocabulary`] — never a hand-rolled append beside this one.
pub(crate) fn gpm_group_names(elements: &[PatternElement], out: &mut Vec<String>) {
    for element in elements {
        let name = match element {
            PatternElement::Node(node) => node.variable.as_ref(),
            PatternElement::Relationship(rel) => rel.variable.as_ref(),
            PatternElement::Quantified(qpp) => qpp.subpath_variable.as_ref(),
        };
        if let Some(name) = name {
            push_new(out, name);
        }
    }
}

/// The rebinding-name vocabulary of one quantified group:
/// [`gpm_group_names`] over its inner elements plus the group's own
/// subpath variable — the one spelling every full-vocabulary consumer
/// reads: the accumulated names the GPM quantified expansion rebinds
/// (`gpm/qpp`), the early conjunct gate's rebinding classification
/// (`gpm/early`), the frontier matcher arm's hard decline
/// (exec/kernels/frontier_match.rs), and the pattern walk's
/// element-binding accumulation ([`element_binding_names`]). The
/// element-level signature of [`gpm_group_names`] cannot see the subpath
/// variable (it lives on the quantified pattern, not an inner element) —
/// this is the one home of that append, so no consumer hand-rolls a
/// second one beside it.
pub(crate) fn gpm_group_vocabulary(qpp: &QuantifiedPathPattern, out: &mut Vec<String>) {
    gpm_group_names(&qpp.elements, out);
    if let Some(name) = &qpp.subpath_variable {
        push_new(out, name);
    }
}

/// The vocabulary helpers' dedupe policy: a name already accumulated
/// stays put — one binding per name, first spelling wins.
fn push_new(out: &mut Vec<String>, name: &SmallName) {
    // Text render: the accumulated vocabulary is the String scope domain.
    if !out.iter().any(|n| name == n.as_str()) {
        out.push(name.to_string());
    }
}

/// The one path-mode → relationship-uniqueness policy: the modes whose
/// semantics need the walk to reject a relationship it already used
/// (LABS-668 slice 1's extraction). The DFS walk's `enforce_unique` gate
/// and [`gpm_path_mode_allows`] both read it.
pub(crate) fn path_mode_requires_unique_rels(mode: PathMode) -> bool {
    matches!(mode, PathMode::Trail | PathMode::Acyclic | PathMode::Simple)
}

/// The per-path mode check: whether a walked path's nodes and rels
/// satisfy the written mode — the one table the GPM engine's per-path
/// pipeline and the frontier emission's per-witness check both apply,
/// so an accepted row is accepted by the same predicate both ways. The
/// check is identity-only, so it reads the path as id sequences: the
/// GPM walk's own id slices (`GpmState`) as-is, the frontier's
/// materialized records projected to their ids.
pub(crate) fn gpm_path_mode_allows(
    mode: Option<PathMode>,
    nodes: impl ExactSizeIterator<Item = NodeId> + Clone,
    rels: impl IntoIterator<Item = RelId>,
) -> bool {
    let mode = mode.unwrap_or(PathMode::Walk);
    if !path_mode_requires_unique_rels(mode) {
        return true;
    }
    match mode {
        PathMode::Walk => unreachable!("Walk carries no uniqueness requirement"),
        PathMode::Trail => {
            let mut seen = HashSet::new();
            rels.into_iter().all(|id| seen.insert(id))
        }
        PathMode::Acyclic => {
            let mut seen = HashSet::new();
            nodes.into_iter().all(|id| seen.insert(id))
        }
        // Simple = Trail's rel uniqueness AND Acyclic's node uniqueness
        // (the closing node may equal the first) — one total arm, so the
        // predicate is whole regardless of what the caller already
        // enforced (the walk's `enforce_unique` gate is an
        // optimization, not part of the mode's contract).
        PathMode::Simple => {
            let mut seen_rels = HashSet::new();
            if !rels.into_iter().all(|id| seen_rels.insert(id)) {
                return false;
            }
            let mut seen = HashSet::new();
            let first = nodes.clone().next();
            let last = nodes.len().saturating_sub(1);
            for (index, id) in nodes.enumerate() {
                if index == last && first == Some(id) {
                    continue;
                }
                if !seen.insert(id) {
                    return false;
                }
            }
            true
        }
    }
}

/// One inlined allReduce conjunct of the MATCH WHERE: the accumulator
/// state is advanced one element per quantification iteration and the
/// branch is pruned when a prefix predicate is false. Extraction
/// ([`inlined_all_reduces`]) guarantees the per-prefix values equal the
/// post-filter's per-element values, so the pruning is exact.
pub(crate) struct InlinedAllReduce<'c> {
    /// The quantified group the fold runs over (`v IN <group>`). The
    /// name fields ride the IR's representation (ADR-0509 stage 3).
    pub(crate) group: &'c SmallName,
    pub(crate) accumulator: &'c SmallName,
    pub(crate) initial: &'c Expr,
    pub(crate) ident: &'c SmallName,
    pub(crate) expression: &'c Expr,
    pub(crate) predicate: &'c Expr,
}

/// The MATCH-WHERE allReduce conjuncts this pattern's quantified
/// expansions can evaluate mid-path. A conjunct qualifies when:
///
/// * it is a top-level `AND` conjunct — a lone failure of it rejects the
///   row, so pruning on its prefix verdicts only removes rows the
///   post-filter (still in place) would drop;
/// * its list is a bare variable accumulated by exactly one quantified
///   element of this pattern, and is not that element's subpath variable
///   (whose per-iteration value is a constructed path, not an iteration
///   row binding);
/// * every name the update/predicate references besides the accumulator
///   and the loop variable — which the fold binds before evaluating
///   either, exactly as the post-filter does — and every name the
///   initial references (the accumulator and loop variable included: the
///   initial runs on the entering walk row, before the fold binds them,
///   so a reference there can read a CIP-28 null pre-binding the
///   post-filter's completed row does not hold) is bound before the
///   quantification — the entering row or earlier elements of the
///   pattern — so the mid-path values equal the post-filter's values
///   exactly. A name bound later (the pattern's end node, the path
///   variable, another group list) disqualifies the conjunct; it stays a
///   post-filter.
///
/// Also the frontier matcher arm's allReduce gate
/// (exec/kernels/frontier_match.rs): it reads the exact specs the GPM
/// engine would run against the same row, so its answer there is exact.
pub(crate) fn inlined_all_reduces<'c>(
    path: &PathPattern,
    row: &Row,
    conjuncts: &'c [Expr],
) -> Vec<InlinedAllReduce<'c>> {
    // A path selector chooses among the pattern's full candidate set (a
    // MATCH-WHERE predicate narrows the already-selected path, CIP-60
    // scenario 8's semantics), so the mid-path pruning must stand down —
    // the same selector gate the early conjuncts take (`gpm/early`).
    if path.selector.is_some() {
        return Vec::new();
    }
    let quantifieds: Vec<&QuantifiedPathPattern> = path
        .elements
        .iter()
        .filter_map(|element| match element {
            PatternElement::Quantified(qpp) => Some(qpp),
            _ => None,
        })
        .collect();
    if quantifieds.is_empty() {
        return Vec::new();
    }
    let group_names: Vec<Vec<String>> = quantifieds
        .iter()
        .map(|qpp| {
            let mut names = Vec::new();
            gpm_group_names(&qpp.elements, &mut names);
            names
        })
        .collect();
    let mut out = Vec::new();
    // The names bound entering each element, walked left to right: the
    // row's own keys plus every binding the earlier elements make.
    let mut bound: HashSet<String> = row.keys().cloned().collect();
    for element in &path.elements {
        if let PatternElement::Quantified(qpp) = element {
            let mut groups = Vec::new();
            gpm_group_names(&qpp.elements, &mut groups);
            for expr in conjuncts {
                if let Some(spec) = inlinable_over(expr, qpp, &groups, &group_names, &bound) {
                    out.push(spec);
                }
            }
        }
        element_binding_names(element, &mut bound);
    }
    out
}

/// The conjunct as an inlinable spec over `qpp`'s groups, or `None` when
/// any of the qualification rules above fails.
fn inlinable_over<'c>(
    expr: &'c Expr,
    qpp: &QuantifiedPathPattern,
    groups: &[String],
    all_group_names: &[Vec<String>],
    bound: &HashSet<String>,
) -> Option<InlinedAllReduce<'c>> {
    let Expr::AllReduce {
        accumulator,
        initial,
        ident,
        list,
        expression,
        predicate,
    } = expr
    else {
        return None;
    };
    let group = match &**list {
        Expr::Variable(name) => name,
        _ => return None,
    };
    // This quantification accumulates the group, and exactly one
    // quantification in the pattern does (a shape shared with another
    // would leave the post-filter reading the other's elements; the
    // planner's group-variable reuse rules already forbid it — this keeps
    // the inliner's premise local to the pattern).
    if !groups.iter().any(|name| group == name.as_str()) {
        return None;
    }
    if all_group_names
        .iter()
        .filter(|names| names.iter().any(|name| group == name.as_str()))
        .count()
        != 1
    {
        return None;
    }
    // The subpath variable's per-iteration value is a constructed path;
    // the other group variables' values are iteration row bindings.
    if qpp.subpath_variable.as_ref() == Some(group) {
        return None;
    }
    for part in [&**expression, &**predicate] {
        if !refs_only_bound_names(part, accumulator, ident, bound) {
            return None;
        }
    }
    if !initial_refs_only_bound_names(initial, accumulator, ident, bound) {
        return None;
    }
    Some(InlinedAllReduce {
        group,
        accumulator,
        initial,
        ident,
        expression,
        predicate,
    })
}

/// Whether every name `expr` references besides the accumulator and the
/// loop variable is already bound (`bound`) — the mid-path evaluation
/// then reads the same values the post-filter's evaluation reads. Sound
/// for the update and the predicate only: the fold binds the accumulator
/// and the loop variable before evaluating either, mid-path exactly as
/// the post-filter's `eval_all_reduce` does.
fn refs_only_bound_names(
    expr: &Expr,
    accumulator: &SmallName,
    ident: &SmallName,
    bound: &HashSet<String>,
) -> bool {
    let mut vars = HashSet::new();
    expr_vars(expr, &mut vars);
    accumulator.with_str(|text| {
        vars.remove(text);
    });
    ident.with_str(|text| {
        vars.remove(text);
    });
    vars.iter().all(|name| bound.contains(name))
}

/// Whether the allReduce's `initial` is stable mid-path: it references
/// neither the accumulator nor the loop variable, and every name it does
/// reference is already bound (`bound`). The `initial` is evaluated on
/// the entering walk row — before the fold binds either name — so a
/// reference to one of them reads whatever that row holds: a CIP-28
/// non-local singleton (a name referenced inside the quantification and
/// declared later in the pattern, e.g. the pattern's end node) is
/// pre-bound to null there (ADR-0335) while the post-filter's completed
/// row holds the pattern's final binding. The two `initial` values then
/// disagree and the mid-path fold tracks a different accumulator than
/// the post-filter's, so any such reference disqualifies the conjunct —
/// it stays a post-filter (review round 1, the `total = size(b.id)`
/// reproducer with `b` the end node).
fn initial_refs_only_bound_names(
    initial: &Expr,
    accumulator: &SmallName,
    ident: &SmallName,
    bound: &HashSet<String>,
) -> bool {
    let mut vars = HashSet::new();
    expr_vars(initial, &mut vars);
    if accumulator.with_str(|text| vars.contains(text))
        || ident.with_str(|text| vars.contains(text))
    {
        return false;
    }
    vars.iter().all(|name| bound.contains(name))
}

/// The names one pattern element binds: a node or relationship variable,
/// or a quantified element's subpath variable and group variables — the
/// per-element spelling [`gpm_group_names`] owns for the plain kinds (a
/// single-element slice: that helper walks elements, and this element is
/// one), [`gpm_group_vocabulary`] for a quantified one.
fn element_binding_names(element: &PatternElement, out: &mut HashSet<String>) {
    let mut names = Vec::new();
    match element {
        PatternElement::Node(_) | PatternElement::Relationship(_) => {
            gpm_group_names(std::slice::from_ref(element), &mut names);
        }
        PatternElement::Quantified(qpp) => gpm_group_vocabulary(qpp, &mut names),
    }
    out.extend(names);
}

/// What a row that passed the reduce's checks becomes.
pub(crate) enum ReduceSink<'a> {
    /// The filter-only reduce `match_clause` builds when a cap rides a
    /// `WHERE`-bearing walk without a sink projection (LABS-853): the
    /// clause filter runs once, in the sink, ahead of the cap's count.
    Filter,
    /// The projected row is stored.
    Project(&'a SinkProjection<'a>),
    /// The streamed-aggregation sink: the raw row is offered to the fold
    /// and not returned, so the clause collects nothing.
    Fold(&'a dyn RowFold),
}

impl<'a> ReduceSink<'a> {
    /// The sink a clause's projection and fold select; a fold excludes a
    /// projection (the consumer folds the raw row).
    pub(crate) fn select(
        projection: Option<&'a SinkProjection<'a>>,
        fold: Option<&'a dyn RowFold>,
    ) -> Self {
        match (fold, projection) {
            (Some(fold), _) => Self::Fold(fold),
            (None, Some(projection)) => Self::Project(projection),
            (None, None) => Self::Filter,
        }
    }
}

/// What the last pattern of a `MATCH` evaluates before a row is stored
/// — the plain spec bundle `reduce_row` reads (moved from `expand`, the
/// home every consumer already reaches).
pub(crate) struct MatchReduce<'a> {
    pub sink: ReduceSink<'a>,
    pub patterns: &'a [PathPattern],
    pub where_clause: Option<&'a Expr>,
    pub params: &'a BTreeMap<String, Value>,
    pub cypher_version: Option<u32>,
}

/// A consumer that takes a `MATCH`'s finished rows one at a time instead of
/// the clause collecting them. Object-safe so [`MatchReduce`] carries it
/// without naming the aggregation's lifetime.
pub(crate) trait RowFold {
    fn offer(&self, graph: &ExecGraph<'_>, row: Row) -> Result<()>;

    /// A finished container a rung built (the batch chain, the contracted
    /// kernels) offered whole. The default folds it row by row, uncharged
    /// (an aggregation keeps its historical answer: the container is
    /// consumed at once); a consumer that buffers the rows onward charges
    /// the container against the expression cap first, so a kernel-built
    /// container is refused at the cap exactly as the held path refused it.
    fn offer_container(&self, graph: &ExecGraph<'_>, rows: Vec<Row>) -> Result<()> {
        rows.into_iter().try_for_each(|row| self.offer(graph, row))
    }
}

/// The `MATCH` output folded into an [`Aggregation`] as it is emitted, with
/// the count of rows it took (the clause's PROFILE row count).
pub(crate) struct MatchFold<'a> {
    aggregation: RefCell<Aggregation<'a>>,
    offered: Cell<u64>,
}

impl<'a> MatchFold<'a> {
    pub(crate) fn new(aggregation: Aggregation<'a>) -> Self {
        Self {
            aggregation: RefCell::new(aggregation),
            offered: Cell::new(0),
        }
    }

    /// The aggregation holding every offered row, and how many there were.
    pub(crate) fn into_parts(self) -> (Aggregation<'a>, u64) {
        (self.aggregation.into_inner(), self.offered.get())
    }
}

impl RowFold for MatchFold<'_> {
    fn offer(&self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        self.offered.set(self.offered.get() + 1);
        self.aggregation.borrow_mut().offer(graph, row)
    }
}

/// The matched-rows payload: each surviving row with its used-rel set
/// (the relationship-uniqueness carrier). The one name for this shape,
/// shared by the walk's sinks, the hub join's side bindings, and the
/// shortest-route matcher (LABS-348: it had three private spellings).
pub(crate) type Matched = Vec<(Row, UsedSet)>;

/// The reduce tail as one state model — the reduce bundle
/// ([`MatchReduce`], the checks and projection `reduce_row` runs) and the
/// used-rel carry decision as ONE variant pair, not two independent
/// facts that must stay hand-in-sync: an intermediate pattern (or a
/// plain side, or a probe route) carries its used-set forward and never
/// reduces ([`MatchTail::Carry`]); the last pattern finishes its rows —
/// running the reduce when the clause sink projects here
/// ([`MatchTail::Finish`] with `Some`), dropping the used-set otherwise
/// (`Finish` with `None`). The dead combination — a reduce that also
/// carries — is unrepresentable, and the carry decision has the one
/// [`MatchTail::store_row`] spelling: every consumer routes through it
/// (the classic collector's fused step, the GPM Cap arm, and
/// the materializing sink's whole-set arm). Built where its values are
/// decided — `match_paths`'s last-pattern arm and the plain-side
/// enumeration (match_clause.rs) and `exists_probe::seek`'s probe
/// dispatch — and threaded from there through `match_one_path`'s fork
/// into both engines: the GPM chain (`match_gpm_path`, `mount_rider`,
/// the emit pipeline's Cap arm and the materializing sink) and
/// the classic walk's `CollectSink`. The rider never rides in it: the
/// rider is a separate vocabulary (a cap or a probe, `rider::Rider`)
/// alongside the tail, not a member of it — bundling them would be one
/// struct carrying two independent facts (the parallel-state rule).
#[derive(Clone, Copy)]
pub(crate) enum MatchTail<'a> {
    /// An intermediate pattern's tail: keep the used-set (the next
    /// pattern's uniqueness carrier), no reduce. Also the tail a probe
    /// route carries (`exists_probe::seek`): a probe answers a verdict
    /// through its halt cell, never projects, and no probe consumer
    /// reads the tail — `Carry` is that named fact, the honest spelling
    /// of the probe's tailless ride.
    Carry,
    /// The last pattern's tail: the used-set is dropped (finished rows
    /// do not carry it) and the reduce runs when the clause sink
    /// projects here.
    Finish { reduce: Option<&'a MatchReduce<'a>> },
}

impl MatchTail<'_> {
    /// One accepted row into a collector's output — the shared back half
    /// of both cap arms (`CollectSink::offer` for the classic walk, the
    /// GPM `Cap` arm) and the one spelling of the carry decision every
    /// consumer routes through (whole row sets too: the materializing sink's
    /// arms): a carry tail keeps the set, built lazily so the drop arm
    /// never pays for it; a finish tail drops it and the closure never
    /// runs, storing the empty carry (`UsedSet::default()`, LABS-822)
    /// instead. The halves that differ stay with their arms:
    /// the classic collector meters workspace bytes around this store,
    /// the GPM arm sets the walk's halt cell after it.
    pub(crate) fn store_row(&self, row: Row, set: impl FnOnce() -> UsedSet, out: &mut Matched) {
        out.push((
            row,
            match self {
                MatchTail::Carry => set(),
                MatchTail::Finish { .. } => UsedSet::default(),
            },
        ));
    }

    /// One complete row through the fused tail step — the one spelling
    /// of the pipeline both engines' collectors read (`CollectSink::offer`
    /// for the classic walk, the GPM `Cap` arm for the GPM walk) and the
    /// GPM materializing tail's whole-set loop reads row by row
    /// (the collecting sink's, cap `None`), so the ordering policy and its
    /// rationale live in one place: the reduce's
    /// checks and projection (`reduce_row`), the cap's count on the
    /// pre-projection row the checks accepted (a Distinct cap evaluates
    /// its RETURN items on the counted row, so the counted row is the
    /// pre-projection one by contract; a row-limit cap riding a
    /// `WHERE`-bearing walk counts behind a filter-only reduce, so a row
    /// the clause filter drops never moves it; a no-reduce tail accepts
    /// every row, so its count runs before the row moves), then
    /// the row to store — the projected row under a reduce, the row
    /// itself (moved, never cloned) without one. `Ok(None)` rejects: the
    /// row failed the tail's checks and the cap never counted it.
    pub(crate) fn finish_row(
        &self,
        graph: &ExecGraph<'_>,
        row: Row,
        cap: Option<&mut MatchCap<'_>>,
    ) -> Result<Option<Row>> {
        // the one spelling of the count, at its one load-bearing
        // position per arm
        let count = |cap: Option<&mut MatchCap<'_>>, row: &Row| -> Result<()> {
            if let Some(cap) = cap {
                cap.offer(graph, row)?;
            }
            Ok(())
        };
        match self {
            MatchTail::Finish {
                reduce: Some(reduce),
            } => {
                // a cap counts the pre-projection row the checks accepted,
                // so only a capped tail keeps it past the reduce's move
                let counted = cap.is_some().then(|| row.clone());
                let Some(stored) = reduce_row(graph, row, Some(*reduce))? else {
                    return Ok(None);
                };
                if let Some(counted) = &counted {
                    count(cap, counted)?;
                }
                Ok(Some(stored))
            }
            MatchTail::Finish { reduce: None } | MatchTail::Carry => {
                count(cap, &row)?;
                Ok(Some(row))
            }
        }
    }
}

/// The per-row reduce tail every complete candidate runs, in the order
/// the materializing path applies it: the reduce's deferred inline
/// predicates, then its WHERE, then the projection. `None` rejects the
/// row; `Some` returns the row to keep — the projected row under a
/// reduce, the row itself without one (a pass-through clone, cheap:
/// every `Value` payload is `Arc`-shared). The one spelling of this
/// tail: the collecting sink loops it, the cap rider's Cap
/// arm (exec/gpm/emit.rs) calls it, the walk's own sink
/// `CollectSink::offer` (exec/expand.rs) fuses its offered rows through
/// it — its cap's count reading the pre-projection row after the tail
/// accepted it — and the frontier family's rider wiring
/// (exec/kernels/frontier_run.rs, ADR-0478) runs its emitted rows
/// through it — so a rider-emitted row is the row the materialized walk
/// would emit by construction, not by hand-maintained copies agreeing.
pub(crate) fn reduce_row(
    graph: &ExecGraph<'_>,
    row: Row,
    reduce: Option<&MatchReduce<'_>>,
) -> Result<Option<Row>> {
    let Some(reduce) = reduce else {
        return Ok(Some(row));
    };
    // the one filter policy — the same candidate_passes the walk's sink
    // and the ordinary MATCH projection consult
    if !candidate_passes(
        graph,
        reduce.patterns,
        reduce.where_clause,
        &row,
        reduce.params,
        reduce.cypher_version,
    )? {
        return Ok(None);
    }
    match reduce.sink {
        ReduceSink::Fold(fold) => {
            fold.offer(graph, row)?;
            Ok(None)
        }
        ReduceSink::Filter => Ok(Some(row)),
        ReduceSink::Project(projection) => Ok(Some(finish_match_row(
            graph,
            &row,
            projection,
            reduce.params,
            reduce.cypher_version,
        )?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_row_carries_a_carry_tails_set() {
        let mut out: Matched = Vec::new();
        MatchTail::Carry.store_row(Row::new(), || UsedSet::from_iter([1, 2]), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, UsedSet::from_iter([1, 2]));
    }

    #[test]
    fn store_row_drops_a_finish_tails_set() {
        // LABS-822: a finish tail drops whatever the producer would
        // carry — the closure never runs and the stored set is the
        // shared empty carry. (That representation is the
        // implementation's own cost story, in `UsedSet::default()`.)
        let mut out: Matched = Vec::new();
        MatchTail::Finish { reduce: None }.store_row(
            Row::new(),
            || UsedSet::from_iter([1, 2]),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].1.is_empty());
    }
}
