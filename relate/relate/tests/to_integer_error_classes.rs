//! `toInteger()`'s out-of-range string rejection carries the error *class*
//! live Neo4j reports: the type exception under GQLSTATUS 22003 (ADR-0565),
//! not the run-time argument class the integer-argument range sites carry.
//! The rejection is a value outside the argument's domain, so it renders
//! through `Error::invalid_argument_value` (the `TypeError` class).
//!
//! This file owns the `toInteger()` conversion only: the class and message
//! fragment of its out-of-range string site, plus the `OrNull` and list
//! projections that turn the same rejection into null. Other integer sites
//! are pinned by their own tests — arithmetic overflow in
//! `tests/checked_int_ops.rs`, the `range()` zero-step site in the
//! list/integer family.

mod common;

use common::{connection, list, rows};
use relate::Value;

/// `toInteger()`'s out-of-range string site: an exact decimal outside the
/// INTEGER range is the `TypeError` class, while the `OrNull` variant and
/// the list projection turn it into null.
#[test]
fn to_integer_out_of_range_strings_are_type_errors() {
    let mut conn = connection();
    for input in ["1e300", "9223372036854775808", "9223372036854775807.5"] {
        let query = format!("RETURN toInteger('{input}') AS v");
        // One run: the class and the message describe the same error.
        let error = conn.query(&query).unwrap_err();
        assert_eq!(
            relate::binding_error_metadata(&error).1,
            "TypeError",
            "{query}: {error}"
        );
        assert!(
            error.to_string().starts_with(
                "type error: InvalidArgumentValue: toInteger() expected 'input' to be \
                 of type INTEGER and in the range -9223372036854775808 to \
                 9223372036854775807 but found "
            ),
            "{query}: {error}"
        );
        assert!(error.to_string().ends_with(input), "{query}: {error}");
    }
}

/// The `OrNull` variant catches the same rejection as null, and the list
/// projection follows its own OrNull rules (ADR-0175): the scalar is null,
/// the projected list element is null.
#[test]
fn to_integer_or_null_projections_are_null() {
    let mut conn = connection();
    assert_eq!(
        rows(&mut conn, "RETURN toIntegerOrNull('1e300') AS v"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        rows(&mut conn, "RETURN toIntegerList(['1e300']) AS v"),
        vec![vec![list(vec![Value::Null])]]
    );
}
