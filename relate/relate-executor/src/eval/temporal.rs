//! The chronological comparison operator (Cypher 25, ADR-0467).
//!
//! Temporal construction and full-seconds rendering live elsewhere: the
//! builtin rows in `eval::function::temporal` and
//! `Temporal::full_seconds_string` (relate-core, ADR-0448).

use std::cmp::Ordering;

use relate_core::{temporal, Value};
use relate_ir::ChronoOp;

/// The value's temporal family — core's one variant grouping (the
/// `TemporalKind` partition), `None` for every non-temporal value: the
/// two `Date` spellings are one family (chrono range vs civil
/// out-of-range years), as are the two `LocalDateTime` spellings; zoned
/// `Time`, zoned `DateTime`, and the rest are families of one. Unlike
/// families never compare (the operator's mixed-kind policy), and
/// `Duration` is its own family so two durations reduce to the ordinary
/// relations (`=` defined, ordering null).
///
/// The executor keeps no second copy of the partition: a new temporal
/// variant changes core's `kind` match and this alias follows.
fn chrono_kind(value: &Value) -> Option<temporal::TemporalKind> {
    value.as_temporal().map(temporal::Temporal::kind)
}

/// Cypher 25 chronological comparison:
/// `a IS [NOT] BEFORE | AFTER | SAME INSTANT | SAME REPRESENTATION b`.
///
/// Null operands are null in every form. Same-family operands reduce to
/// the core relations -- `SameInstant` to
/// [`temporal::Temporal::same_instant`], `SameRepresentation` to `=`
/// ([`temporal::Temporal::equals`]), the ordering forms to
/// [`temporal::Temporal::instant_ordering`] (an unordered pair, e.g. two
/// durations, stays null like `<` does). Unlike families, or a
/// non-temporal operand, null the ordering forms and falsify the SAME
/// forms -- so `1 IS NOT SAME REPRESENTATION 1` is true while
/// `1 IS NOT BEFORE 2` is null. `negated` inverts a boolean and leaves
/// null null.
pub(crate) fn chrono_compare(op: ChronoOp, l: &Value, r: &Value, negated: bool) -> Value {
    if l.is_null() || r.is_null() {
        return Value::Null;
    }
    let same_family = matches!((chrono_kind(l), chrono_kind(r)), (Some(a), Some(b)) if a == b);
    let result = if same_family {
        let (a, b) = (
            l.as_temporal()
                .expect("chrono_kind verified a temporal value"),
            r.as_temporal()
                .expect("chrono_kind verified a temporal value"),
        );
        match op {
            ChronoOp::SameInstant => Some(a.same_instant(b)),
            ChronoOp::SameRepresentation => Some(a.equals(b)),
            ChronoOp::Before => a.instant_ordering(b).map(|o| o == Ordering::Less),
            ChronoOp::After => a.instant_ordering(b).map(|o| o == Ordering::Greater),
        }
    } else {
        match op {
            ChronoOp::Before | ChronoOp::After => None,
            ChronoOp::SameInstant | ChronoOp::SameRepresentation => Some(false),
        }
    };
    match result {
        Some(b) => Value::Bool(b ^ negated),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> Value {
        Value::Temporal(temporal::parse_iso("datetime", s).expect("parseable datetime"))
    }

    fn d(s: &str) -> Value {
        Value::Temporal(temporal::parse_iso("date", s).expect("parseable date"))
    }

    #[test]
    fn chronological_matrix_rows() {
        // The TemporalChronologicalCompare operator rows: one instant under
        // two offsets, and the DST overlap (same representation, ordered
        // instants).
        let a = dt("2020-01-01T12:00+01:00");
        let b = dt("2020-01-01T11:00Z");
        assert_eq!(
            chrono_compare(ChronoOp::SameInstant, &a, &b, false),
            Value::Bool(true)
        );
        assert_eq!(
            chrono_compare(ChronoOp::SameRepresentation, &a, &b, false),
            Value::Bool(false)
        );
        assert_eq!(
            chrono_compare(ChronoOp::Before, &a, &b, false),
            Value::Bool(false)
        );
        assert_eq!(
            chrono_compare(ChronoOp::After, &a, &b, false),
            Value::Bool(false)
        );
        let early = dt("2020-10-25T02:30+02:00[Europe/Stockholm]");
        let late = dt("2020-10-25T02:30+01:00[Europe/Stockholm]");
        assert_eq!(
            chrono_compare(ChronoOp::SameInstant, &early, &late, false),
            Value::Bool(false)
        );
        assert_eq!(
            chrono_compare(ChronoOp::SameRepresentation, &early, &late, false),
            Value::Bool(true)
        );
        assert_eq!(
            chrono_compare(ChronoOp::Before, &early, &late, false),
            Value::Bool(true)
        );
    }

    #[test]
    fn chronological_null_and_not_forms() {
        let a = d("2020-01-01");
        let null = Value::Null;
        for (op, negated) in [
            (ChronoOp::Before, false),
            (ChronoOp::After, true),
            (ChronoOp::SameInstant, false),
            (ChronoOp::SameRepresentation, true),
        ] {
            assert_eq!(chrono_compare(op, &null, &a, negated), Value::Null);
            assert_eq!(chrono_compare(op, &a, &null, negated), Value::Null);
        }
        let b = d("2020-01-02");
        assert_eq!(
            chrono_compare(ChronoOp::Before, &a, &b, false),
            Value::Bool(true)
        );
        assert_eq!(
            chrono_compare(ChronoOp::Before, &a, &b, true),
            Value::Bool(false)
        );
    }

    #[test]
    fn chronological_non_temporal_and_mixed_kinds() {
        // Non-temporal operands: ordering forms null (NOT keeps null),
        // SAME forms false (NOT makes true).
        let one = Value::Int(1);
        let two = Value::Int(2);
        assert_eq!(
            chrono_compare(ChronoOp::Before, &one, &two, false),
            Value::Null
        );
        assert_eq!(
            chrono_compare(ChronoOp::Before, &one, &two, true),
            Value::Null
        );
        assert_eq!(
            chrono_compare(ChronoOp::SameInstant, &one, &two, false),
            Value::Bool(false)
        );
        assert_eq!(
            chrono_compare(ChronoOp::SameRepresentation, &one, &one, true),
            Value::Bool(true)
        );
        // Mixed temporal kinds behave the same as non-temporal.
        let date = d("2020-01-01");
        let zoned = dt("2020-01-01T00:00Z");
        assert_eq!(
            chrono_compare(ChronoOp::Before, &date, &zoned, false),
            Value::Null
        );
        assert_eq!(
            chrono_compare(ChronoOp::SameInstant, &date, &zoned, false),
            Value::Bool(false)
        );
        // A non-temporal against a temporal too.
        let text = Value::String("not a temporal".into());
        assert_eq!(
            chrono_compare(ChronoOp::After, &text, &zoned, false),
            Value::Null
        );
        assert_eq!(
            chrono_compare(ChronoOp::SameInstant, &zoned, &text, false),
            Value::Bool(false)
        );
    }
}
