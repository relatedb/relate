//! The endpoint pricing ladder: one canonical estimate of the rows an
//! element produces, from a bound variable down to the unmeasured rank
//! sentinels, and the seek picks the plan tree narrates from the same
//! derivation.

use relate_ir::{orient, pattern_prop_key, var_prop_named, BinaryOp, Expr, NodePattern};
use relate_names::{NamedToken, SmallName};

use super::selectivity;
use crate::stats::{ContainsCount, PatternStats, Provenance};

/// Synthetic row counts for the rank fallback. A higher rank is cheaper,
/// and a known row count below the label sentinel beats an unknown label.
const RANK_EQ: u128 = 1 << 20;
const RANK_LABEL: u128 = 1 << 48;
pub(super) const RANK_BARE: u128 = 1 << 62;

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
pub fn crossover_scan_rows<L>(
    labels: impl IntoIterator<Item = L>,
    rows_of: impl Fn(&L) -> Option<u64>,
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
    /// A seed whose unseekable filter still pays the whole-label scan: `rows`
    /// the estimated output, `scan` the label rows the filter reads, and
    /// `basis` where `rows` came from — a sampled `CONTAINS` count, the
    /// key statistics, or the default table. The walk compounds the two
    /// costs as `max(scan, output x degree)` (ADR-0474).
    Scanned {
        rows: u128,
        scan: u128,
        basis: Basis,
    },
    Rank(u128),
}

/// Where an estimate's rows came from, ordered by trust: a larger variant
/// is more trustworthy. The ordering gates strategies and breaks ties
/// between orientations the cost model prices alike; it never overrides a
/// strictly cheaper cost (a default can win an anchor it genuinely prices
/// lower).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Basis {
    /// An unmeasured rank sentinel: no label count at all.
    Rank,
    /// The default selectivity table (ADR-0694).
    Default,
    /// A sampled count (a `CONTAINS` scan sample).
    Sampled,
    /// The per-key statistics, tagged with their own provenance (ADR-0696).
    Statistic(Provenance),
    /// A count read off an index or the label's own row count.
    Measured,
}

impl Estimate {
    /// The trust the estimate's rows carry.
    pub fn basis(self) -> Basis {
        match self {
            Estimate::Measured(_) => Basis::Measured,
            Estimate::Scanned { basis, .. } => basis,
            Estimate::Rank(_) => Basis::Rank,
        }
    }
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
    // The conjunct view feeds the three where-clause levels (the seek
    // pick's window arm, CONTAINS, the default selectivity table); it is
    // cloned for all but the last, never collected — this runs once per (pattern, row) in the
    // executor's cheapest-start loop, which stays allocation-free
    // (LABS-351, stellar F-3-1).
    let measured = |hits: Option<u64>| hits.map(u128::from).map(Estimate::Measured);
    // The equality-before-window ordering is `best_seek`'s, not this
    // chain's (stellar round 2): the first measured level prices the
    // one pick — the same derivation the plan tree's seek narration
    // matches — then CONTAINS, then label rows.
    measured(best_seek(node, where_conjuncts.clone(), stats).map(|pick| pick.rows))
        .or_else(|| contains_hits(node, where_conjuncts.clone(), stats))
        .or_else(|| label_rung(node, where_conjuncts, stats))
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
    let mut best: Option<(&SmallName, NamedToken, &Expr, u64)> = None;
    for (key, value) in pairs {
        // A value-map key enters the name domain here, beside the token
        // planning resolved for it.
        let key = pattern_prop_key(&node.prop_keys, key);
        for named in &node.labels {
            let Some(rows) = stats.equality_rows(named, &key, value) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|(_, _, _, best_rows)| rows < *best_rows)
            {
                best = Some((&named.0, key.clone(), value, rows));
            }
        }
    }
    best.map(|(label, prop, bound, rows)| SeekPick {
        label,
        prop: prop.0,
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
    let scan_rows = crossover_scan_rows(node.labels.iter(), |label| stats.label_rows(label));
    // The argmin kept as references until the winner is known — the same
    // winner-only discipline as `best_equality` above: one property clone
    // for the pick that wins, none for the candidates that do not.
    let mut best: Option<(&SmallName, NamedToken, BinaryOp, &Expr, u64)> = None;
    for expr in where_conjuncts {
        let Some((prop, op, bound)) = range_prop(expr, var) else {
            continue;
        };
        for named in &node.labels {
            let Some(rows) = stats.range_rows(named, &prop, op, bound) else {
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
                best = Some((&named.0, prop.clone(), op, bound, rows));
            }
        }
    }
    best.map(|(label, prop, op, bound, rows)| SeekPick {
        label,
        prop: prop.0,
        op,
        bound,
        rows,
    })
}

/// Whether the stats can already price this conjunct on the node: a
/// plain `var.key = c` the equality level reads, or a comparison the
/// range level reads (a window the crossover gate declined is still a
/// measurement). The default selectivity table prices only what this
/// declines, so servability has one spelling — the recognizers and
/// probes of the levels above.
pub(super) fn priced_by_stats(expr: &Expr, node: &NodePattern, stats: &dyn PatternStats) -> bool {
    let Some(var) = node.variable.as_ref() else {
        return false;
    };
    if let Some((prop, op, bound)) = range_prop(expr, var) {
        return node
            .labels
            .iter()
            .any(|label| stats.range_rows(label, &prop, op, bound).is_some());
    }
    let Expr::Binary {
        op: BinaryOp::Eq,
        left,
        right,
    } = expr
    else {
        return false;
    };
    orient(|e| key_named(e, var), left, right, BinaryOp::Eq).is_some_and(|(prop, bound, _)| {
        node.labels
            .iter()
            .any(|label| stats.equality_rows(label, &prop, bound).is_some())
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
fn range_prop<'a>(expr: &'a Expr, var: &SmallName) -> Option<(NamedToken, BinaryOp, &'a Expr)> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Lt | BinaryOp::Lte | BinaryOp::Gt | BinaryOp::Gte
    ) {
        return None;
    }
    orient(|e| key_named(e, var), left, right, *op).map(|(prop, bound, op)| (prop, op, bound))
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
        .flat_map(|(prop, needle)| {
            node.labels
                .iter()
                .map(move |label| (label, prop.clone(), needle))
        })
        .filter_map(|(label, prop, needle)| stats.contains_rows(label, &prop, needle))
        .min_by_key(|count| count.rows())
        .map(|count| match count {
            ContainsCount::TextIndex(rows) => Estimate::Measured(u128::from(rows)),
            ContainsCount::SampleScan { rows, scan } => Estimate::Scanned {
                rows: u128::from(rows),
                scan: u128::from(scan),
                basis: Basis::Sampled,
            },
        })
}

/// The label level: the label's rows, scaled by the selectivity of the
/// node's predicates that no measured level priced — a
/// [`Estimate::Scanned`] whose basis is the least trustworthy term's —
/// else the plain [`Estimate::Measured`] label count. `None` without a
/// label count.
fn label_rung<'a>(
    node: &NodePattern,
    where_conjuncts: impl Iterator<Item = &'a Expr>,
    stats: &dyn PatternStats,
) -> Option<Estimate> {
    let label_rows = label_hits(node, stats)?;
    let scan = u128::from(label_rows);
    // A conjunct the stats price is a measurement the label count already
    // answers for (a window the crossover gate declined); only the rest
    // reaches the pricing.
    let unpriced = where_conjuncts.filter(|expr| !priced_by_stats(expr, node, stats));
    let ctx = selectivity::Context {
        node,
        label_rows,
        stats,
    };
    Some(match selectivity::price(&ctx, unpriced) {
        Some(priced) => {
            let rows = selectivity::scale_rows(scan, priced.selectivity);
            Estimate::Scanned {
                rows,
                scan,
                basis: priced.basis,
            }
        }
        None => Estimate::Measured(scan),
    })
}

pub(super) fn label_hits(node: &NodePattern, stats: &dyn PatternStats) -> Option<u64> {
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

/// `key_of` with the key beside its plan-resolved token.
fn key_named(expr: &Expr, var: &SmallName) -> Option<NamedToken> {
    var_prop_named(expr)
        .filter(|(name, _)| *name == var)
        .map(|(_, key)| key)
}

fn contains_prop<'a>(expr: &'a Expr, var: &SmallName) -> Option<(NamedToken, &'a Expr)> {
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
    key_named(left, var).map(|key| (key, right.as_ref()))
}

pub(super) fn endpoint_bound(node: &NodePattern, bound: Bound<'_>) -> bool {
    node.variable
        .as_ref()
        .is_some_and(|var| var.with_str(|text| bound(text)))
}
