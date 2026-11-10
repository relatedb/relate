//! Plan-time integer-argument rules (ADR-0175): a statically-known
//! wrong-typed literal at an `range()` / `substring()` / `left()` /
//! `right()` site, a list index, or a slice bound. Split from `tests`
//! under the LABS-311 size gate.

use relate_core::{integer_args, Error};

use super::*;

// ADR-0175 (statically-known floats): a float *literal* at an
// integer-argument site — range(), substring()/left()/right(), list
// index, slice bounds — fails at plan time with the same
// InvalidArgumentType kind the site raises at runtime (Neo4j's 22N27 /
// 22NB1 static gate fires at the same phase). The planner raises
// `Error::ty`: for range() and list index the rendered message matches
// the runtime failure; for substring()/left()/right()/slice the wording
// is plan-time-only — at runtime those sites coerce floats (truncate
// toward zero, NaN → 0, saturate) and only raise NumberOutOfRange with
// different wording, per ADR-0175. `parse_and_lower` must succeed (the
// rejection is the planner's, not the front-end's).
#[test]
fn float_literals_at_integer_argument_sites_fail_at_plan_time() {
    // The typed assertion consumes the site's authored text from
    // `relate_core::integer_args` and pins which site the planner chose.
    for (src, site) in [
        ("RETURN range(1.5, 5)", &integer_args::RANGE_START),
        ("RETURN range(1, 5.9)", &integer_args::RANGE_END),
        ("RETURN range(1, 10, 2.5)", &integer_args::RANGE_STEP),
        // The parser splits either sign into a unary operator; a signed
        // float literal is the same mistake.
        ("RETURN range(-1.1, 1, 1)", &integer_args::RANGE_START),
        (
            "RETURN substring('abcdef', 1.5)",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN substring('abcdef', 1, 2.5)",
            &integer_args::SUBSTRING_LENGTH,
        ),
        ("RETURN left('abcdef', 2.9)", &integer_args::LEFT_LENGTH),
        // Live Neo4j 2026.10.1 rejects the `+` spelling at the same 22N27
        // static gate, and the front end never folds the unary plus.
        ("RETURN left('abcdef', +2.9)", &integer_args::LEFT_LENGTH),
        ("RETURN right('abcdef', 2.9)", &integer_args::RIGHT_LENGTH),
        ("RETURN [1,2,3][1.0]", &integer_args::LIST_INDEX),
        ("RETURN [1,2,3][1.0..2]", &integer_args::SLICE_START),
        ("RETURN [1,2,3][1..2.9]", &integer_args::SLICE_END),
    ] {
        let q = parse_and_lower(src)
            .unwrap_or_else(|e| panic!("`{src}` must survive the front-end: {e}"));
        let error = plan(q, &mut |_| false).expect_err(&format!("`{src}` must fail at plan time"));
        assert_eq!(
            error,
            Error::invalid_argument_type(site.type_error),
            "`{src}`"
        );
    }
}

/// Neo4j's static type gate rejects a non-numeric literal too, not only a
/// float one: live 2026.10.1 reports `substring('abcdef','x')`,
/// `substring('abcdef',1,'x')`, `left('abcdef','x')`, `right('abcdef','x')`
/// and `range('a',5)` as `Neo.ClientError.Statement.SyntaxError` type
/// mismatches. A `null` start/length stays legal.
#[test]
fn non_numeric_literals_at_integer_argument_sites_fail_at_plan_time() {
    for (src, site) in [
        (
            "RETURN substring('abcdef', 'x')",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN substring('abcdef', 1, 'x')",
            &integer_args::SUBSTRING_LENGTH,
        ),
        (
            "RETURN substring('abcdef', null, 'x')",
            &integer_args::SUBSTRING_LENGTH,
        ),
        (
            "RETURN substring('abcdef', true)",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN substring('abcdef', [1])",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN substring('abcdef', {a: 1})",
            &integer_args::SUBSTRING_START,
        ),
        ("RETURN left('abcdef', 'x')", &integer_args::LEFT_LENGTH),
        ("RETURN right('abcdef', 'x')", &integer_args::RIGHT_LENGTH),
        ("RETURN range('a', 5)", &integer_args::RANGE_START),
        // A temporal constructor call over literals is folded to a
        // `RuntimeValue` by the front end; it is not an INTEGER literal
        // either (live Neo4j 2026.10.1: 22N27 TypeError).
        (
            "RETURN substring('abcdef', date('2020-01-01'))",
            &integer_args::SUBSTRING_START,
        ),
        (
            "RETURN range(date('2020-01-01'), 5)",
            &integer_args::RANGE_START,
        ),
    ] {
        let q = parse_and_lower(src)
            .unwrap_or_else(|e| panic!("`{src}` must survive the front-end: {e}"));
        let error = plan(q, &mut |_| false).expect_err(&format!("`{src}` must fail at plan time"));
        assert_eq!(
            error,
            Error::invalid_argument_type(site.type_error),
            "`{src}`"
        );
    }
    // A null start/length is legal (it nulls the call), and an integer
    // literal keeps its runtime range check.
    for src in [
        "RETURN substring('abcdef', null)",
        "RETURN substring('abcdef', null, null)",
        "RETURN substring('abcdef', 1)",
        "RETURN substring('abcdef', -1)",
        "RETURN left('abcdef', null)",
    ] {
        let q = parse_and_lower(src)
            .unwrap_or_else(|e| panic!("`{src}` must survive the front-end: {e}"));
        assert!(plan(q, &mut |_| false).is_ok(), "`{src}` must plan");
    }
}

#[test]
fn non_literal_floats_at_integer_argument_sites_plan_fine() {
    // Every non-literal float — a constant expression, a function call,
    // a parameter, a variable — is handled at runtime (ADR-0175's
    // recorded compile-vs-runtime deviation); planning must not reject
    // it.
    for src in [
        "RETURN range(0.0/0.0, 5)",
        "RETURN range(head([1.5, 'x']), 5)",
        "RETURN substring('abcdef', head([1.5, 'x']))",
        "RETURN left('abcdef', $n)",
        "RETURN [1,2,3][head([1.0, 'x'])]",
        "RETURN [1,2,3][head([1.5, 'x'])..2]",
        "UNWIND [1.5] AS n RETURN range(n, 5)",
    ] {
        let q = parse_and_lower(src)
            .unwrap_or_else(|e| panic!("`{src}` must survive the front-end: {e}"));
        assert!(plan(q, &mut |_| false).is_ok(), "`{src}` must plan");
    }
}
