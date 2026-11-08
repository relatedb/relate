//! The seek/endpoint test family (ADR-0486's range levels and
//! ADR-0493's crossover): the level stubs that record what the stats
//! saw, the pick derivations, and the crossover gates — split from the
//! order tests' root (the file crossed the family-growth line the
//! stellar structure round named; the shared scaffolding — Stub (the
//! family's one data-driven stats fixture, the range table included),
//! patterns_of, first_label — stays in the root, and the parse
//! front half (with_leading_match) lives there too: the root's
//! first_hop rides it and the child reaches up, never down).

use std::cell::Cell;

use relate_names::SmallName;

use super::super::*;
use super::*;
use crate::stats::{ContainsCount, PatternStats};

/// The seek/endpoint tests' one "nothing is bound" fact — the `Bound`
/// every endpoint estimate is called with.
fn unbound(_: &str) -> bool {
    false
}

/// The stats stub behind the equality-parameter test: records whether
/// the `$city` parameter reached `equality_rows` unresolved.
struct SawParameter(Cell<bool>);

impl PatternStats for SawParameter {
    fn label_rows(&self, _: &SmallName) -> Option<u64> {
        None
    }
    fn equality_rows(&self, label: &SmallName, property: &SmallName, value: &Expr) -> Option<u64> {
        if label == "Place"
            && property == "name"
            && matches!(value, Expr::Parameter(name) if name == "city")
        {
            self.0.set(true);
            return Some(1);
        }
        None
    }
    fn contains_rows(&self, _: &SmallName, _: &SmallName, _: &Expr) -> Option<ContainsCount> {
        None
    }
    fn rel_type_rows(&self, _: &SmallName) -> Option<u64> {
        None
    }
}

#[test]
fn equality_parameters_reach_stats() {
    let stats = SawParameter(Cell::new(false));
    let _ = patterns_of("MATCH (l:Place) WHERE l.name = $city RETURN l", &stats);
    assert!(stats.0.get());
}

/// The stats stub behind the range-level tests: a bare `Hub` label and a
/// comparison window on `Target.x`, so the window is the only signal
/// that can outrank the bare label (the `SawParameter` shape, one level
/// up).
struct SawRange(Cell<bool>);

impl PatternStats for SawRange {
    fn label_rows(&self, label: &SmallName) -> Option<u64> {
        (label == "Hub").then_some(200_000)
    }

    fn equality_rows(&self, _: &SmallName, _: &SmallName, _: &Expr) -> Option<u64> {
        None
    }

    /// Records the raw conjunct the way `SawParameter` does: the bound
    /// arrives unresolved (a parameter — `seek_value` is the
    /// implementor's), with the op the recognizer oriented.
    fn range_rows(
        &self,
        label: &SmallName,
        property: &SmallName,
        op: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        if label == "Target"
            && property == "x"
            && op == BinaryOp::Lt
            && matches!(bound, Expr::Parameter(name) if name == "limit")
        {
            self.0.set(true);
            return Some(3);
        }
        None
    }

    fn contains_rows(&self, _: &SmallName, _: &SmallName, _: &Expr) -> Option<ContainsCount> {
        None
    }

    fn rel_type_rows(&self, _: &SmallName) -> Option<u64> {
        None
    }
}

/// ADR-0486's ladder level: a comparison-predicated element outranks a
/// bare label element and orients the pattern to start there — the
/// window (3) beats the `Hub` label's 200,000 rows, where the
/// pre-level ladder priced the comparison side at its unmeasured label
/// rank and kept the `Hub` start. The parameterized bound reaches the
/// new stats method (the `equality_parameters_reach_stats` mirror), and
/// the mirrored spelling (`$limit > t.x`) reaches it with the op
/// oriented — the `range_seek_expr` orientation policy.
#[test]
fn comparison_parameters_reach_stats_and_win_the_start() {
    let stats = SawRange(Cell::new(false));
    let patterns = patterns_of(
        "MATCH (h:Hub)-[:R]->(t:Target) WHERE t.x < $limit RETURN h",
        &stats,
    );
    assert!(stats.0.get());
    assert_eq!(first_label(&patterns[0]), "Target");
    assert_eq!(
        patterns[0].nodes.last().unwrap().labels[0].to_string(),
        "Hub"
    );

    // The mirrored operand: property right, `>` mirrored to `<`.
    let stats = SawRange(Cell::new(false));
    let patterns = patterns_of(
        "MATCH (h:Hub)-[:R]->(t:Target) WHERE $limit > t.x RETURN h",
        &stats,
    );
    assert!(stats.0.get(), "the mirrored conjunct orients to Lt");
    assert_eq!(first_label(&patterns[0]), "Target");
}

/// The level's rank (ADR-0486): between the equality point read and the
/// sampled `CONTAINS` — the ladder's measured level takes `best_seek`'s
/// pick (the point read first, the window above CONTAINS), pinned here
/// against a stub that can answer all three levels.
#[test]
fn the_range_window_ranks_between_equality_and_contains() {
    let stats = Stub {
        equals: &[("N", "id", "7", 2)],
        ranges: &[("N", "x", BinaryOp::Lt, 10, 5)],
        contains: &[(
            "N",
            "w",
            "needle",
            ContainsCount::SampleScan {
                rows: 1,
                scan: 1_000,
            },
        )],
        ..Stub::EMPTY
    };
    // Equality above: a point read is the degenerate tightest window.
    assert_eq!(
        estimate_of(
            "MATCH (n:N {id: '7'}) WHERE n.x < 10 AND n.w CONTAINS 'needle' RETURN n",
            &stats
        ),
        Estimate::Measured(2)
    );
    // The window above CONTAINS: an exact seekable count outranks a
    // sampled extrapolation carrying a scan floor.
    assert_eq!(
        estimate_of(
            "MATCH (n:N) WHERE n.x < 10 AND n.w CONTAINS 'needle' RETURN n",
            &stats
        ),
        Estimate::Measured(5)
    );
    // Without the window the sampled compound is the level (the control).
    assert_eq!(
        estimate_of("MATCH (n:N) WHERE n.w CONTAINS 'needle' RETURN n", &stats),
        Estimate::SampleScan {
            rows: 1,
            scan: 1_000
        }
    );
}

/// The pick derivation itself (ADR-0486): `best_seek` owns the
/// equality-before-window ordering — the anchored point read wins when
/// both levels serve, carrying the winning pair's identity; the window
/// when only it serves; `None` when neither does. The ladder prices the
/// same pick (the rank test above) and the plan tree's seek narration
/// matches it (the top-level crate's seek tests).
#[test]
fn best_seek_picks_equality_before_the_window() {
    let stats = Stub {
        equals: &[("N", "id", "7", 2)],
        ranges: &[("N", "x", BinaryOp::Lt, 10, 5)],
        ..Stub::EMPTY
    };
    // Both levels serve: the point read wins — a point lookup is the
    // degenerate tightest window on the same index, its op the fold's.
    assert_pick(
        "MATCH (n:N {id: '7'}) WHERE n.x < 10 RETURN n",
        &stats,
        Some(("N", "id", BinaryOp::Eq, 2)),
    );
    // Only the window serves: the winning conjunct's identity, the op
    // as the recognizer oriented it.
    assert_pick(
        "MATCH (n:N) WHERE n.x < 10 RETURN n",
        &stats,
        Some(("N", "x", BinaryOp::Lt, 5)),
    );
    // Neither level serves (an unindexed CONTAINS only): no pick.
    assert_pick(
        "MATCH (n:N) WHERE n.w CONTAINS 'needle' RETURN n",
        &stats,
        None,
    );
}

/// The pick consumer's one spelling: parse, derive the pick, and
/// assert its identity inside the front half's closure (the pick
/// borrows the parsed pattern, so the comparison runs while the query
/// is alive).
fn assert_pick(cypher: &str, stats: &Stub, want: Option<(&str, &str, BinaryOp, u64)>) {
    with_leading_match(cypher, |pattern, w| {
        // The pick's names render through Display for the literal
        // expectation below.
        let got = best_seek(&pattern.nodes[0], where_conjuncts_of(w), stats).map(|pick| {
            (
                pick.label.to_string(),
                pick.prop.to_string(),
                pick.op,
                pick.rows,
            )
        });
        let want =
            want.map(|(label, prop, op, rows)| (label.to_string(), prop.to_string(), op, rows));
        assert_eq!(got, want);
    });
}

// ADR-0218: with no bound node and equal coarse ranks, the part orients
// toward the end whose label is rarer.

/// The crossover policy itself (ADR-0493): a window at or above a
/// quarter of its label's rows declines the seek — except a small
/// window (at or under the floor), which always seeks.
#[test]
fn range_seek_wins_below_a_quarter_and_under_the_floor() {
    // A minority window seeks: 999 of 4,000.
    assert!(range_seek_wins(999, 4_000));
    // Exactly a quarter declines: the scan with the residual filter is
    // cheaper from here up.
    assert!(!range_seek_wins(1_000, 4_000));
    assert!(!range_seek_wins(3_999, 4_000));
    // The floor: a small window seeks regardless of the label's size —
    // anchored point reads and small IN windows stay on the index.
    assert!(range_seek_wins(200, 400));
    assert!(range_seek_wins(1, 2));
}

/// One endpoint estimate over the parsed pieces — the single
/// `endpoint_estimate` spelling every consumer rides (with the
/// `unbound` fact every endpoint test uses).
fn endpoint_of(
    pattern: &PathPattern,
    where_clause: Option<&Expr>,
    stats: &dyn PatternStats,
) -> Estimate {
    endpoint_estimate(
        &pattern.nodes[0],
        &unbound,
        where_conjuncts_of(where_clause).iter().copied(),
        stats,
    )
}

/// One parse → leading MATCH → endpoint estimate.
fn estimate_of(cypher: &str, stats: &dyn PatternStats) -> Estimate {
    with_leading_match(cypher, |pattern, where_clause| {
        endpoint_of(pattern, where_clause, stats)
    })
}

/// ADR-0493's ladder gate: a majority window's level declines, so the
/// ladder falls through to the label rows (the scan's population); a
/// minority window still wins the anchor, and the pick derivation
/// agrees (no seek narrated for a gated window).
#[test]
fn the_crossover_gates_the_range_level() {
    const QUERY: &str = "MATCH (n:N) WHERE n.x < 10 RETURN n";
    // The one fact the cases vary: the window.
    fn window(ranges: &'static [(&'static str, &'static str, BinaryOp, i64, u64)]) -> Stub {
        Stub {
            labels: &[("N", 100_000)],
            ranges,
            ..Stub::EMPTY
        }
    }
    // A majority window (56,802 of 100,000 — the measured
    // `age >= 18` shape): the level declines, the label rows price the
    // element, and no seek is picked to narrate.
    let stats = window(&[("N", "x", BinaryOp::Lt, 10, 56_802)]);
    assert_eq!(estimate_of(QUERY, &stats), Estimate::Measured(100_000));
    assert_pick(QUERY, &stats, None);
    // A minority window (170 of 100,000 — the measured `age > 50`
    // shape): the window still wins and narrates its seek.
    let stats = window(&[("N", "x", BinaryOp::Lt, 10, 170)]);
    assert_eq!(estimate_of(QUERY, &stats), Estimate::Measured(170));
    assert_pick(QUERY, &stats, Some(("N", "x", BinaryOp::Lt, 170)));
}

/// ADR-0493's population rule on a multi-label node: the window gates
/// against the pattern's smallest label (the conjunction the scan
/// serves), not the pick's own label — and the per-candidate gate runs
/// before the min, so a gated-out smallest window cannot hide a
/// passing larger one.
#[test]
fn the_crossover_gates_multi_label_nodes_against_the_smallest_label() {
    // The one fact the cases vary: the window.
    fn multi_label_estimate(
        ranges: &'static [(&'static str, &'static str, BinaryOp, i64, u64)],
    ) -> Estimate {
        let stats = Stub {
            labels: &[("A", 100_000), ("B", 4_000)],
            ranges,
            ..Stub::EMPTY
        };
        estimate_of("MATCH (n:A:B) WHERE n.x < 10 RETURN n", &stats)
    }
    // A window of 1,500 against the smallest label's 4,000: at or
    // above a quarter — the level declines and the label rows price
    // the element (not the pick's own 100,000-row A, not the window).
    assert_eq!(
        multi_label_estimate(&[("A", "x", BinaryOp::Lt, 10, 1_500)]),
        Estimate::Measured(4_000)
    );
    // A window of 900 still seeks: 900 × 4 < 4,000.
    assert_eq!(
        multi_label_estimate(&[("A", "x", BinaryOp::Lt, 10, 900)]),
        Estimate::Measured(900)
    );
}
