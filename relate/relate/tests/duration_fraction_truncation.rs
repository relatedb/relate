//! Duration component fractions truncate at the nanosecond tier, matching
//! Neo4j's `(long)` cast at every tier. Rounding answered one nanosecond
//! high (`duration('P0.9Y')` was `P10M24DT8H23M16.800000002S`) and turned a
//! sub-nanosecond map field into a whole nanosecond
//! (`duration({microseconds: 0.0005})` was `PT0.000000001S`). Every expected
//! string is live-pinned against Neo4j 2026.10.1.

mod common;

use common::{connection, text};

#[test]
fn fractional_year_and_month_components_truncate() {
    let mut conn = connection();
    for (query, expected) in [
        (
            "RETURN toString(duration('P0.9Y')) AS d",
            "P10M24DT8H23M16.800000001S",
        ),
        (
            "RETURN toString(duration('P0.3Y')) AS d",
            "P3M18DT6H17M27.599999998S",
        ),
        (
            "RETURN toString(duration({years: 0.9})) AS d",
            "P10M24DT8H23M16.800000001S",
        ),
        (
            "RETURN toString(duration({months: 0.9})) AS d",
            "P27DT9H26M11.399999999S",
        ),
        (
            "RETURN toString(duration('P-0.9Y')) AS d",
            "P-10M-24DT-8H-23M-16.800000001S",
        ),
    ] {
        assert_eq!(text(&mut conn, query), expected, "{query}");
    }
}

#[test]
fn sub_nanosecond_fields_contribute_nothing() {
    let mut conn = connection();
    for (query, expected) in [
        (
            "RETURN toString(duration({nanoseconds: 1.9})) AS d",
            "PT0.000000001S",
        ),
        (
            "RETURN toString(duration({microseconds: 0.0005})) AS d",
            "PT0S",
        ),
        (
            "RETURN toString(duration({seconds: 1.0000000005})) AS d",
            "PT1S",
        ),
    ] {
        assert_eq!(text(&mut conn, query), expected, "{query}");
    }
}

/// Scaling shares the one fold: `duration('P1Y') * 0.3` answered
/// `…27.599999999S` while the fold rounded, and `duration('P1M') * 0.9` keeps
/// its truncated month fraction.
#[test]
fn duration_scaling_keeps_the_truncated_fold() {
    let mut conn = connection();
    for (query, expected) in [
        (
            "RETURN toString(duration('P1Y') * 0.3) AS d",
            "P3M18DT6H17M27.599999998S",
        ),
        (
            "RETURN toString(duration('P1M') * 0.9) AS d",
            "P27DT9H26M11.399999999S",
        ),
    ] {
        assert_eq!(text(&mut conn, query), expected, "{query}");
    }
}
