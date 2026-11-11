//! Checked integer arithmetic in the evaluator
//! (memdb-merge/30-checked-int-ops).
//!
//! The evaluator's generic integer ops (Div, Mod, num_op's Sub/Mul) and
//! range()'s step accumulation used raw i64 arithmetic that unwound on
//! overflow in debug builds and wrapped (or looped forever) in release.
//! `abs(i64::MIN)` was the same class. An embedded engine must never
//! unwind into the host: overflow now surfaces as an honest
//! "integer out of range" query error — Neo4j errors on integer overflow
//! (never wraps), and node 03 established that phrasing for unary minus.
//!
//! ADR-0569 (superseding ADR-0175's carve-out) makes division checked too:
//! `i64::MIN / -1` raises "integer out of range" like every other
//! overflowing operator (live Neo4j 2026.10.1 raises 22003), while
//! `i64::MIN % -1` keeps its exact 0 result (in
//! `in_range_integer_arithmetic_still_works` below). range()'s size
//! computation still errors on i64 overflow, but a range whose size is
//! representable no longer errors just because the *last* step would leave
//! i64 (Neo4j returns it: e.g. `range(i64::MAX, i64::MAX)` is
//! `[i64::MAX]`).
//!
//! Every OVERFLOW_QUERIES entry panicked in a debug build before the fix.
//!
//! memdb-merge/31-align-add-overflow: add()'s (Int, Int) arm additionally
//! *saturated* on overflow (`i64::MIN + -1` → `i64::MIN`); it now shares the
//! same checked contract and honest error.

use std::panic::{catch_unwind, AssertUnwindSafe};

use relate::{binding_error_metadata, Database, Value};

const OVERFLOW_QUERIES: &[&str] = &[
    // Contract repro: Mul on pure integers.
    r#"RETURN 4611686018427387904 * 4"#,
    // Sub / Mul variants at the i64 edges.
    r#"RETURN -9223372036854775807 - 2"#,
    r#"RETURN (-9223372036854775807 - 1) - 1"#,
    r#"RETURN (-9223372036854775807 - 1) * -1"#,
    r#"RETURN 9223372036854775807 * 2"#,
    // Add at both i64 edges (memdb-merge/31-align-add-overflow): add()'s
    // (Int, Int) arm saturated silently; it must error like Sub/Mul/Div/Mod.
    // i64::MIN / MAX are not literal-representable, so derive MIN via Sub.
    r#"RETURN (-9223372036854775807 - 1) + -1"#,
    r#"RETURN (-9223372036854775807 - 1) + (-9223372036854775807 - 1)"#,
    r#"RETURN 9223372036854775807 + 1"#,
    r#"RETURN 9223372036854775807 + 9223372036854775807"#,
    // range() size overflow (ADR-0175): `end - start` or `diff/step + 1`
    // leaves i64 — Neo4j raises 22N28 from its subtractExact/addExact.
    r#"RETURN range(-9223372036854775807 - 1, 9223372036854775807)"#,
    r#"RETURN range(0, 9223372036854775807)"#,
    // Division is checked like the other operators since ADR-0569 (live
    // Neo4j 2026.10.1 raises 22003 for the one overflowing integer
    // division); ADR-0175's plain-Java wrap is superseded.
    r#"RETURN (-9223372036854775807 - 1) / -1"#,
];

/// The historical Sub/abs repro derived `i64::MIN` months through
/// `duration.between` over unbounded civil years. Duration assembly now
/// applies the one representability bound (`Temporal::duration_checked`), so
/// the between itself is the honest range error instead of handing a
/// non-representable `Duration` to the next integer op.
#[test]
fn temporal_derived_overflow_stops_at_the_duration_seam() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    for query in [
        r#"RETURN duration.between(date('+768614336404564650-09-01'), date('+0000-01-01'))"#,
        r#"RETURN (duration.between(date('+768614336404564650-09-01'), date('+0000-01-01'))).months - 1"#,
        r#"RETURN abs(duration.between(date('+768614336404564650-09-01'), date('+0000-01-01')).months)"#,
    ] {
        let err = match conn.query(query) {
            Ok(_) => panic!("query must fail with an error: {query}"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("temporal value out of range"),
            "query must fail with the duration range error: {query} (got: {err})"
        );
    }
}

#[test]
fn overflowing_integer_ops_error() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    for query in OVERFLOW_QUERIES {
        let err = match conn.query(query) {
            Ok(_) => panic!("query must fail with an error: {query}"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("integer out of range"),
            "query must fail with an honest range error: {query} (got: {err})"
        );
        // Overflow is Neo4j's 22N28 (`Neo.ClientError.Statement.
        // ArithmeticError`), a `TypeError`-class failure — not the
        // `NumberOutOfRange` detail `toInteger()` rejects with.
        assert_eq!(
            binding_error_metadata(&err).1,
            "TypeError",
            "integer overflow must carry the TypeError class: {query} (got: {err})"
        );
    }
}

/// No unwind may escape the public query path for those inputs. A fresh
/// database per query keeps a hypothetically poisoned engine from masking a
/// later panic.
#[test]
fn overflowing_integer_ops_do_not_unwind_through_public_api() {
    for query in OVERFLOW_QUERIES {
        let mut conn = Database::open(":memory:").unwrap().connect().unwrap();
        let outcome = catch_unwind(AssertUnwindSafe(|| conn.query(query)));
        let result = outcome.unwrap_or_else(|_| panic!("query panicked: {query}"));
        assert!(result.is_err(), "query must fail with an error: {query}");
    }
}

/// In-range controls: boundary-adjacent values whose results are
/// representable keep working, with exact results.
#[test]
fn in_range_integer_arithmetic_still_works() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let controls: &[(&str, i64)] = &[
        ("RETURN 7 / 2", 3),
        ("RETURN -7 / 2", -3),
        ("RETURN (-9223372036854775807 - 1) / 1", i64::MIN),
        // ADR-0569: MIN / -1 is a checked overflow (see OVERFLOW_QUERIES);
        // MIN % -1 keeps its exact 0 result.
        ("RETURN (-9223372036854775807 - 1) % -1", 0),
        ("RETURN 7 % 3", 1),
        ("RETURN (-9223372036854775807 - 1) % 1", 0),
        ("RETURN 1000000007 * 1000000009", 1000000016000000063),
        ("RETURN 4611686018427387904 * 1", 4611686018427387904),
        ("RETURN -9223372036854775807 - 0", -9223372036854775807),
        (
            "RETURN (-9223372036854775807 - 1) + 1",
            -9223372036854775807,
        ),
        ("RETURN 9223372036854775807 + 0", 9223372036854775807),
        ("RETURN (-9223372036854775807 - 1) + 0", i64::MIN),
        ("RETURN 9223372036854775806 + 1", 9223372036854775807),
        ("RETURN 9223372036854775807 + -9223372036854775807", 0),
        ("RETURN 9223372036854775807 - 9223372036854775807", 0),
        ("RETURN abs(-9223372036854775807)", 9223372036854775807),
        ("RETURN abs(0 - 9223372036854775807)", 9223372036854775807),
    ];
    for &(query, expected) in controls {
        let result = conn
            .query(query)
            .unwrap_or_else(|e| panic!("{query} must succeed: {e}"));
        assert_eq!(result.rows[0][0].as_int(), Some(expected), "for {query}");
    }
}

/// range() controls near the boundary whose step accumulation stays in i64.
#[test]
fn in_range_range_controls_still_work() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let controls: &[(&str, &[i64])] = &[
        ("RETURN range(5, -5, -3)", &[5, 2, -1, -4]),
        ("RETURN range(0, 10, 9223372036854775807)", &[0]),
        (
            "RETURN range(-9223372036854775807, -9223372036854775807)",
            &[-9223372036854775807],
        ),
        (
            "RETURN range(9223372036854775806, 9223372036854775806)",
            &[9223372036854775806],
        ),
        // ADR-0175: a range whose size is representable no longer errors
        // just because the next step would leave i64 (Neo4j returns it).
        (
            "RETURN range(9223372036854775807, 9223372036854775807)",
            &[9223372036854775807],
        ),
        (
            "RETURN range(9223372036854775800, 9223372036854775807, 9223372036854775807)",
            &[9223372036854775800],
        ),
        (
            "RETURN range(-9223372036854775807 - 1, -9223372036854775807 - 1, -9223372036854775807)",
            &[i64::MIN],
        ),
        ("RETURN range(3, 1)", &[]),
    ];
    for &(query, expected) in controls {
        let result = conn
            .query(query)
            .unwrap_or_else(|e| panic!("{query} must succeed: {e}"));
        let Value::List(items) = &result.rows[0][0] else {
            panic!("expected a list for {query}, got {:?}", result.rows[0][0]);
        };
        let ints: Vec<i64> = items.iter().map(|v| v.as_int().unwrap()).collect();
        assert_eq!(ints, expected, "for {query}");
    }
}

/// Divide/modulo by zero keep their distinct pre-existing errors.
#[test]
fn division_and_modulo_by_zero_still_error() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    for query in ["RETURN 1 / 0", "RETURN 1 % 0"] {
        assert!(conn.query(query).is_err(), "{query} must fail");
    }
}
