//! Cardinality estimates and MATCH pattern order.
//!
//! A missing count falls back to a rank sentinel. Measured counts may be
//! scaled by relationship degree; rank sentinels stay unscaled.

use std::collections::HashSet;

use relate_ir::{Expr, NodePattern, PathPattern, PatternElement, RelPattern};
use relate_names::NamedToken;

use crate::stats::{NoPatternStats, PatternStats};

mod estimate;
mod selectivity;

pub use estimate::{
    best_seek, crossover_scan_rows, endpoint_estimate, range_seek_gated, where_conjuncts_of, Bound,
    Estimate, SeekPick,
};
use estimate::{endpoint_bound, label_hits, RANK_BARE};

/// Named paths, named variable-length relationships, and GPM paths stay
/// in written order (TCK Match9). A GPM path's `nodes`/`rels` may be a
/// cleared or duplicated view of the `elements` the GPM engine walks, so
/// an orientation flip could desync the two views — it declines to
/// reorder and only the cost ladder's bare rank prices it.
fn orientation_locked(path: &PathPattern) -> bool {
    path.has_path_name()
        || path.needs_gpm()
        || path.nodes.len() < 2
        || path
            .rels
            .iter()
            .any(|rel| rel.var_length && rel.variable.is_some())
}

/// The reversal's evaluation-order gate — the planner-side twin of the
/// executor's `pattern_self_contained` (ADR-0350): the forward walk binds a
/// pattern's elements left to right, so an element's predicate may legally
/// reference another element's variable and see it bound. Reversing the
/// path evaluates those predicates in the opposite order, so the reference
/// would see an unbound variable — the strict evaluator rejects that
/// (ADR-0335) instead of fabricating null. A path whose element predicates
/// reference a peer pattern variable therefore keeps its written
/// orientation; a variable the surrounding scope already binds is safe in
/// every order.
fn reversal_safe(path: &PathPattern, bound: Bound<'_>) -> bool {
    let own = |var: &str| {
        path.nodes
            .iter()
            .any(|n| n.variable.as_ref().is_some_and(|v| v == var))
            || path
                .rels
                .iter()
                .any(|r| r.variable.as_ref().is_some_and(|v| v == var))
    };
    let mut refs = HashSet::new();
    for node in &path.nodes {
        if let Some(e) = &node.properties {
            relate_ir::expr_vars(e, &mut refs);
        }
        if let Some(e) = &node.where_clause {
            relate_ir::expr_vars(e, &mut refs);
        }
        if let Some(le) = &node.label_expr {
            relate_ir::label_expr_vars(le, &mut refs);
        }
    }
    for rel in &path.rels {
        if let Some(e) = &rel.properties {
            relate_ir::expr_vars(e, &mut refs);
        }
        if let Some(e) = &rel.where_clause {
            relate_ir::expr_vars(e, &mut refs);
        }
    }
    !refs.iter().any(|var| own(var) && !bound(var.as_str()))
}

/// `(cost, seed)`: the degree-scaled walk cost and the start endpoint's
/// own estimate it was seeded from — the tie-break needs the seed, so it
/// is returned rather than recomputed (stellar F-2-6).
fn direction_cost(
    path: &PathPattern,
    reversed: bool,
    bound: Bound<'_>,
    where_conjuncts: &[&Expr],
    stats: &dyn PatternStats,
) -> (u128, u128) {
    if path.nodes.is_empty() {
        return (RANK_BARE, RANK_BARE);
    }
    // The walk cost and the tie-break seed both read the one shared
    // spellings — [`walk_cost`] and [`seed_rows`] — the same arithmetic
    // the executor's estimate runs (LABS-671). The oriented view is two
    // iterator directions, never the collected `Vec` pair this used to
    // build per call: the seed endpoint is the walk's first node in the
    // walked direction, the hop chain zips nodes with rels in it, and
    // the orientation fact is derived beside both.
    let seed = if reversed {
        path.nodes.last().expect("nodes is non-empty")
    } else {
        path.nodes.first().expect("nodes is non-empty")
    };
    let estimate = endpoint_estimate(seed, bound, where_conjuncts.iter().copied(), stats);
    let walked = if reversed {
        walk_cost(
            estimate,
            path.nodes
                .iter()
                .rev()
                .zip(path.rels.iter().rev())
                .map(|(node, rel)| (node, rel, seed_on_rel_start(rel, true))),
            stats,
        )
    } else {
        walk_cost(
            estimate,
            path.nodes
                .iter()
                .zip(path.rels.iter())
                .map(|(node, rel)| (node, rel, seed_on_rel_start(rel, false))),
            stats,
        )
    };
    (walked, seed_rows(estimate))
}

/// Which side of `rel` a walk's seed node rides (LABS-426): the typed
/// derivation every walk-orientation consumer shares — a written-outgoing
/// rel seeded from its start side walked forward, a written-incoming one
/// seeded from its start side walked reversed, and an UNDIRECTED rel has
/// no start side in either orientation (the mean stays; the histogram is
/// keyed by the start side only). [`reverse_path`] flips directions, so a
/// post-reversal spelling is its own written direction.
pub fn seed_on_rel_start(rel: &RelPattern, reversed: bool) -> bool {
    match rel.direction {
        relate_ir::Direction::Outgoing => !reversed,
        relate_ir::Direction::Incoming => reversed,
        relate_ir::Direction::Both => false,
    }
}

/// What one hop's input rows are — the fact that decides whether the
/// degree-sum clamp holds ([`apply_degree`] vs [`apply_path_degree`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum HopRows {
    /// Distinct seed nodes: their degree sum cannot exceed the rel
    /// type's total, so the tail estimate clamps at it.
    Seeds,
    /// Paths: one node recurs across many rows (every path through it),
    /// so the distinct-seed sum bound does not hold and the tail
    /// multiplies unclamped.
    Paths,
}

/// One hop's fanout over a distinct seed set — the planner's walk model
/// and the kernels' cost gates price a hop leaving distinct nodes with
/// it (see [`HopRows::Seeds`]).
pub fn apply_degree(
    rows: u128,
    start: &NodePattern,
    rel: &RelPattern,
    seed_is_start: bool,
    stats: &dyn PatternStats,
) -> u128 {
    scale_degree(rows, start, rel, seed_is_start, stats, HopRows::Seeds)
}

/// One hop's fanout over a path multiset — [`apply_degree`]'s policy
/// without its degree-sum clamp ([`HopRows::Paths`]): a chained path
/// multiplicity (a quantifier's second and later hops) whose rows are
/// paths, not distinct nodes. `pub` for the frontier family's cost gate,
/// which prices the DFS's path count against the distinct endpoints.
pub fn apply_path_degree(
    rows: u128,
    start: &NodePattern,
    rel: &RelPattern,
    seed_is_start: bool,
    stats: &dyn PatternStats,
) -> u128 {
    scale_degree(rows, start, rel, seed_is_start, stats, HopRows::Paths)
}

fn scale_degree(
    rows: u128,
    start: &NodePattern,
    rel: &RelPattern,
    seed_is_start: bool,
    stats: &dyn PatternStats,
    hop_rows: HopRows,
) -> u128 {
    priced_degree(rows, start, rel, seed_is_start, stats, hop_rows).unwrap_or(rows)
}

/// How many relationship types a hop names: the one discriminator the
/// hop-pricing sites share, so the "exactly one type" test cannot drift
/// between them.
enum RelTypes<'a> {
    /// An untyped rel.
    None,
    /// Exactly one named type.
    One(&'a NamedToken),
    /// An alternation (`[:A|B]`).
    Many,
}

impl<'a> RelTypes<'a> {
    fn of(rel: &'a RelPattern) -> Self {
        match rel.types.as_slice() {
            [] => Self::None,
            [one] => Self::One(one),
            _ => Self::Many,
        }
    }
}

/// One hop's fanout for a kernel's cost gate (LABS-991): [`apply_degree`]'s
/// price where the statistics can name one, and — where they cannot (an
/// untyped rel, or a hop leaving an unlabeled node, which the walk model
/// leaves at the input row count) — the out-degree tail over every
/// labeled node ([`PatternStats::any_degree_tail`]), unclamped so the
/// bound compounds across a chain's hops. A hop leaving the walk's seed on
/// the rel's start side only: the in-degree and undirected shapes have no
/// histogram to ride and keep the unpriced row count, as does a stats
/// source with no histogram. `pub` for the executor kernels' gate, the one
/// consumer — the planner's own orientation costs keep [`apply_degree`].
pub fn apply_hop_bound(
    rows: u128,
    start: &NodePattern,
    rel: &RelPattern,
    seed_is_start: bool,
    stats: &dyn PatternStats,
) -> u128 {
    if let Some(priced) = priced_degree(rows, start, rel, seed_is_start, stats, HopRows::Seeds) {
        return priced;
    }
    if !seed_is_start {
        return rows;
    }
    // One named type rides that type's tail; untyped and alternated
    // (`[:A|B]`) rels ride the any-type tail, which bounds every type.
    let rel_type = match RelTypes::of(rel) {
        RelTypes::One((rel_type, _)) => Some(rel_type),
        RelTypes::None | RelTypes::Many => None,
    };
    stats
        .any_degree_tail(rel_type)
        .map_or(rows, |tail| rows.saturating_mul(u128::from(tail)))
}

/// The walk model's price for one hop, or `None` when the statistics
/// cannot name one (an untyped or alternated rel, no rel-type count, a
/// seed with no label count or an empty label) — the callers differ only
/// in what an unpriced hop costs.
fn priced_degree(
    rows: u128,
    start: &NodePattern,
    rel: &RelPattern,
    seed_is_start: bool,
    stats: &dyn PatternStats,
    hop_rows: HopRows,
) -> Option<u128> {
    let RelTypes::One(rel_type) = RelTypes::of(rel) else {
        return None;
    };
    // LABS-426 (the mean-to-tail flip): when the hop's seed rides the
    // rel's START side ([`seed_is_start`], the caller's one derived fact),
    // the fanout rides the (seed-label, rel-type) out-degree TAIL — the
    // log2-bucket top, the conservative floor of the highest observed
    // degree — not the mean: a label with a 53-degree mean and one
    // 50k-degree supernode expands its seed rows by the supernode's
    // bucket, the one plan-cost fact the mean cannot express. The min
    // across the seed's labels — every label's tail bounds the
    // intersection's max degree (the A:B node's fanout is inside both the
    // (A,type) and the (B,type) histograms), so the tighter conservative
    // bound is the min, the same discipline [`label_hits`] uses. Any
    // other hop shape — undirected ([`seed_on_rel_start`] answers false
    // in both orientations: no start side), or the seed on the rel's end
    // side — keeps the mean, as does a seed with no histogram (the null
    // stats, an unlabeled start).
    if seed_is_start {
        if let Some(tail) = start
            .labels
            .iter()
            .filter_map(|label| stats.degree_tail(label, rel_type))
            .min()
        {
            // The clamp: the seed subset's degree SUM cannot exceed the
            // rel type's total — a whole-label scan (rows near the label
            // population) is priced by that total, the mean's own scale,
            // not rows x the one supernode's bucket (the sandbox's
            // 1000 x 32768 = 32.7M priced a 53k-fanout scan and flipped
            // the seed to the 100k-DbHits side). A small-seed seek rides
            // the tail alone: one row matched the label's supernode is
            // the case the mean cannot price.
            let scaled = rows.saturating_mul(u128::from(tail));
            return Some(match (hop_rows, stats.rel_type_rows(rel_type)) {
                (HopRows::Seeds, Some(total)) => scaled.min(u128::from(total)),
                _ => scaled,
            });
        }
    }
    let rel_rows = stats.rel_type_rows(rel_type)?;
    let label_rows = label_hits(start, stats)?;
    if label_rows == 0 {
        return None;
    }
    let scaled = rows.saturating_mul(u128::from(rel_rows)) / u128::from(label_rows);
    Some(if rows > 0 && rel_rows > 0 && scaled == 0 {
        1
    } else {
        scaled
    })
}

pub fn seed_rows(estimate: Estimate) -> u128 {
    // The seed's own cost IS the zero-hop walk — one spelling, derived
    // from [`walk_cost`] (the compound model's own doc). Stats is never
    // read on an empty chain, so the no-stats placeholder serves.
    walk_cost(estimate, std::iter::empty::<Hop<'_>>(), &NoPatternStats)
}

/// One hop of the walk model's input: the node the walk is leaving,
/// the rel it traverses, and the hop's derived orientation fact
/// (`seed_is_start`, from the one typed derivation
/// [`seed_on_rel_start`]) — computed once by the orientation owner,
/// never re-derived inside the walk.
pub type Hop<'a> = (&'a NodePattern, &'a RelPattern, bool);

/// The degree-scaled walk over `hops`: one spelling of hop
/// application, read by every walk cost via [`walk_cost`] (module-
/// private: its only callers are in this module).
fn degree_walk<'a>(
    rows: u128,
    hops: impl Iterator<Item = Hop<'a>>,
    stats: &dyn PatternStats,
) -> u128 {
    let mut output = rows;
    for (node, rel, seed_is_start) in hops {
        output = apply_degree(output, node, rel, seed_is_start, stats);
    }
    output
}

/// The walk cost of an endpoint estimate over `hops` — the ONE model
/// of which estimate kind gets which walk, read by the planner's
/// `direction_cost` (the whole pattern's hop chain) and the executor's
/// `GraphStats::estimate` (the first hop of the same chain), so the
/// per-row start selection and the plan-time orientation cannot
/// disagree on the anchor (LABS-671's insurance test ties them at the
/// executor side). A measured seed walks the degree-scaled chain; a
/// sampled `CONTAINS` seed compounds (ADR-0474): its walk cost is the
/// degree-scaled output floored at the whole-label scan — the scan is
/// paid once, before any hop, so it never rides the degree (GBL SF0.1
/// q8: the 13,750-row Forum scan floor keeps the 137-row sampled title
/// seed from undercutting Person's 1,528-row scan whose degree-scaled
/// walk lands on the same 13,750 rels). A rank sentinel carries no
/// measured label to scale — its rows travel unscaled.
pub fn walk_cost<'a>(
    estimate: Estimate,
    hops: impl Iterator<Item = Hop<'a>>,
    stats: &dyn PatternStats,
) -> u128 {
    match estimate {
        Estimate::Measured(rows) => degree_walk(rows, hops, stats),
        // the compound: the degree-scaled walk floored at the scan
        Estimate::SampleScan { rows, scan } | Estimate::Defaulted { rows, scan } => {
            degree_walk(rows, hops, stats).max(scan)
        }
        Estimate::Rank(rows) => rows,
    }
}

/// `(reversed, cost)`. A strictly cheaper expanded cost wins. When both
/// directions estimate the same output (a typed hop scaled by its degree),
/// the smaller seed endpoint wins (ADR-0218): that is the scan, and the
/// expanded row count hides it. Equal seeds keep the written direction.
fn choose_orientation(
    path: &PathPattern,
    bound: Bound<'_>,
    where_conjuncts: &[&Expr],
    stats: &dyn PatternStats,
) -> (bool, u128) {
    // The written orientation is the one the elements' left-to-right
    // binding order is defined on; a locked or peer-referenced path keeps
    // it. Beyond that, an orientation is forced exactly when only one
    // endpoint is bound — seed from the bound end.
    let forced = match (path.nodes.first(), path.nodes.last()) {
        (Some(first), Some(last)) if !orientation_locked(path) => {
            let first_bound = endpoint_bound(first, bound);
            let last_bound = endpoint_bound(last, bound);
            if !reversal_safe(path, bound) || (first_bound && !last_bound) {
                Some(false)
            } else if last_bound && !first_bound {
                Some(true)
            } else {
                None
            }
        }
        _ => Some(false),
    };
    if let Some(reversed) = forced {
        return (
            reversed,
            direction_cost(path, reversed, bound, where_conjuncts, stats).0,
        );
    }
    let (written, written_seed) = direction_cost(path, false, bound, where_conjuncts, stats);
    let (flipped, flipped_seed) = direction_cost(path, true, bound, where_conjuncts, stats);
    if flipped < written {
        return (true, flipped);
    }
    if written < flipped {
        return (false, written);
    }
    if flipped_seed < written_seed {
        (true, flipped)
    } else {
        (false, written)
    }
}

fn pattern_touches(path: &PathPattern, bound: Bound<'_>) -> bool {
    relate_ir::path_pattern_vars(path).any(|var| var.with_str(|text| bound(text)))
}

fn reverse_path(path: &mut PathPattern) {
    path.nodes.reverse();
    path.rels.reverse();
    for rel in &mut path.rels {
        rel.direction = rel.direction.flip();
    }
    if path.elements.len() == path.nodes.len() + path.rels.len() {
        path.elements.reverse();
        for element in &mut path.elements {
            if let PatternElement::Relationship(rel) = element {
                rel.direction = rel.direction.flip();
            }
        }
    }
}

/// Greedy seed plus connected expansion: the cheapest pattern seeds, then
/// each next pattern that shares a variable already bound (LABS-157), or
/// the cheapest remaining one when none is connected. Orientation and the
/// bound set advance with every chosen pattern; the grown set stays
/// internal (connectivity/orientation state — the caller's binding set is
/// `rewrite.rs`'s `advance_bound` fold, the one spelling of what a clause
/// binds). Equal costs keep written order.
pub(crate) fn order_patterns(
    patterns: &mut Vec<PathPattern>,
    prebound: &HashSet<String>,
    where_clause: Option<&Expr>,
    stats: &dyn PatternStats,
) {
    let mut bound = prebound.clone();
    // The MATCH's conjunct view, derived once for every orientation this
    // MATCH prices: the where clause is loop-invariant, so each
    // candidate's `choose_orientation` reads this slice instead of
    // re-flattening the `AND` tree per call.
    let where_conjuncts = where_conjuncts_of(where_clause);
    let mut pending: Vec<(usize, PathPattern)> =
        std::mem::take(patterns).into_iter().enumerate().collect();
    let mut chosen = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        // The bound set stays the loop's mutable state; the ladder reads
        // it through the membership predicate (one view per iteration,
        // re-created after each chosen pattern extends the set).
        let bound_view = |name: &str| bound.contains(name);
        // The greedy candidate rule without the collected index `Vec`s
        // this used to build per iteration: when a pattern is already
        // chosen and some pending pattern connects to the bound set, the
        // connected patterns are the candidates, otherwise every pending
        // pattern is (the first choice is unmade or nothing connects).
        let connected_exists = !chosen.is_empty()
            && pending
                .iter()
                .any(|(_, path)| pattern_touches(path, &bound_view));
        // The first (cost, written) minimum wins — `min_by_key` settles
        // it over the filtered candidates without collecting them.
        let (index, (reversed, _)) = pending
            .iter()
            .enumerate()
            .filter(|(_, (_, path))| !connected_exists || pattern_touches(path, &bound_view))
            .map(|(index, (_, path))| {
                (
                    index,
                    choose_orientation(path, &bound_view, &where_conjuncts, stats),
                )
            })
            .min_by_key(|&(index, (_, cost))| (cost, pending[index].0))
            .expect("candidate set is non-empty");
        let (_, mut path) = pending.remove(index);
        if reversed {
            reverse_path(&mut path);
        }
        add_pattern_vars(&mut bound, &path);
        chosen.push(path);
    }
    *patterns = chosen;
}

/// Add one pattern's variables to a binding set — the ordering pass's own
/// incremental scope, fed pattern by pattern as `order_patterns` commits
/// each chosen orientation. The canonical IR walk supplies the names for
/// both pattern representations.
fn add_pattern_vars(bound: &mut HashSet<String>, p: &PathPattern) {
    // Map-bridge feeds: the bound sets stay String-keyed; every IR name
    // renders once, at its push.
    bound.extend(
        relate_ir::pattern_anchor_variables(p)
            .into_iter()
            .map(|name| name.to_string()),
    );
}

#[cfg(test)]
mod tests;
