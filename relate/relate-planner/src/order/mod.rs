//! Cardinality estimates and MATCH pattern order.
//!
//! A missing count falls back to a rank sentinel. Measured counts may be
//! scaled by relationship degree; rank sentinels stay unscaled.

use std::collections::HashSet;

use relate_ir::{
    key_of, orient, BinaryOp, Expr, NodePattern, PathPattern, PatternElement, RelPattern,
};
use relate_names::SmallName;

use crate::rewrite::add_pattern_vars;
use crate::stats::{ContainsCount, NoPatternStats, PatternStats};

/// Synthetic row counts for the rank fallback. A higher rank is cheaper,
/// and a known row count below the label sentinel beats an unknown label.
const RANK_EQ: u128 = 1 << 20;
const RANK_LABEL: u128 = 1 << 48;
const RANK_BARE: u128 = 1 << 62;

/// ADR-0493's small-window floor: a range window of at most this many
/// ids always seeks — anchored point reads and small `IN` windows stay
/// on the index regardless of tiny labels, where the absolute cost
/// difference is noise.
const RANGE_SEEK_WIN_FLOOR: u64 = 256;

/// ADR-0493's range-seek crossover — the one policy both the ladder's
/// range level (here) and the executor funnel's `Seek::Range` serve
/// (pattern_walk/candidates) consult, so pricing and execution can never
/// disagree. Measured on a 1.8M-relationship social-network graph: the
/// seek's per-candidate cost
/// (~1.36 µs, each candidate materializing a full node) is ~5.2× the
/// label scan's per-node cost (~0.26 µs) — break-even ≈19%, the
/// crossover sits at 1/4 with margin. `true` = the window still wins
/// as an index seek; `false` = the label scan with the residual filter
/// is cheaper. Revisit when the seek becomes id-only (LABS-853) — the
/// fraction is expected to rise.
pub(crate) fn range_seek_wins(window: u64, label_rows: u64) -> bool {
    window <= RANGE_SEEK_WIN_FLOOR || window * 4 < label_rows
}

/// ADR-0493's crossover population — the scan a range seek replaces is
/// the pattern's label conjunction, sized by its smallest label. The
/// one fold both consumers ride (the ladder's `best_range` over the
/// planner's stats, the executor funnel's `Seek::crossover_declines`
/// over the graph's counts): `None` when the pattern names no label or
/// any label is unmeasured (no measurement, no gate — ADR-0486's
/// default). Allocation-free: the executor's cheapest-start loop runs
/// it per (pattern, row) (LABS-351).
pub fn crossover_scan_rows(
    labels: impl IntoIterator<Item = SmallName>,
    rows_of: impl Fn(&SmallName) -> Option<u64>,
) -> Option<u64> {
    let mut scan_rows: Option<u64> = None;
    for label in labels {
        let rows = rows_of(&label)?;
        scan_rows = Some(scan_rows.map_or(rows, |min| min.min(rows)));
    }
    scan_rows
}

/// ADR-0493's gate over a population: `true` when a measured scan
/// population makes the window cheaper as the label scan — the one
/// composition of [`crossover_scan_rows`] and [`range_seek_wins`] both
/// consumers apply, so pricing and execution decline the same windows.
pub fn range_seek_gated(window: u64, scan_rows: Option<u64>) -> bool {
    scan_rows.is_some_and(|scan| !range_seek_wins(window, scan))
}

/// One canonical endpoint ladder (LABS-351): a bound variable is one
/// row, then the best measured index signal (equality, the range window,
/// CONTAINS, then label rows), then the unmeasured rank sentinels.
/// Measured counts and rank sentinels share one ordered scale (the
/// constants are ordered, `direction_cost` compares them directly), so
/// both the planner's pattern ordering and the executor's start-element
/// choice consume the same estimate. `pub` for the executor's
/// `cheapest_start`.
///
/// A sampled `CONTAINS` measurement carries its own arm (the compound
/// start estimate, ADR-0474): the seed's output rows and the whole-label
/// scan the unseekable filter pays are two different costs — the scan is
/// a one-time floor, the output scales with the hops — so the walk
/// compounds them as `max(scan, output x degree)` instead of scaling the
/// seed alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Estimate {
    Measured(u128),
    /// A sampled `CONTAINS` seed: `rows` the sampled output estimate,
    /// `scan` the label-bucket rows the unindexed filter still pays.
    SampleScan {
        rows: u128,
        scan: u128,
    },
    Rank(u128),
}

/// The where clause as its conjunct list — the ladder's CONTAINS level
/// reads individual conjuncts, the executor passes its residual conjuncts
/// in this shape natively, and the plan tree's seek narration (ADR-0486)
/// reads the same view, so no consumer re-flattens the `AND` tree.
pub fn where_conjuncts_of(where_clause: Option<&Expr>) -> Vec<&Expr> {
    let mut conjuncts = Vec::new();
    if let Some(expr) = where_clause {
        each_conjunct(expr, &mut |expr| conjuncts.push(expr));
    }
    conjuncts
}

fn unmeasured_rows(node: &NodePattern) -> u128 {
    let equality = matches!(&node.properties, Some(Expr::Map(pairs)) if !pairs.is_empty());
    if equality {
        RANK_EQ
    } else if !node.labels.is_empty() {
        RANK_LABEL
    } else {
        RANK_BARE
    }
}

/// The bound check as a membership predicate, not a concrete collection:
/// the executor's `cheapest_start` runs once per (pattern, row) and must
/// not clone the row's keys into a `HashSet` per call (LABS-351, stellar
/// F-3-1).
pub type Bound<'a> = &'a dyn Fn(&str) -> bool;

pub fn endpoint_estimate<'a>(
    node: &'a NodePattern,
    bound: Bound<'_>,
    where_conjuncts: impl Iterator<Item = &'a Expr> + Clone,
    stats: &dyn PatternStats,
) -> Estimate {
    if endpoint_bound(node, bound) {
        return Estimate::Measured(1);
    }
    // The conjunct view feeds the two where-clause levels (the seek
    // pick's window arm, CONTAINS); it is cloned for the second level,
    // never collected — this runs once per (pattern, row) in the
    // executor's cheapest-start loop, which stays allocation-free
    // (LABS-351, stellar F-3-1).
    let measured = |hits: Option<u64>| hits.map(u128::from).map(Estimate::Measured);
    // The equality-before-window ordering is `best_seek`'s, not this
    // chain's (stellar round 2): the first measured level prices the
    // one pick — the same derivation the plan tree's seek narration
    // matches — then CONTAINS, then label rows.
    measured(best_seek(node, where_conjuncts.clone(), stats).map(|pick| pick.rows()))
        .or_else(|| contains_hits(node, where_conjuncts, stats))
        .or_else(|| measured(label_hits(node, stats)))
        .unwrap_or_else(|| Estimate::Rank(unmeasured_rows(node)))
}

/// The tightest indexed equality on the folded property map (ADR-0024's
/// fold — the degenerate tightest window on the same index, ADR-0486):
/// the minimum `equality_rows` over the map's pairs and the node's
/// labels, returned as the pick — the winning pair carrying the point
/// read's [`BinaryOp::Eq`] — so `best_seek` consumes the same
/// derivation the ladder prices, never a second spelling of the min
/// policy. `None` when the stats cannot speak for any pair.
fn best_equality<'a>(node: &'a NodePattern, stats: &dyn PatternStats) -> Option<SeekPick<'a>> {
    let Expr::Map(pairs) = node.properties.as_ref()? else {
        return None;
    };
    // The argmin kept as references until the winner is known: only the
    // winning pick's property is classified into an owned name — building
    // a `SeekPick` (a property name clone) per (pair, label) candidate
    // allocated for a minimum only one of them wins. Consult order and
    // the first-wins tie-break are the collected form's.
    let mut best: Option<(&SmallName, SmallName, &Expr, u64)> = None;
    for (key, value) in pairs {
        // A value-map key enters the name domain here, classified once
        // (the stats and the pick speak names).
        let key = SmallName::from_name(key.as_str());
        for label in &node.labels {
            let Some(rows) = stats.equality_rows(label, &key, value) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|(_, _, _, best_rows)| rows < *best_rows)
            {
                best = Some((label, key.clone(), value, rows));
            }
        }
    }
    best.map(|(label, prop, bound, rows)| SeekPick {
        label,
        prop,
        op: BinaryOp::Eq,
        bound,
        rows,
    })
}

/// The range window level (ADR-0486) as an argmin over the where
/// clause's conjuncts: the label, property, op, bound, and count of the
/// servable comparison conjunct with the minimum count across the start
/// element's labels — the `best_equality`/`contains_hits` min policy.
/// The count is the index's posting-length window: an exact, seekable
/// measurement, so it consumes as [`Estimate::Measured`] paying no scan
/// floor, and ranks above the sampled `CONTAINS` level (an exact count
/// outranks an extrapolation carrying a scan floor — the seekable side
/// must win the anchor) while the equality point read above it stays
/// the degenerate tightest window on the same index. The plan tree's
/// seek narration consumes this same derivation through [`best_seek`],
/// never a second spelling of the policy. Module-private beside its
/// level sibling `best_equality`: the level derivations are
/// [`best_seek`]'s to compose, and the ladder and the seek narration
/// both consume the pick.
fn best_range<'a>(
    node: &'a NodePattern,
    where_conjuncts: impl IntoIterator<Item = &'a Expr>,
    stats: &dyn PatternStats,
) -> Option<SeekPick<'a>> {
    let var = node.variable.as_ref()?;
    // ADR-0493's crossover population, folded once per pattern — the
    // one rule the executor funnel's gate applies too.
    let scan_rows = crossover_scan_rows(node.labels.iter().cloned(), |l| stats.label_rows(l));
    // The argmin kept as references until the winner is known — the same
    // winner-only discipline as `best_equality` above: one property clone
    // for the pick that wins, none for the candidates that do not.
    let mut best: Option<(&SmallName, &SmallName, BinaryOp, &Expr, u64)> = None;
    for expr in where_conjuncts {
        let Some((prop, op, bound)) = range_prop(expr, var) else {
            continue;
        };
        for label in &node.labels {
            let Some(rows) = stats.range_rows(label, prop, op, bound) else {
                continue;
            };
            // ADR-0493's crossover per candidate, BEFORE the min: a
            // gated-out smallest window must not hide a passing larger
            // one on another label or conjunct. A window at or above a
            // quarter of the scan population (above the small-window
            // floor) is cheaper as the label scan with the residual
            // filter — the candidate declines, the ladder falls
            // through to CONTAINS / label rows.
            if !range_seek_gated(rows, scan_rows)
                && best
                    .as_ref()
                    .is_none_or(|(_, _, _, _, best_rows)| rows < *best_rows)
            {
                best = Some((label, prop, op, bound, rows));
            }
        }
    }
    best.map(|(label, prop, op, bound, rows)| SeekPick {
        label,
        prop: prop.clone(),
        op,
        bound,
        rows,
    })
}

/// The comparison-conjunct recognizer (ADR-0486's v1 floor): the var's
/// own property under one of the four comparison ops, operand-oriented
/// the way the executor's `range_seek_expr` orients — property on either
/// side, the op mirrored for a right-side property (`5 > n.x` is
/// `n.x < 5`) — the planner/executor twin `contains_prop` vs
/// `string_seek_expr` already is (LABS-351). Module-private beside its
/// twin: reachable only through [`best_range`], which feeds the
/// ladder's window level and the seek narration through [`best_seek`],
/// so the tree-build consumes this one spelling, never a second. `Eq`
/// declines (the property-map fold and `equality_rows` own the point
/// read), as do `IN` and `STARTS WITH` (deferred, ADR-0486).
fn range_prop<'a>(expr: &'a Expr, var: &SmallName) -> Option<(&'a SmallName, BinaryOp, &'a Expr)> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Lt | BinaryOp::Lte | BinaryOp::Gt | BinaryOp::Gte
    ) {
        return None;
    }
    orient(|e| key_of(e, var), left, right, *op).map(|(prop, bound, op)| (prop, op, bound))
}

/// One indexed seek the start element's predicates resolve to
/// (ADR-0486): the winning `(label, property)` index pair, the op, the
/// bound, and the count — the equality-before-window pick, the
/// anchored-equality point read (a point lookup, the degenerate
/// tightest window on the same index, carrying [`BinaryOp::Eq`])
/// outranking the comparison window, an ordering spelled once, in
/// [`best_seek`], never again as separate control flow in a consumer:
/// the ladder's first measured level ([`endpoint_estimate`]) and the
/// plan tree's seek narration (`index_seek` in the top-level crate)
/// both consume this one pick.
pub struct SeekPick<'a> {
    /// The label of the winning `(label, property)` index pair.
    pub label: &'a SmallName,
    /// The indexed property the seek serves. Owned: the property map's
    /// value-domain key is classified into the name domain at the pick
    /// (there is nothing in the node pattern to borrow it from).
    pub prop: SmallName,
    /// The seek's op: [`BinaryOp::Eq`] for the folded point read, the
    /// oriented comparison op for the window.
    pub op: BinaryOp,
    /// The winning pair's bound expression — as folded for the point
    /// read, as recognized for the window.
    pub bound: &'a Expr,
    /// The index's count for the seek — the hit count for the point
    /// read, the posting-length window for the comparison.
    pub rows: u64,
}

impl SeekPick<'_> {
    /// The pick's own count — the measured rows the ladder prices the
    /// pick with. Module-private beside the level derivations: the
    /// ladder ([`endpoint_estimate`]) is the accessor's one caller; the
    /// plan tree's narration destructures the fields.
    fn rows(&self) -> u64 {
        self.rows
    }
}

/// The one equality-before-window derivation (ADR-0486): the pick a
/// seek-capable start element's predicates resolve to — an indexed
/// equality on the folded map first (a point lookup is the degenerate
/// tightest window on the same index, so it outranks), else the
/// servable comparison conjunct's window. The ladder's measured level
/// and the plan tree's seek narration consume this derivation, never a
/// second spelling of the ranking.
pub fn best_seek<'a>(
    node: &'a NodePattern,
    where_conjuncts: impl IntoIterator<Item = &'a Expr>,
    stats: &dyn PatternStats,
) -> Option<SeekPick<'a>> {
    best_equality(node, stats).or_else(|| best_range(node, where_conjuncts, stats))
}

/// The CONTAINS level with its provenance: the minimum count across the
/// node's labels, kept as the measured [`ContainsCount`] so the ladder can
/// price the sampled form's scan floor (the compound, ADR-0474) while the
/// TEXT-indexed form stays a plain measured count.
fn contains_hits<'a>(
    node: &NodePattern,
    where_conjuncts: impl IntoIterator<Item = &'a Expr>,
    stats: &dyn PatternStats,
) -> Option<Estimate> {
    let var = node.variable.as_ref()?;
    where_conjuncts
        .into_iter()
        .filter_map(|expr| contains_prop(expr, var))
        .flat_map(|(prop, needle)| node.labels.iter().map(move |label| (label, prop, needle)))
        .filter_map(|(label, prop, needle)| stats.contains_rows(label, prop, needle))
        .min_by_key(|count| count.rows())
        .map(|count| match count {
            ContainsCount::TextIndex(rows) => Estimate::Measured(u128::from(rows)),
            ContainsCount::SampleScan { rows, scan } => Estimate::SampleScan {
                rows: u128::from(rows),
                scan: u128::from(scan),
            },
        })
}

fn label_hits(node: &NodePattern, stats: &dyn PatternStats) -> Option<u64> {
    node.labels
        .iter()
        .filter_map(|label| stats.label_rows(label))
        .min()
}

fn each_conjunct<'a>(expr: &'a Expr, visit: &mut impl FnMut(&'a Expr)) {
    if let Expr::Binary {
        op: BinaryOp::And,
        left,
        right,
    } = expr
    {
        each_conjunct(left, visit);
        each_conjunct(right, visit);
    } else {
        visit(expr);
    }
}

fn contains_prop<'a>(expr: &'a Expr, var: &SmallName) -> Option<(&'a SmallName, &'a Expr)> {
    let Expr::Binary {
        op: BinaryOp::Contains,
        left,
        right,
    } = expr
    else {
        return None;
    };
    // CONTAINS is operand-asymmetric (needle/haystack), so only the left
    // side is recognized — a right-side property declines like every other
    // unservable shape, matching string_seek_expr's one-sided policy; the
    // estimate falls back to the unfiltered count.
    key_of(left, var).map(|key| (key, right.as_ref()))
}

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
    let rel_type = match rel.types.as_slice() {
        [rel_type] => Some(rel_type),
        _ => None,
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
    let [rel_type] = rel.types.as_slice() else {
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

fn endpoint_bound(node: &NodePattern, bound: Bound<'_>) -> bool {
    node.variable
        .as_ref()
        .is_some_and(|var| var.with_str(|text| bound(text)))
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
        Estimate::SampleScan { rows, scan } => degree_walk(rows, hops, stats).max(scan),
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
    if orientation_locked(path) || path.nodes.is_empty() {
        return (
            false,
            direction_cost(path, false, bound, where_conjuncts, stats).0,
        );
    }
    let first_bound = endpoint_bound(path.nodes.first().expect("nodes is non-empty"), bound);
    let last_bound = endpoint_bound(path.nodes.last().expect("nodes is non-empty"), bound);
    // The written orientation is the one the elements' left-to-right
    // binding order is defined on; a peer-referenced predicate keeps it.
    // Beyond that, an orientation is forced exactly when only one
    // endpoint is bound — seed from the bound end.
    let forced = if !reversal_safe(path, bound) || (first_bound && !last_bound) {
        Some(false)
    } else if last_bound && !first_bound {
        Some(true)
    } else {
        None
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
/// bound set advance with every chosen pattern; the grown bound set is
/// returned. Equal costs keep written order.
pub(crate) fn order_patterns(
    patterns: &mut Vec<PathPattern>,
    prebound: &HashSet<String>,
    where_clause: Option<&Expr>,
    stats: &dyn PatternStats,
) -> HashSet<String> {
    let mut bound = prebound.clone();
    if patterns.is_empty() {
        return bound;
    }
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
    bound
}

#[cfg(test)]
mod tests;
