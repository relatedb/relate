//! Quantified path patterns: the repetition cursor, the per-variable
//! group accumulators, the QPP DFS over `gpm_walk_elements` iterations,
//! and the mid-path `allReduce` inliner (an allReduce conjunct of the
//! MATCH WHERE whose list is one of the quantification's group lists is
//! evaluated per iteration, pruning the branch the moment a prefix
//! predicate is false — a null prefix poisons the fold instead, since
//! the list evaluator scans past a null and a later element can raise).

use std::sync::Arc;

use super::walk::{gpm_accept_target, gpm_walk_elements, target_rejects_before_clone};
use super::*;
use relate_ir::QuantifiedPathPattern;

/// Quantified-path-pattern iteration cursor: repetition count, the
/// per-variable group accumulators, the inlined allReduce folds running
/// down this DFS branch, and the pre-bound group lists (the legacy
/// var-length exact-trail pins, captured once at expansion entry — see
/// `gpm_expand_quantified`). Each accumulator is the shared list a stop
/// binds as-is (`Value::List`'s own `Arc`): a stop's bind is one `Arc`
/// clone, never a copy of the list; an iteration's push copies only a
/// list the parent branch still shares (`Arc::make_mut`).
struct QppCursor<'a> {
    count: u32,
    max: u32,
    groups: Vec<(&'a str, Arc<Vec<Value>>)>,
    reduces: Vec<BranchReduce<'a>>,
    prebound: &'a BTreeMap<String, Value>,
    /// The group's own element table (LABS-801), built once at the
    /// group's entry — the per-iteration re-entry into the elements walk
    /// reads it, never re-resolves.
    filters: &'a [super::ElementFilter],
}

/// One inlined allReduce's running state down a DFS branch: the fold, or
/// `None` once a step (or the initial) raised — the branch then runs
/// unpruned and the post-filter, which stays in place, reproduces the
/// error on whatever rows survive. Pruning never changes which errors
/// surface: a raised step would raise identically in the post-filter of
/// every row this branch produces, and a branch that produces no rows was
/// never evaluated there.
#[derive(Clone)]
struct BranchReduce<'a> {
    spec: &'a InlinedAllReduce<'a>,
    fold: Option<AllReduceFold<'a>>,
}

/// The inlined allReduce specs tracking this quantification (their group
/// is one of its accumulated names), each with its fold started from the
/// initial's value on the entering row. A spec whose initial raises is
/// poisoned from the start (`fold: None`).
fn active_reduces<'a>(gctx: &GpmCtx<'a>, names: &[String], row: &Row) -> Vec<BranchReduce<'a>> {
    let GpmCtx { graph, params, .. } = *gctx;
    let inlined = &gctx.walk.inlined;
    #[cfg(test)]
    if allreduce_pruning_disabled() {
        return Vec::new();
    }
    inlined
        .iter()
        .filter(|spec| names.iter().any(|name| name.as_str() == spec.group))
        .map(|spec| {
            let fold = match eval(graph, row, spec.initial, params) {
                // A raising initial poisons the fold rather than
                // propagating: the untouched post-filter raises it on the
                // rows that survive, exactly as it did before inlining.
                Ok(acc) => Some(AllReduceFold::start(
                    acc,
                    spec.accumulator,
                    spec.ident,
                    spec.expression,
                )),
                Err(_) => None,
            };
            BranchReduce { spec, fold }
        })
        .collect()
}

/// Advance every inlined fold one iteration down this branch. `false`
/// means a predicate is false on the new prefix and no extension of the
/// branch can make the allReduce conjunct true — `eval_all_reduce`
/// returns false the moment a prefix predicate is false — so the caller
/// drops the iteration. A null prefix verdict is NOT deciding: the list
/// evaluator scans past a null (`saw_null`, the scan continues) and a
/// later element can still raise, erroring the row — pruning on null
/// could swallow that error, so a null verdict poisons the fold and the
/// branch runs unpruned, the post-filter deciding on the completed row
/// (review round 1, the null-prefix-then-raise reproducer). A raising
/// step poisons that fold the same way and the scan continues.
fn advance_reduces(
    gctx: &GpmCtx<'_>,
    reduces: &mut [BranchReduce<'_>],
    iteration_row: &Row,
) -> Result<bool> {
    let GpmCtx { graph, params, .. } = *gctx;
    for reduce in reduces.iter_mut() {
        let Some(mut fold) = reduce.fold.take() else {
            continue; // poisoned: this branch runs unpruned
        };
        let Some(element) = iteration_row.get_name(reduce.spec.group).cloned() else {
            // No element appended this iteration: nothing to fold and
            // nothing to check, matching the group accumulation's skip.
            reduce.fold = Some(fold);
            continue;
        };
        let verdict = fold.step(
            graph,
            &mut iteration_row.clone(),
            &element,
            reduce.spec.predicate,
            params,
        );
        match verdict {
            Ok(AllReduceStep::Holds) => reduce.fold = Some(fold),
            // False is deciding: no extension survives the post-filter.
            Ok(AllReduceStep::Failed) => return Ok(false),
            // Null is not: poison the fold, never prune on it.
            Ok(AllReduceStep::Null) => reduce.fold = None,
            Err(_) => reduce.fold = None,
        }
    }
    Ok(true)
}

#[cfg(test)]
thread_local! {
    /// Test hook: stand the mid-path allReduce pruning down (the
    /// post-filter still applies), so a differential test can run the
    /// very same statement both ways.
    static ALLREDUCE_PRUNING_DISABLED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Run `f` with the mid-path allReduce pruning stood down. The flag is
/// restored on unwind too, so a panicking assertion inside the closure
/// cannot leave the pruning disabled for every later test on the thread.
#[cfg(test)]
pub(super) fn with_allreduce_pruning_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::super::test_support::with_flag_disabled(&ALLREDUCE_PRUNING_DISABLED, f)
}

#[cfg(test)]
fn allreduce_pruning_disabled() -> bool {
    ALLREDUCE_PRUNING_DISABLED.with(|flag| flag.get())
}

pub(super) fn gpm_expand_quantified(
    gctx: &GpmCtx<'_>,
    mut state: GpmState,
    qpp: &QuantifiedPathPattern,
    filters: &[super::ElementFilter],
    target: Option<&NodePattern>,
    sink: &mut StateSink<'_>,
) -> Result<()> {
    // The group's whole rebinding vocabulary — inner element names plus
    // the subpath variable — from the one spelling every consumer reads.
    let mut names = Vec::new();
    gpm_group_vocabulary(qpp, &mut names);
    // A name a predicate of this quantification references that neither
    // the entering row nor the quantification itself binds — a non-local
    // singleton declared later in the pattern — reads as null inside the
    // quantification (Neo4j-verified); binding it on the
    // walk's base row states that once instead of the evaluator
    // fabricating it (ADR-0335).
    for name in qpp_unbound_predicate_names(qpp, &state.row, &names) {
        state.row.insert(name, Value::Null);
    }
    // The group accumulators, one entry per vocabulary name **in the
    // vocabulary's order** (`gpm_group_vocabulary`: inner element names
    // in pattern order, the subpath variable last) — the order the
    // accept's group-bind binds a fresh name in. That is the order every
    // count>=1 stop's iteration walk binds the names in (its accepts
    // bind the elements in pattern order; the subpath variable is
    // constructed into its list, never bound by the walk, so it appends
    // last), so the zero-repetition stop — where every vocabulary name
    // is fresh — appends them in the same positions and every complete
    // state of one quantified walk carries the same schema. The
    // completion's memoized path-name rung stamps all of them
    // (`Row::stamp`'s prefix invariant); an accept that bound in any
    // other order — a sorted map's alphabetical one, say — would give
    // the zero-repetition row a differently ordered schema and corrupt
    // every later completion's row (the original failure sorted `p2` before
    // `x`/`z`).
    let groups = names
        .iter()
        .map(|name| (name.as_str(), Arc::new(Vec::<Value>::new())))
        .collect::<Vec<_>>();
    // The legacy var-length reuse rule (LABS-661): a group name the
    // entering row already binds to a list pins the exact trail — the
    // accumulated list must equal that binding for a branch to be
    // accepted (`WITH [r1, r2] AS rs MATCH (a)-[rs*]->(b)`), the same
    // compatibility check `gpm_rel_dfs` applies to a relationship
    // variable. Captured once at entry: the DFS levels below replace the
    // row with per-iteration rows, which rebind the name per hop. A
    // fresh name (absent or null — `prebound`, the pinned-or-fresh
    // policy every rebinding reads) pins nothing.
    let prebound: BTreeMap<String, Value> = names
        .iter()
        .filter_map(|name| match prebound(state.row.get(name)) {
            Prebound::Fresh => None,
            Prebound::Pinned(pinned) => Some((name.clone(), pinned.clone())),
        })
        .collect();
    let reduces = active_reduces(gctx, &names, &state.row);
    // The unbounded (`{m,}` / legacy `*`) DFS depth cap: one rule for every
    // spelling — the rel count under DIFFERENT RELATIONSHIPS (a sound
    // uniqueness bound), the satiety cap under REPEATABLE ELEMENTS (no
    // uniqueness termination; see [`GpmCtx::unbounded_dfs_cap`], the
    // walk-memoized helper the legacy var-length rel DFS consults too, so
    // both spellings of one unbounded pattern enumerate to the same depth,
    // LABS-661).
    let max = qpp.max.unwrap_or_else(|| gctx.unbounded_dfs_cap());
    gpm_qpp_dfs(
        gctx,
        &mut state,
        qpp,
        target,
        QppCursor {
            count: 0,
            max,
            groups,
            reduces,
            prebound: &prebound,
            filters,
        },
        sink,
    )?;
    Ok(())
}

/// Every predicate a quantified pattern carries: each inner element's own
/// `WHERE` in pattern order, then the pattern-level `WHERE` — the one
/// policy both the iteration check and the unbound-name scan walk.
fn qpp_predicates(qpp: &QuantifiedPathPattern) -> impl Iterator<Item = &Expr> {
    qpp.elements
        .iter()
        .filter_map(|element| match element {
            PatternElement::Node(node) => node.where_clause.as_ref(),
            PatternElement::Relationship(rel) => rel.where_clause.as_ref(),
            PatternElement::Quantified(_) => None,
        })
        .chain(qpp.where_clause.as_ref())
}

/// The predicate-referenced names of `qpp` that `row` does not bind and the
/// quantification's own group `names` will not bind — the later-declared
/// non-local singletons [`gpm_expand_quantified`] binds to null.
fn qpp_unbound_predicate_names(
    qpp: &QuantifiedPathPattern,
    row: &Row,
    names: &[String],
) -> Vec<String> {
    let mut vars = HashSet::new();
    for predicate in qpp_predicates(qpp) {
        relate_ir::expr_vars(predicate, &mut vars);
    }
    vars.into_iter()
        .filter(|name| !row.contains_key(name) && !names.contains(name))
        .collect()
}

fn gpm_qpp_iteration_predicates_ok(
    graph: &ExecGraph<'_>,
    qpp: &QuantifiedPathPattern,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    for predicate in qpp_predicates(qpp) {
        if !crate::eval::predicate_value_holds(&eval(graph, row, predicate, params)?)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The quantification DFS over one in-place state, in the shape of
/// `gpm_rel_dfs` (REL8-2242): an iteration's chain ids extend the
/// state's trail and are truncated again after the descent, never cloned
/// per hop, and the frame *consumes* the state's row and used set (moved
/// into the iteration walk, replaced by the iteration's own before each
/// descent) — so only an accepted stop clones the state, and the caller
/// must not read `row`/`used` after the call. The group vocabulary names
/// are borrowed from the entry (`QppCursor::groups`), never re-allocated
/// per iteration.
fn gpm_qpp_dfs(
    gctx: &GpmCtx<'_>,
    state: &mut GpmState,
    qpp: &QuantifiedPathPattern,
    target: Option<&NodePattern>,
    cursor: QppCursor<'_>,
    sink: &mut StateSink<'_>,
) -> Result<()> {
    let GpmCtx { graph, params, .. } = *gctx;
    let QppCursor {
        count,
        max,
        groups,
        reduces,
        prebound,
        filters,
    } = cursor;
    if count >= qpp.min && !target_rejects_before_clone(gctx, state, target)? {
        let mut accepted = state.clone();
        // The pre-bound group lists gate the accept: a branch whose
        // accumulated list differs from the pinned binding is dropped —
        // the legacy exact-trail rule (see `gpm_expand_quantified`).
        // The binds run in `groups`' order — the vocabulary's — so a
        // fresh name (every name at the zero-repetition stop, the
        // subpath variable at the others) appends exactly where the
        // iteration walk's own binds put it: one schema for every
        // complete state of the walk (see `gpm_expand_quantified`).
        let mut compatible = true;
        for (name, values) in &groups {
            let value = Value::List(Arc::clone(values));
            if let Some(pinned) = prebound.get(*name) {
                if !pinned.eq_non_null(&value) {
                    compatible = false;
                    break;
                }
            }
            accepted.row.insert((*name).to_string(), value);
        }
        if compatible && gpm_accept_target(gctx, &mut accepted, target)? {
            // the one complete-state delivery spelling — a rider (this
            // element is the pattern's last) receives the state and its
            // answer halts the enumeration: no further iterations, no
            // deeper hops
            if super::emit::deliver(gctx, sink, accepted)? {
                return Ok(());
            }
        }
    }
    if count >= max {
        return Ok(());
    }
    let Some(PatternElement::Node(first)) = qpp.elements.first() else {
        // a quantification that does not open on a node element (the
        // lowering's flat label-alternation spelling) has no iteration
        // walk to drive: no further hop, as before
        return Ok(());
    };
    let mut clean = std::mem::take(&mut state.row);
    // The vocabulary rides one carrier — the cursor's `groups` list, its
    // names the group's whole rebinding vocabulary in bind order
    // (`gpm_expand_quantified` builds it 1:1 from
    // `gpm_group_vocabulary`): the row's rebinding names leave before
    // the iteration walk rebinds them.
    for (name, _) in &groups {
        clean.shift_remove(name);
    }
    if !node_ok(graph, state.current, first, &clean, params)?
        || !gpm_bind_node(graph, first, state.current, &mut clean)
    {
        return Ok(());
    }
    // The iteration's slice opens on its start node's id — the one
    // slice-entry policy (`push_visible_node`).
    let mut local_start = Vec::new();
    push_visible_node(graph, state.current, &mut local_start);
    let iter_start = GpmState {
        row: clean,
        used: std::mem::take(&mut state.used),
        current: state.current,
        nodes: local_start,
        rels: Vec::new(),
    };
    // The quantified operand's own walk never offers to the rider (its
    // states are iteration rows, not complete bindings — the accepted
    // stops below go to the continuation, whose last element offers
    // those). Each complete iteration streams into the closure below, no
    // iteration `Vec` (REL8-2242).
    let mut descend = |iteration: GpmState| -> Result<()> {
        if gctx.halted() {
            return Ok(());
        }
        if !gpm_qpp_iteration_predicates_ok(graph, qpp, &iteration.row, params)? {
            return Ok(());
        }
        // PROFILE: one quantification hop taken — the store-read tally
        // behind the plan table's `hops` kind and `Db Hits` total, so a
        // pruned and an unpruned run of the same query are comparable.
        gctx.walk.tallies.hop();
        // Mid-path allReduce: advance each fold one element. A prefix
        // predicate that is false kills every extension of this
        // iteration — the post-filter would reject them all.
        let mut next_reduces = reduces.clone();
        if !advance_reduces(gctx, &mut next_reduces, &iteration.row)? {
            return Ok(());
        }
        // One carrier again: the vocabulary names are the map's keys.
        // Each list takes this iteration's binding for its name — the
        // subpath variable's the constructed path, every other name's
        // the iteration row's binding. Every key exists by construction
        // (the map was keyed 1:1 from the vocabulary at entry) and so
        // does every row binding: the walk of the group's elements
        // binds each named element at its accept and returns only
        // complete iterations — the entering row's vocabulary names
        // were scrubbed before the walk — so an absent binding is an
        // invariant break, never a skip.
        let mut next_groups = groups.clone();
        for (name, values) in &mut next_groups {
            let values = Arc::make_mut(values);
            if qpp.subpath_variable.as_ref().is_some_and(|v| v == name) {
                // the subpath variable observes the iteration's path —
                // the one place an iteration's slices materialize
                values.push(Value::from_path(super::materialize_path(
                    graph,
                    &iteration.nodes,
                    &iteration.rels,
                )));
            } else {
                values.push(
                    iteration
                        .row
                        .get(name)
                        .expect("a complete iteration row binds every vocabulary name")
                        .clone(),
                );
            }
        }
        // The iteration's row, used set and cursor replace the state's;
        // its chain ids extend the trail, truncated again below.
        let (trail_nodes, trail_rels) = (state.nodes.len(), state.rels.len());
        state.row = iteration.row;
        state.used = iteration.used;
        state.current = iteration.current;
        state.nodes.extend(iteration.nodes.into_iter().skip(1));
        state.rels.extend(iteration.rels);
        let descended = gpm_qpp_dfs(
            gctx,
            state,
            qpp,
            target,
            QppCursor {
                count: count + 1,
                max,
                groups: next_groups,
                reduces: next_reduces,
                prebound,
                filters,
            },
            sink,
        );
        state.nodes.truncate(trail_nodes);
        state.rels.truncate(trail_rels);
        descended
    };
    gpm_walk_elements(gctx, &qpp.elements, filters, 1, iter_start, &mut descend)
}
