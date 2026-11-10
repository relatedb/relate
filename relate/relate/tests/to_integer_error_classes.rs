//! `toInteger()`'s out-of-range string rejection carries the error *class*
//! the openCypher TCK pins for `NumberOutOfRange`: `ArgumentError` (List11),
//! the class `ErrorKind::NumberOutOfRange` maps to — so the kind travels as
//! data through `Error::argument_value_kind`, never as a message prefix a
//! binding would have to scan. Live Neo4j 2026.10.1 (probed 2026-11-10 via
//! the 6.4.1 Python driver) reports `Neo.ClientError.Statement.TypeError`
//! under GQLSTATUS 22003, a code both exception classes construct; the TCK
//! class for the `NumberOutOfRange` detail is `ArgumentError`.
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
/// INTEGER range is `ArgumentError: NumberOutOfRange`, while the `OrNull`
/// variant and the list projection turn it into null.
#[test]
fn to_integer_out_of_range_strings_are_argument_errors() {
    let mut conn = connection();
    for input in ["1e300", "9223372036854775808", "9223372036854775807.5"] {
        let query = format!("RETURN toInteger('{input}') AS v");
        // One run: the class and the message describe the same error.
        let error = conn.query(&query).unwrap_err();
        common::assert_argument_error(&error);
        // The kind travels as data, not as a prefix a binding would scan:
        // assert the typed variant, so the two variants that share a class
        // and a rendering cannot be swapped for one another.
        assert!(
            matches!(&error, relate::Error::ArgumentValue { .. }),
            "{query}: {error:?}"
        );
        assert!(
            error.to_string().starts_with(
                "semantic error: NumberOutOfRange: toInteger() expected 'input' to be \
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
