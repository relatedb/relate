//! Per-key value statistics as the planner reads them (the per-key
//! statistics ADR): the figures of one `(label, property)` over a label's
//! nodes, and the row estimate an equality predicate on the key derives
//! from them, each tagged with how it was produced.
//!
//! The collector lives in the executor; these types are the contract the
//! [`super::PatternStats`] methods `key_stats` and `equality_estimate`
//! return, so the planner never depends on how a figure was measured.

use relate_core::Value;

/// How a figure was produced, ordered by trust: a larger variant is more
/// trustworthy, so a planner level can ask `provenance >= UpperBound`
/// without matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provenance {
    /// A sketch: the distinct count is an estimate and the heavy-hitter
    /// counts are upper bounds.
    Sketched,
    /// Read off a declared index or a built equality pair: the postings
    /// are a superset of the live values (superseded versions stay until
    /// the next prune), so counts are upper bounds.
    UpperBound,
    /// Reserved: no producer yet. The equality-pair bridge that reads a
    /// built pair with no write since its build will produce it. A
    /// declared index's answer is [`Provenance::UpperBound`], since its
    /// postings keep superseded versions until the next prune.
    Exact,
}

/// A value a key holds more often than most, with a count that is an upper
/// bound on how many admitted values equal it.
#[derive(Clone, Debug, PartialEq)]
pub struct HeavyValue {
    pub value: Value,
    pub count: u64,
}

/// The statistics of one `(label, property)`, over the values the
/// collector's type gate admitted.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyStats {
    /// How the figures were produced.
    pub provenance: Provenance,
    /// Admitted values read.
    pub count: u64,
    /// Values of this key the type gate declined (long text, lists).
    pub declined: u64,
    /// Estimated distinct admitted values, never above `count`.
    pub distinct: u64,
    /// Smallest value, for a key whose values share one orderable family.
    pub min: Option<Value>,
    /// Largest value, under the same condition as `min`.
    pub max: Option<Value>,
    /// Values that are numbers: the denominator of [`KeyStats::mean`].
    pub numeric_count: u64,
    /// Sum of the integer values (saturating).
    pub int_sum: i128,
    /// Sum of the float values.
    pub float_sum: f64,
    /// The most frequent values, most frequent first.
    pub heavy: Vec<HeavyValue>,
}

impl KeyStats {
    /// The mean of the numeric values, `None` when there are none or the
    /// sum is not finite (a key holding an infinity, or `+inf` with `-inf`,
    /// has no meaningful mean).
    pub fn mean(&self) -> Option<f64> {
        if self.numeric_count == 0 {
            return None;
        }
        let sum = self.int_sum as f64 + self.float_sum;
        sum.is_finite().then(|| sum / self.numeric_count as f64)
    }

    /// The row estimate of an equality on this key, for a label of
    /// `label_rows` nodes: the value's own count when the caller found it
    /// among the heavy values (by the store's equality identity, which a
    /// plain `Value` comparison does not share: an integer and an equal
    /// float are one value there), otherwise the admitted values spread
    /// over the distinct count. Never zero for a label that has rows (a
    /// value the sketch did not see may still exist) and never above the
    /// label's rows.
    pub fn equality_rows(&self, heavy_count: Option<u64>, label_rows: u64) -> u64 {
        let rows = heavy_count.unwrap_or_else(|| self.count.div_ceil(self.distinct.max(1)));
        rows.clamp(u64::from(label_rows > 0), label_rows)
    }
}

/// A row-count estimate with its provenance: the planner's reading of an
/// equality predicate on a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowEstimate {
    pub rows: u64,
    pub provenance: Provenance,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(count: u64, distinct: u64, heavy: Vec<HeavyValue>) -> KeyStats {
        KeyStats {
            provenance: Provenance::Sketched,
            count,
            declined: 0,
            distinct,
            min: None,
            max: None,
            numeric_count: 0,
            int_sum: 0,
            float_sum: 0.0,
            heavy,
        }
    }

    #[test]
    fn provenance_orders_by_trust() {
        assert!(Provenance::Sketched < Provenance::UpperBound);
        assert!(Provenance::UpperBound < Provenance::Exact);
        assert!(Provenance::Exact >= Provenance::UpperBound);
    }

    #[test]
    fn a_heavy_value_answers_with_its_own_count() {
        let s = stats(1000, 50, Vec::new());
        assert_eq!(s.equality_rows(Some(400), 1000), 400);
    }

    #[test]
    fn other_values_spread_over_the_distinct_count() {
        let s = stats(1000, 50, Vec::new());
        assert_eq!(s.equality_rows(None, 1000), 20);
    }

    #[test]
    fn the_estimate_is_bounded_by_the_label_and_never_zero_on_a_populated_label() {
        let skew = stats(1000, 2, Vec::new());
        assert_eq!(skew.equality_rows(Some(900), 500), 500);
        let sparse = stats(3, 1_000_000, Vec::new());
        assert_eq!(sparse.equality_rows(None, 3), 1);
        assert_eq!(sparse.equality_rows(None, 0), 0);
    }

    #[test]
    fn the_mean_needs_numeric_values() {
        let mut s = stats(4, 4, Vec::new());
        assert_eq!(s.mean(), None);
        s.numeric_count = 4;
        s.int_sum = 10;
        s.float_sum = 2.0;
        assert_eq!(s.mean(), Some(3.0));
    }

    #[test]
    fn a_non_finite_sum_has_no_mean() {
        let mut s = stats(2, 2, Vec::new());
        s.numeric_count = 2;
        s.float_sum = f64::INFINITY;
        assert_eq!(s.mean(), None, "an infinity");
        s.float_sum = f64::NAN;
        assert_eq!(s.mean(), None, "+inf and -inf cancel to NaN");
    }

    use std::cell::Cell;
    use std::sync::Arc;

    use relate_ir::Expr;
    use relate_names::{NamedToken, SmallName, Token};

    use crate::stats::memo::StatsMemo;
    use crate::stats::{ContainsCount, NoPatternStats, PatternStats};

    fn named(name: &str) -> NamedToken {
        (SmallName::from_name(name), Token::Unresolved)
    }

    /// Answers both new consults and counts how often each reached it.
    #[derive(Default)]
    struct Fake {
        key_calls: Cell<u32>,
        estimate_calls: Cell<u32>,
        built: bool,
    }

    impl PatternStats for Fake {
        fn label_rows(&self, _: &NamedToken) -> Option<u64> {
            None
        }
        fn equality_rows(&self, _: &NamedToken, _: &NamedToken, _: &Expr) -> Option<u64> {
            None
        }
        fn contains_rows(&self, _: &NamedToken, _: &NamedToken, _: &Expr) -> Option<ContainsCount> {
            None
        }
        fn rel_type_rows(&self, _: &NamedToken) -> Option<u64> {
            None
        }
        fn key_stats(&self, _: &NamedToken, _: &NamedToken) -> Option<Arc<KeyStats>> {
            self.key_calls.set(self.key_calls.get() + 1);
            self.built.then(|| Arc::new(stats(10, 5, Vec::new())))
        }
        fn equality_estimate(
            &self,
            _: &NamedToken,
            _: &NamedToken,
            _: &Expr,
        ) -> Option<RowEstimate> {
            self.estimate_calls.set(self.estimate_calls.get() + 1);
            self.built.then_some(RowEstimate {
                rows: 2,
                provenance: Provenance::Sketched,
            })
        }
    }

    #[test]
    fn the_defaults_are_unmeasured() {
        let (l, p) = (named("City"), named("country"));
        assert_eq!(NoPatternStats.key_stats(&l, &p), None);
        assert_eq!(
            NoPatternStats.equality_estimate(&l, &p, &Expr::Int(1)),
            None
        );
    }

    #[test]
    fn the_memo_answers_repeats_of_a_built_key_from_its_table() {
        let fake = Fake {
            built: true,
            ..Fake::default()
        };
        let memo = StatsMemo::new(&fake);
        let (l, p) = (named("City"), named("country"));
        for _ in 0..3 {
            assert_eq!(memo.key_stats(&l, &p).map(|s| s.distinct), Some(5));
            assert_eq!(
                memo.equality_estimate(&l, &p, &Expr::Int(1))
                    .map(|e| e.rows),
                Some(2)
            );
        }
        assert_eq!(fake.key_calls.get(), 1);
        assert_eq!(fake.estimate_calls.get(), 1);
        // A different operand is a different question.
        memo.equality_estimate(&l, &p, &Expr::Int(2));
        assert_eq!(fake.estimate_calls.get(), 2);
    }

    #[test]
    fn the_memo_never_stores_an_unmeasured_answer() {
        let fake = Fake::default();
        let memo = StatsMemo::new(&fake);
        let (l, p) = (named("City"), named("country"));
        assert_eq!(memo.key_stats(&l, &p), None);
        assert_eq!(memo.key_stats(&l, &p), None);
        assert_eq!(memo.equality_estimate(&l, &p, &Expr::Int(1)), None);
        assert_eq!(memo.equality_estimate(&l, &p, &Expr::Int(1)), None);
        assert_eq!(fake.key_calls.get(), 2);
        assert_eq!(fake.estimate_calls.get(), 2);
    }
}
