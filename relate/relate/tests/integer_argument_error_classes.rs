//! The runtime `NumberOutOfRange` sites carry Neo4j's `ArgumentError`
//! class (ADR-0175's 22N03 mapping), not the bare type class its legacy
//! `type error: NumberOutOfRange: …` rendering suggested.
//!
//! Live Neo4j 2026.10.1 (Bolt driver `Neo4jError.code`, probed 2026-11-10)
//! reports `Neo.ClientError.Statement.ArgumentError` for a `range()` zero
//! step, for a `substring()` / `left()` / `right()` position beyond
//! [0, 2147483647] and for a percentile outside [0, 1]. The FLOAT
//! slice-bound int32 check is ADR-0175's 2026.09-probed rule; 2026.10.1
//! returns `[]` there instead (recorded drift, see the ADR's probe table),
//! so it is pinned to the ADR, not to live.
//!
//! The runtime (ANY-typed) spelling is probed here because the engine has
//! no static float typing: a float *literal* at these sites is rejected at
//! plan time with `InvalidArgumentType` (ADR-0175's static-gate rule).

mod common;

use common::connection;

/// The runtime `NumberOutOfRange` sites, with the detail token each
/// carries.
const SITES: &[(&str, &str)] = &[
    (
        "RETURN range(1, 5, head([0.0/0.0, 'x'])) AS v",
        "NumberOutOfRange: range() step cannot be 0",
    ),
    (
        "RETURN substring('abcdef', head([1.0/0.0, 'x'])) AS v",
        "NumberOutOfRange: substring() start",
    ),
    (
        "RETURN substring('abcdef', head([-1.5, 'x'])) AS v",
        "NumberOutOfRange: substring() start",
    ),
    (
        "RETURN substring('abcdef', 1, head([1.0/0.0, 'x'])) AS v",
        "NumberOutOfRange: substring() length",
    ),
    (
        "RETURN left('abcdef', head([1.0/0.0, 'x'])) AS v",
        "NumberOutOfRange: left() length",
    ),
    (
        "RETURN right('abcdef', head([1.0/0.0, 'x'])) AS v",
        "NumberOutOfRange: right() length",
    ),
    (
        "RETURN [1,2,3][head([1.0/0.0, 'x'])..2] AS v",
        "NumberOutOfRange: slice start",
    ),
    (
        "RETURN percentileCont(1, 2.0) AS v",
        "NumberOutOfRange: percentile",
    ),
];

#[test]
fn number_out_of_range_sites_are_argument_errors() {
    let mut conn = connection();
    for (statement, _detail) in SITES {
        let error = conn.query(statement).unwrap_err();
        // The variant, not just the class pair both argument variants share:
        // a site reverting to the DDL `ArgumentError` must fail this.
        assert!(
            matches!(&error, relate_core::Error::ArgumentValue { .. }),
            "{statement}: {error}"
        );
        assert_eq!(
            relate::binding_error_metadata(&error),
            ("ArgumentError", "ArgumentError"),
            "{statement}: {error}"
        );
    }
}

#[test]
fn number_out_of_range_sites_keep_their_message_tokens() {
    let mut conn = connection();
    for (statement, detail) in SITES {
        let error = conn.query(statement).unwrap_err();
        assert!(error.to_string().contains(detail), "{statement}: {error}");
    }
}
