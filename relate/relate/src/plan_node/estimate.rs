//! The MATCH operator's estimate, read from the planner's pricing ladder
//! (ADR-0708). The planner orders each clause's patterns by
//! [`endpoint_estimate`] and walks them by [`walk_cost`]; the plan tree
//! narrates the same derivation instead of keeping a second rule, so
//! EXPLAIN's estimate is the planner's pick by construction.
//!
//! The estimate is the rows of the pattern's start element — the first
//! node of the (already ordered) single pattern. `None` where the ladder
//! only ranks the element (no count) and for a multi-pattern clause,
//! whose combined size no ladder level covers.

use relate_ir::{Expr, PathPattern};
use relate_model::wire::EstimateProvenance;
use relate_planner::{endpoint_estimate, where_conjuncts_of, Basis, Estimate};

use crate::stats::PlanStats;

/// An estimate and the ladder level that produced it — one fact, so a
/// node holds both or neither (ADR-0708).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanEstimate {
    /// The estimated rows.
    pub rows: u64,
    /// The planner ladder level behind `rows`.
    pub provenance: EstimateProvenance,
}

/// The one u128→u64 saturation for displayed rows: clamp, then convert —
/// never a silent `unwrap_or` that hides the width policy.
pub(super) fn clamp_rows(rows: u128) -> u64 {
    u64::try_from(rows.min(u128::from(u64::MAX))).expect("clamped to u64")
}

impl PlanEstimate {
    /// An index count: the seek's own rows, a measurement by construction
    /// (`best_seek` reads the index), which the fused steps chain off.
    pub(crate) fn measured(rows: u64) -> Self {
        Self {
            rows,
            provenance: EstimateProvenance::Measured,
        }
    }

    /// A ladder estimate as a displayable one. The one mapping from the
    /// planner's `Estimate` variants to the wire provenance — exhaustive,
    /// so a new variant fails to compile here. A rank sentinel is an
    /// ordering key, not a count: it answers `None`.
    pub(super) fn from_ladder(estimate: Estimate) -> Option<Self> {
        // The estimate's own output rows — never `seed_rows`, which is the
        // walk *cost* and floors a sampled or defaulted seed at its scan.
        // The trust is the planner's own `Estimate::basis()`, decomposed
        // once; the rows are the variant's own field.
        let provenance = match estimate.basis() {
            Basis::Measured => EstimateProvenance::Measured,
            Basis::Sampled => EstimateProvenance::Sampled,
            Basis::Statistic(_) => EstimateProvenance::Statistic,
            Basis::Default => EstimateProvenance::Defaulted,
            // A rank sentinel is an ordering key, not a count.
            Basis::Rank => return None,
        };
        let (Estimate::Measured(rows) | Estimate::Scanned { rows, .. } | Estimate::Rank(rows)) =
            estimate;
        Some(Self {
            rows: clamp_rows(rows),
            provenance,
        })
    }
}

/// The ladder's estimate for a single pattern's start element — the
/// planner's own call, over the clause's `WHERE` conjuncts, with no
/// variable bound (EXPLAIN plans the statement standalone).
pub(super) fn start_estimate(
    pattern: &PathPattern,
    where_clause: Option<&Expr>,
    stats: &dyn PlanStats,
) -> Option<PlanEstimate> {
    let start = pattern.nodes.first()?;
    let conjuncts = where_conjuncts_of(where_clause);
    PlanEstimate::from_ladder(endpoint_estimate(
        start,
        &|_| false,
        conjuncts.iter().copied(),
        stats,
    ))
}

/// A MATCH clause's estimate: the start element's, for a single pattern.
pub(super) fn match_estimate(
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    stats: &dyn PlanStats,
) -> Option<PlanEstimate> {
    let [single] = patterns else {
        return None;
    };
    start_estimate(single, where_clause, stats)
}

impl super::PlanNode {
    /// Stamp an optional ladder estimate: rows and provenance together,
    /// or neither.
    pub(super) fn with_estimate_of(mut self, estimate: Option<PlanEstimate>) -> Self {
        self.estimate = estimate;
        self
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_ir::{BinaryOp, Expr};
    use relate_planner::{endpoint_estimate, where_conjuncts_of};

    use super::super::fixtures::{compact, comparison, node, seeking};
    use super::*;

    /// The mapping is one-to-one on the planner's variants; a rank
    /// sentinel is an ordering key and displays nothing.
    #[test]
    fn each_ladder_variant_names_its_provenance() {
        let rows = |e| PlanEstimate::from_ladder(e).map(|p| (p.rows, p.provenance));
        assert_eq!(
            rows(Estimate::Measured(7)),
            Some((7, EstimateProvenance::Measured))
        );
        assert_eq!(
            rows(Estimate::Scanned {
                rows: 3,
                scan: 90,
                basis: Basis::Sampled
            }),
            Some((3, EstimateProvenance::Sampled))
        );
        assert_eq!(
            rows(Estimate::Scanned {
                rows: 4,
                scan: 60,
                basis: Basis::Statistic(relate_planner::Provenance::Sketched)
            }),
            Some((4, EstimateProvenance::Statistic))
        );
        assert_eq!(
            rows(Estimate::Scanned {
                rows: 5,
                scan: 50,
                basis: Basis::Default
            }),
            Some((5, EstimateProvenance::Defaulted))
        );
        assert_eq!(rows(Estimate::Rank(1 << 48)), None);
    }

    /// EXPLAIN's estimate IS the ladder's pick for the pattern's start
    /// element (ADR-0708): the label scan, the index window and the
    /// default-selectivity level each equal `endpoint_estimate` called
    /// directly, with the matching provenance.
    #[test]
    fn the_estimate_equals_the_planners_pick() {
        let stats = seeking(
            &[("User", 100)],
            &[("User", "id", 1)],
            &[("User", "age", 5)],
            BTreeMap::new(),
        );
        let direct = |pattern: &PathPattern, w: Option<&Expr>| {
            let conjuncts = where_conjuncts_of(w);
            endpoint_estimate(
                &pattern.nodes[0],
                &|_| false,
                conjuncts.iter().copied(),
                &stats,
            )
        };

        // A bare label: its row count, measured.
        let scan = compact(vec![node(Some("n"), &["User"])], Vec::new());
        let got = match_estimate(std::slice::from_ref(&scan), None, &stats).expect("priced");
        assert_eq!(
            Estimate::Measured(u128::from(got.rows)),
            direct(&scan, None)
        );
        assert_eq!(
            (got.rows, got.provenance),
            (100, EstimateProvenance::Measured)
        );

        // An index window: the seek's count, measured.
        let windowed = compact(vec![node(Some("n"), &["User"])], Vec::new());
        let where_age = comparison("n", "age", BinaryOp::Gt, Expr::Int(5));
        let got = match_estimate(std::slice::from_ref(&windowed), Some(&where_age), &stats)
            .expect("priced");
        assert_eq!(
            Estimate::Measured(u128::from(got.rows)),
            direct(&windowed, Some(&where_age))
        );
        assert_eq!(
            (got.rows, got.provenance),
            (5, EstimateProvenance::Measured)
        );

        // A predicate no index serves: the default-selectivity level.
        let where_city = comparison("n", "city", BinaryOp::Eq, Expr::String("London".into()));
        let got =
            match_estimate(std::slice::from_ref(&scan), Some(&where_city), &stats).expect("priced");
        assert!(
            matches!(direct(&scan, Some(&where_city)), Estimate::Scanned { rows, basis: Basis::Default, .. } if rows == u128::from(got.rows))
        );
        assert_eq!(got.provenance, EstimateProvenance::Defaulted);
        assert!(got.rows < 100, "the default narrows the label count");
    }

    /// No count, no estimate: an unmeasured label and a multi-pattern
    /// clause display nothing — never a fabricated number.
    #[test]
    fn an_unpriced_element_displays_no_estimate() {
        let stats = seeking(&[("User", 100)], &[], &[], BTreeMap::new());
        let unknown = compact(vec![node(None, &["City"])], Vec::new());
        assert_eq!(match_estimate(&[unknown], None, &stats), None);
        let pair = vec![
            compact(vec![node(Some("a"), &["User"])], Vec::new()),
            compact(vec![node(Some("b"), &["User"])], Vec::new()),
        ];
        assert_eq!(match_estimate(&pair, None, &stats), None);
    }
}
