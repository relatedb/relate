//! ADR-0619: source-level `\uXXXX` unicode escapes, end-to-end through the
//! public `relate::Database` API. An escape spells its character in the two
//! positions that widen acceptance — whitespace between tokens and
//! identifier text — matching the reference server, which decodes the same
//! spellings; every other position keeps the historical rejection.

use crate::common;

use common::{connection, err_text, row};

#[test]
fn an_escape_spells_whitespace_between_tokens() {
    // The three separators a text-first corpus cannot write as characters
    // (each is a line terminator for its embedded-text format) plus a
    // space: the escape spelling is the only expressible form there, and
    // the engine must read it as the character it names.
    let mut conn = connection();
    for escape in [r"\u2028", r"\u0085", r"\u000b", r"\u0020"] {
        let query = format!("RETURN{escape}1 AS r");
        let result = row(&mut conn, &query);
        assert_eq!(result[0].as_int(), Some(1), "escape {escape}");
    }
}

#[test]
fn an_escape_spells_identifier_text() {
    let mut conn = connection();
    // An escaped identifier in both its binding and its use, and an
    // escaped keyword letter: one spelling of the same query.
    let result = row(&mut conn, r"WITH 1 AS \u0078 RETURN \u0078");
    assert_eq!(result[0].as_int(), Some(1));
    let result = row(&mut conn, r"\u0052ETURN 2");
    assert_eq!(result[0].as_int(), Some(2));
}

#[test]
fn an_escape_outside_the_widened_positions_is_rejected() {
    // ADR-0619's boundary: an escape may not spell a string quote or a
    // digit — the historical `unexpected character:` rejection, the same
    // class the reference server's own rejections take, never a silent
    // half-decode.
    let mut conn = connection();
    for query in [r"RETURN \u0027x\u0027", r"RETURN \u0031"] {
        let error = err_text(conn.query(query));
        assert!(error.contains("unexpected character"), "{query}: {error}");
    }
}
