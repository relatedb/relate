//! ADR-0175 (LABS-178): the integer-argument sites — `range()`,
//! `substring()` / `left()` / `right()`, `SKIP` / `LIMIT`, list index and
//! slice, and arithmetic at the i64 limits — follow neo4j:2026.09. The
//! table below pins every other-sites row of ADR-0175's probe table (its
//! "Relate after" column, in table order) through the product parser,
//! planner, and executor:
//!
//! - `range()` / `substring()` / `left()` / `right()` / slice bounds coerce
//!   runtime floats with Java `(long)`-cast semantics (truncate toward
//!   zero, NaN → 0, ±Infinity and out-of-range saturate); `substring()` /
//!   `left()` / `right()` then require [0, 2147483647] and a FLOAT slice
//!   bound the int32 range, both raising NumberOutOfRange. An INTEGER slice
//!   bound is a long instead: it clamps across the whole i64 range (the
//!   harness's ListSlicing pins Integer.MAX_VALUE+1 through Long.MAX/MIN).
//!   A `range()` zero step raises NumberOutOfRange and a size overflow
//!   raises `integer out of range`.
//! - `SKIP` / `LIMIT` and the list index reject every runtime float
//!   (including 1.0, NaN, ±Infinity) with InvalidArgumentType.
//! - Float *literals* at those sites fail at plan time with the same
//!   InvalidArgumentType kind (the `†` rows — the planner raises them, so
//!   they surface as `type error:` before execution; the planner unit
//!   tests in relate-planner pin the plan-time phase).
//! - `(-9223372036854775808) / -1` wraps to -9223372036854775808 and
//!   `% -1` is 0 (Neo4j's plain Java operators); `+` / `-` / `*` / unary
//!   `-` keep raising `integer out of range`.
//!
//! The six `‡` deviation rows (`SKIP` / `LIMIT` of `0.0/0.0`, `1.0/0.0`
//! and `head([1.0/0.0, 'x'])`) fail at compile time in Neo4j (22NB1 /
//! 22003) and at runtime in Relate — same outcome, different phase
//! (ADR-0175's recorded deviation); the table pins Relate's runtime
//! error.

use relate_core::{Error, Value};

use super::test_support::try_exec_cypher;
use crate::graph::Graph;
use crate::test_support::ints;

/// The expected outcome of one probe row — ADR-0175's "Relate after"
/// cell.
enum Expect {
    /// A single-row, single-column `v` result equal to this value.
    V(Value),
    /// A single-row float `v` result that is NaN (NaN != NaN, so it
    /// cannot ride `V`).
    Nan,
    /// A statement's rows: one `x` integer per row, in order.
    Xs(&'static [i64]),
    /// The query fails; the error message starts with this prefix (the
    /// after-cell's stderr prefix).
    Err(&'static str),
}

fn s(text: &str) -> Value {
    Value::String(text.into())
}

/// Run one probe row: `query` is the full statement for the SKIP/LIMIT
/// rows and `RETURN <expr> AS v` for the rest — exactly ADR-0175's probe
/// shape.
fn check(g: &Graph, query: &str, expect: &Expect) {
    match (try_exec_cypher(g, query), expect) {
        (Ok(result), Expect::V(want)) => {
            assert_eq!(result.rows.len(), 1, "{query}: expected one row");
            assert_eq!(result.rows[0].len(), 1, "{query}: expected one column");
            assert_eq!(&result.rows[0][0], want, "{query}");
        }
        (Ok(result), Expect::Nan) => {
            assert_eq!(result.rows.len(), 1, "{query}: expected one row");
            assert!(
                matches!(result.rows[0][0], Value::Float(f) if f.is_nan()),
                "{query}: expected a NaN float, got {:?}",
                result.rows[0][0]
            );
        }
        (Ok(result), Expect::Xs(want)) => {
            let got: Vec<&Value> = result.rows.iter().map(|r| &r[0]).collect();
            let want: Vec<Value> = want.iter().map(|i| Value::Int(*i)).collect();
            assert_eq!(got, want.iter().collect::<Vec<_>>(), "{query}");
        }
        (Err(error), Expect::Err(prefix)) => {
            assert!(
                error.to_string().starts_with(prefix),
                "{query}: expected error starting with {prefix:?}, got {error:?}"
            );
        }
        (Ok(result), Expect::Err(prefix)) => {
            panic!(
                "{query}: expected error {prefix:?}, got rows {:?}",
                result.rows
            )
        }
        (Err(error), _) => panic!("{query}: unexpected error {error}"),
    }
}

#[test]
fn adr0175_other_sites_probe_rows() {
    let g = Graph::default();
    // (query, expected) in ADR-0175's probe-table order; `RETURN … AS v`
    // wraps the expression rows exactly like the ADR's probe command.
    let rows: Vec<(&'static str, Expect)> = vec![
        // --- range() ---
        // Float literal: plan-time rejection (ADR-0175's static-gate rule).
        (
            "RETURN range(1.5, 5) AS v",
            Expect::Err("type error: InvalidArgumentType: range() start"),
        ),
        // Runtime floats truncate toward zero.
        (
            "RETURN range(head([1.5, 'x']), 5) AS v",
            Expect::V(ints(&[1, 2, 3, 4, 5])),
        ),
        (
            "RETURN range(head([-2.9, 'x']), 2) AS v",
            Expect::V(ints(&[-2, -1, 0, 1, 2])),
        ),
        (
            "RETURN range(head([1.0, 'x']), 5) AS v",
            Expect::V(ints(&[1, 2, 3, 4, 5])),
        ),
        (
            "RETURN range(1, head([5.9, 'x'])) AS v",
            Expect::V(ints(&[1, 2, 3, 4, 5])),
        ),
        (
            "RETURN range(1, 10, head([2.5, 'x'])) AS v",
            Expect::V(ints(&[1, 3, 5, 7, 9])),
        ),
        (
            "RETURN range(1, 5, head([1.0, 'x'])) AS v",
            Expect::V(ints(&[1, 2, 3, 4, 5])),
        ),
        // NaN converts to 0.
        (
            "RETURN range(head([0.0/0.0, 'x']), 5) AS v",
            Expect::V(ints(&[0, 1, 2, 3, 4, 5])),
        ),
        (
            "RETURN range(1, head([0.0/0.0, 'x'])) AS v",
            Expect::V(ints(&[])),
        ),
        // A zero step — NaN included — is NumberOutOfRange.
        (
            "RETURN range(1, 5, head([0.0/0.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: range() step cannot be 0"),
        ),
        // ±Infinity saturate at the i64 limits.
        (
            "RETURN range(head([1.0/0.0, 'x']), 5) AS v",
            Expect::V(ints(&[])),
        ),
        (
            "RETURN range(1, 5, head([1.0/0.0, 'x'])) AS v",
            Expect::V(ints(&[1])),
        ),
        // A size computation overflowing i64 is the arithmetic-overflow error.
        (
            "RETURN range(1, head([-1.0/0.0, 'x'])) AS v",
            Expect::Err("type error: integer out of range"),
        ),
        (
            "RETURN range(5, 1, head([-1.0/0.0, 'x'])) AS v",
            Expect::V(ints(&[5])),
        ),
        (
            "RETURN range(head([9.3e18, 'x']), head([9.3e18, 'x'])) AS v",
            Expect::V(ints(&[i64::MAX])),
        ),
        // --- substring() ---
        // Float literal: plan-time rejection.
        (
            "RETURN substring('abcdef', 1.5) AS v",
            Expect::Err("type error: InvalidArgumentType: substring() start must be an integer"),
        ),
        (
            "RETURN substring('abcdef', head([1.5, 'x'])) AS v",
            Expect::V(s("bcdef")),
        ),
        (
            "RETURN substring('abcdef', head([1.0, 'x'])) AS v",
            Expect::V(s("bcdef")),
        ),
        (
            "RETURN substring('abcdef', head([0.0/0.0, 'x'])) AS v",
            Expect::V(s("abcdef")),
        ),
        // Beyond [0, 2147483647] — ±Infinity, 2147483648.0, 9.3e18 — or
        // below 0 raises NumberOutOfRange.
        (
            "RETURN substring('abcdef', head([1.0/0.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: substring() start"),
        ),
        (
            "RETURN substring('abcdef', head([-1.0/0.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: substring() start"),
        ),
        (
            "RETURN substring('abcdef', head([-1.5, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: substring() start"),
        ),
        (
            "RETURN substring('abcdef', head([2147483648.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: substring() start"),
        ),
        (
            "RETURN substring('abcdef', head([9.3e18, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: substring() start"),
        ),
        (
            "RETURN substring('abcdef', 1, head([2.5, 'x'])) AS v",
            Expect::V(s("bc")),
        ),
        (
            "RETURN substring('abcdef', head([0.0/0.0, 'x']), head([0.0/0.0, 'x'])) AS v",
            Expect::V(s("")),
        ),
        (
            "RETURN substring('abcdef', 1, head([1.0/0.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: substring() length"),
        ),
        // --- left() / right() ---
        (
            "RETURN left('abcdef', head([2.9, 'x'])) AS v",
            Expect::V(s("ab")),
        ),
        (
            "RETURN left('abcdef', head([0.0/0.0, 'x'])) AS v",
            Expect::V(s("")),
        ),
        (
            "RETURN left('abcdef', head([1.0/0.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: left() length"),
        ),
        (
            "RETURN left('abcdef', head([-1.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: left() length"),
        ),
        (
            "RETURN right('abcdef', head([2.9, 'x'])) AS v",
            Expect::V(s("ef")),
        ),
        (
            "RETURN right('abcdef', head([0.0/0.0, 'x'])) AS v",
            Expect::V(s("")),
        ),
        (
            "RETURN right('abcdef', head([1.0/0.0, 'x'])) AS v",
            Expect::Err("semantic error: NumberOutOfRange: right() length"),
        ),
        // --- SKIP / LIMIT ---
        ("UNWIND [1,2,3] AS x RETURN x SKIP 1", Expect::Xs(&[2, 3])),
        // Float literals keep their existing parse-time error.
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP 1.0",
            Expect::Err("syntax error: InvalidArgumentType: SKIP requires an integer value"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP 1.5",
            Expect::Err("syntax error: InvalidArgumentType: SKIP requires an integer value"),
        ),
        // ‡ deviation rows: Neo4j rejects these constant float expressions
        // at compile time (22NB1); Relate rejects them at runtime with
        // the same InvalidArgumentType kind — same outcome, different phase.
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP 0.0/0.0",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP 1.0/0.0",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT 1.0",
            Expect::Err("syntax error: InvalidArgumentType: LIMIT requires an integer value"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT 1.5",
            Expect::Err("syntax error: InvalidArgumentType: LIMIT requires an integer value"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT 0.0/0.0",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT 1.0/0.0",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        // ANY-typed runtime floats are all rejected — 1.0 included.
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP head([1.5, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP head([1.0, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP head([0.0/0.0, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        // ‡ deviation row: Neo4j's constant folder refuses the Infinity
        // literal at compile time (22003); Relate rejects at runtime.
        (
            "UNWIND [1,2,3] AS x RETURN x SKIP head([1.0/0.0, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT head([1.5, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT head([1.0, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT head([0.0/0.0, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        // ‡ deviation row (see above).
        (
            "UNWIND [1,2,3] AS x RETURN x LIMIT head([1.0/0.0, 'x'])",
            Expect::Err("type error: InvalidArgumentType: SKIP/LIMIT must be an integer"),
        ),
        // --- list index ---
        // Float literal: plan-time rejection; every runtime float index —
        // 1.0 and NaN included — is a type error (Neo4j 22N01).
        (
            "RETURN [1,2,3][1.0] AS v",
            Expect::Err("type error: InvalidArgumentType: list index must be an integer"),
        ),
        (
            "RETURN [1,2,3][head([1.0, 'x'])] AS v",
            Expect::Err("type error: InvalidArgumentType: list index must be an integer"),
        ),
        (
            "RETURN [1,2,3][head([1.5, 'x'])] AS v",
            Expect::Err("type error: InvalidArgumentType: list index must be an integer"),
        ),
        (
            "RETURN [1,2,3][head([0.0/0.0, 'x'])] AS v",
            Expect::Err("type error: InvalidArgumentType: list index must be an integer"),
        ),
        // --- list slice ---
        // Float literal: plan-time rejection.
        (
            "RETURN [1,2,3][1.0..2] AS v",
            Expect::Err("type error: InvalidArgumentType: slice start must be an integer"),
        ),
        // Bounds truncate toward zero, NaN → 0.
        (
            "RETURN [1,2,3][head([1.0, 'x'])..2] AS v",
            Expect::V(ints(&[2])),
        ),
        (
            "RETURN [1,2,3][head([1.5, 'x'])..head([2.9, 'x'])] AS v",
            Expect::V(ints(&[2])),
        ),
        (
            "RETURN [1,2,3][head([0.0/0.0, 'x'])..2] AS v",
            Expect::V(ints(&[1, 2])),
        ),
        (
            "RETURN [1,2,3][1..head([0.0/0.0, 'x'])] AS v",
            Expect::V(ints(&[])),
        ),
        (
            "RETURN [1,2,3][..head([1.5, 'x'])] AS v",
            Expect::V(ints(&[1])),
        ),
        (
            "RETURN [1,2,3][head([1.5, 'x'])..] AS v",
            Expect::V(ints(&[2, 3])),
        ),
        // A FLOAT bound is int32-exact: beyond it raises NumberOutOfRange.
        (
            "RETURN [1,2,3][head([1.0/0.0, 'x'])..2] AS v",
            Expect::Err("semantic error: NumberOutOfRange: slice start"),
        ),
        // Negative bounds keep the negative-index semantics.
        (
            "RETURN [1,2,3][head([-1.5, 'x'])..2] AS v",
            Expect::V(ints(&[])),
        ),
        (
            "RETURN [1,2,3][head([2147483648.0, 'x'])..] AS v",
            Expect::Err("semantic error: NumberOutOfRange: slice start"),
        ),
        // An INTEGER bound is a long: it clamps across the whole i64 range
        // (the harness pins 2147483648, Long.MAX and Long.MIN).
        (
            "RETURN [1,2,3][..2147483648] AS v",
            Expect::V(ints(&[1, 2, 3])),
        ),
        ("RETURN [1,2,3][2147483648..] AS v", Expect::V(ints(&[]))),
        (
            "RETURN [1,2,3][..9223372036854775807] AS v",
            Expect::V(ints(&[1, 2, 3])),
        ),
        (
            "RETURN [1,2,3][9223372036854775807..] AS v",
            Expect::V(ints(&[])),
        ),
        (
            "RETURN [1,2,3][..(-9223372036854775807 - 1)] AS v",
            Expect::V(ints(&[])),
        ),
        (
            "RETURN [1,2,3][(-9223372036854775807 - 1)..] AS v",
            Expect::V(ints(&[1, 2, 3])),
        ),
        // --- arithmetic at the i64 limits ---
        (
            "RETURN 9223372036854775807 + 1 AS v",
            Expect::Err("type error: integer out of range"),
        ),
        (
            "RETURN (-9223372036854775807 - 1) - 1 AS v",
            Expect::Err("type error: integer out of range"),
        ),
        (
            "RETURN 9223372036854775807 * 2 AS v",
            Expect::Err("type error: integer out of range"),
        ),
        (
            "RETURN -(-9223372036854775807 - 1) AS v",
            Expect::Err("type error: integer out of range"),
        ),
        // MIN / -1 wraps to MIN and MIN % -1 is 0 (Neo4j's plain Java
        // operators carry no overflow check on those two).
        (
            "RETURN (-9223372036854775807 - 1) / -1 AS v",
            Expect::V(Value::Int(i64::MIN)),
        ),
        (
            "RETURN (-9223372036854775807 - 1) % -1 AS v",
            Expect::V(Value::Int(0)),
        ),
        ("RETURN 1 / 0 AS v", Expect::Err("type error: / by zero")),
        ("RETURN 1 % 0 AS v", Expect::Err("type error: % by zero")),
        // Float division/modulo keep IEEE semantics.
        (
            "RETURN 1.0 / 0 AS v",
            Expect::V(Value::Float(f64::INFINITY)),
        ),
        (
            "RETURN 1.0 / 0.0 AS v",
            Expect::V(Value::Float(f64::INFINITY)),
        ),
        ("RETURN 1.0 % 0.0 AS v", Expect::Nan),
        (
            "RETURN 9223372036854775807 + 0.5 AS v",
            Expect::V(Value::Float(9.223372036854776e18)),
        ),
    ];
    // The table must cover every other-sites row of ADR-0175's probe table.
    assert_eq!(rows.len(), 83, "one row per ADR-0175 other-sites probe row");
    for (query, expect) in &rows {
        check(&g, query, expect);
    }
}

/// ADR-0463: `range()` knows its exact element count before it allocates,
/// so an enormous result is refused up front with the expression-budget
/// capacity error (the display names the attempt and the budget). A
/// direction-inconsistent range is empty, so it allocates nothing and is
/// not refused.
/// ADR-0463 regression (round 2): an extreme range length must not wrap the
/// byte formula to a small charge. `reserved_len(len) * size_of::<Value>()`
/// overflows u64 above ~2^58 slots; saturating arithmetic turns that into
/// the typed capacity refusal instead of a wrapped pass followed by a
/// `Vec::with_capacity` abort (release) or arithmetic panic (debug).
#[test]
fn adr0463_extreme_range_length_is_a_typed_refusal_not_an_overflow() {
    let g = Graph::default();
    for n in [1_000_000_000_000_000_000_u64, 1_152_921_504_606_846_976_u64] {
        let query = format!("RETURN range(1, {n}) AS v");
        let error = try_exec_cypher(&g, &query)
            .err()
            .unwrap_or_else(|| panic!("range(1, {n}) must be refused"));
        assert!(
            matches!(
                error,
                Error::Capacity {
                    used,
                    limit,
                    kind: relate_core::CapacityKind::Expression,
                } if used > limit && limit == crate::memory::EXPRESSION_BUDGET_BYTES
            ),
            "range(1, {n}): {error}"
        );
    }
}

#[test]
fn adr0463_range_is_refused_before_an_over_budget_allocation() {
    let g = Graph::default();
    // Assert the typed refusal, not its rendered message: the budget is a
    // constant, and the message rendering has its own pin.
    let error = try_exec_cypher(&g, "RETURN range(1, 1000000000) AS v")
        .err()
        .expect("an over-budget range must be refused");
    let attempted =
        crate::workspace::buffer_bytes(1_000_000_000, std::mem::size_of::<Value>() as u64);
    assert!(
        matches!(
            error,
            Error::Capacity {
                used,
                limit,
                kind: relate_core::CapacityKind::Expression,
            } if used == attempted && limit == crate::memory::EXPRESSION_BUDGET_BYTES
        ),
        "{error}"
    );
    // The refusal is the plain-language, three-part user message: what,
    // that it is a protection, and how to raise it; GB/MB units, no jargon.
    assert!(
        error
            .to_string()
            .contains("tried to build a single value of")
            && error.to_string().contains("not a database limit")
            && error.to_string().contains("--expression-memory-limit"),
        "{error}"
    );
    check(
        &g,
        "RETURN range(1000000000, 1) AS v",
        &Expect::V(ints(&[])),
    );
}
