//! The plan grade (ADR-0687 decision 6): the execution work a cache entry's
//! plan has measured, as the store reads one execution performed — the
//! counter-based metric behind the PROFILE `Db Hits` total, never elapsed
//! time, because measured work catches a shape that still looks right but
//! degrades. A statement entry drives its plan from exactly one root row, so
//! the reading needs no per-input-row normalisation. Every graded execution
//! moves an exponentially weighted average one step toward its reading, and
//! the entry's first graded execution is the baseline the average is compared
//! against: a plan whose average reaches [`GRADE_FACTOR`] times that baseline
//! has degraded. The state is those two numbers — one entry measured any
//! number of times carries no history — so the per-entry bound is constant.
//!
//! The gate the grade feeds is a *dirty* plan's only: a clean entry serves
//! because its footprint still describes the data (ADR-0687 decision 5),
//! and a dirty one serves until its measured work says the shape no longer
//! pays.

/// The degradation factor (ADR-0687 decision 6): a dirty plan's average at
/// or past this multiple of its baseline re-plans the entry on that hit.
pub(crate) const GRADE_FACTOR: f64 = 2.0;

/// The EWMA's smoothing (ADR-0687 decision 6): each graded execution moves
/// the average one eighth of the way toward its reading — the ADR's
/// constant, one spelling for the update and its readers.
const SMOOTHING: f64 = 1.0 / 8.0;

/// A graded entry's two numbers: the baseline its first graded execution
/// recorded and the EWMA over every graded execution since. The baseline is
/// never zero — a zero reading establishes none (see [`PlanGrade::record`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Graded {
    baseline: f64,
    ewma: f64,
}

/// One entry's measured plan grade: `None` until the entry's first graded
/// execution establishes a baseline, `Some(Graded)` from then on — one
/// representation of "ungraded", never a sentinel beside a second field.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PlanGrade(Option<Graded>);

impl PlanGrade {
    /// Fold one graded execution's reading in: the first reading is the
    /// baseline and the average alike, and every later one moves the average
    /// [`SMOOTHING`] of the way toward itself. The baseline never moves, so
    /// a slow decline accumulates across executions instead of resetting.
    ///
    /// A zero first reading establishes no baseline: a plan that read nothing
    /// has nothing to degrade from, and the footprint drift path already
    /// re-plans it. Later readings still fold once a baseline exists.
    pub(crate) fn record(&mut self, reading: f64) {
        let Some(graded) = self.0.as_mut() else {
            if reading > 0.0 {
                self.0 = Some(Graded {
                    baseline: reading,
                    ewma: reading,
                });
            }
            return;
        };
        graded.ewma += (reading - graded.ewma) * SMOOTHING;
    }

    /// Whether the plan has measurably degraded (ADR-0687 decision 6): its
    /// average is at or past [`GRADE_FACTOR`] times the baseline. An ungraded
    /// entry never degrades, and a graded one's baseline is positive by
    /// construction, so the comparison needs no zero guard.
    pub(crate) fn degraded(&self) -> bool {
        self.0
            .is_some_and(|graded| graded.ewma >= graded.baseline * GRADE_FACTOR)
    }

    /// The baseline the entry's first graded execution recorded; `None` while
    /// ungraded (tests).
    #[cfg(test)]
    pub(crate) fn baseline(&self) -> Option<f64> {
        self.0.map(|graded| graded.baseline)
    }

    /// The EWMA over the entry's graded executions; `None` while ungraded
    /// (tests).
    #[cfg(test)]
    pub(crate) fn ewma(&self) -> Option<f64> {
        self.0.map(|graded| graded.ewma)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ADR's update, exactly: the first reading is the baseline and the
    /// average, and each later reading moves the average one eighth of the
    /// distance to itself — in both directions, with the baseline standing.
    #[test]
    fn the_average_moves_one_eighth_toward_each_reading() {
        let mut grade = PlanGrade::default();
        assert_eq!(grade.baseline(), None, "nothing graded yet");
        assert_eq!(grade.ewma(), None);
        grade.record(8.0);
        assert_eq!(
            grade.baseline(),
            Some(8.0),
            "the first reading is the baseline"
        );
        assert_eq!(grade.ewma(), Some(8.0));
        grade.record(16.0);
        assert_eq!(grade.ewma(), Some(9.0), "8 + (16 - 8) / 8");
        grade.record(0.0);
        assert_eq!(
            grade.ewma(),
            Some(9.0 - 9.0 / 8.0),
            "the same step downward"
        );
        assert_eq!(grade.baseline(), Some(8.0), "the baseline never moves");
        // A constant reading converges on itself and never past it.
        let mut constant = PlanGrade::default();
        for _ in 0..200 {
            constant.record(4.0);
        }
        assert!((constant.ewma().expect("graded") - 4.0).abs() < 1e-9);
    }

    /// The bound the per-entry state carries: the average is a convex
    /// combination of the readings recorded, so it never leaves their range
    /// however many executions are graded, and the state is the baseline
    /// plus the average — no per-execution history accumulates.
    #[test]
    fn the_average_stays_inside_the_recorded_range() {
        let mut grade = PlanGrade::default();
        let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
        for round in 1..=500u32 {
            // A swinging reading: far above and far below the baseline.
            let reading = if round % 3 == 0 { 250.0 } else { 0.5 };
            grade.record(reading);
            low = low.min(reading);
            high = high.max(reading);
            let ewma = grade.ewma().expect("graded");
            assert!(
                ewma >= low && ewma <= high,
                "round {round}: {ewma} outside [{low}, {high}]"
            );
            assert_eq!(grade.baseline(), Some(0.5), "the first reading stands");
        }
        // Two numbers inside one optional: the graded pair, no extra flag.
        assert_eq!(
            std::mem::size_of::<PlanGrade>(),
            std::mem::size_of::<Option<Graded>>()
        );
    }

    /// The gate at exactly the ADR's factor: 2x the baseline degrades and
    /// anything below it does not, and an ungraded entry never degrades.
    #[test]
    fn the_grade_degrades_at_twice_the_baseline_and_not_below() {
        let mut grade = PlanGrade::default();
        assert!(!grade.degraded(), "ungraded: nothing to degrade from");
        grade.record(4.0);
        assert!(!grade.degraded(), "at the baseline");
        // 4 + (36 - 4) / 8 = 8 — exactly twice the baseline.
        grade.record(36.0);
        assert_eq!(grade.ewma(), Some(8.0));
        assert!(grade.degraded(), "at the factor, not only past it");
        // A hair below it, from a fresh baseline.
        let mut below = PlanGrade::default();
        below.record(4.0);
        below.record(35.9);
        assert_eq!(below.ewma(), Some(7.9875));
        assert!(!below.degraded());
    }

    /// A zero first reading declines to establish a baseline — a plan that
    /// read nothing has nothing to degrade from — and the first reading that
    /// measured work becomes the baseline instead. A graded entry takes a
    /// zero reading as any other reading.
    #[test]
    fn a_zero_first_reading_establishes_no_baseline() {
        let mut free = PlanGrade::default();
        free.record(0.0);
        assert_eq!(
            free.baseline(),
            None,
            "a plan that read nothing establishes none"
        );
        assert!(!free.degraded());
        free.record(100.0);
        assert_eq!(
            free.baseline(),
            Some(100.0),
            "the first reading that read anything is the baseline"
        );
        assert_eq!(free.ewma(), Some(100.0));
        // A graded entry takes a zero reading as any other reading.
        let mut graded = PlanGrade::default();
        graded.record(8.0);
        graded.record(0.0);
        assert_eq!(graded.baseline(), Some(8.0));
        assert_eq!(graded.ewma(), Some(7.0), "8 - 8 / 8");
    }
}
