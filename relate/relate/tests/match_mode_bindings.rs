//! The BINDINGS spellings of the match modes (ADR-0679): the third word
//! extends either phrase and maps onto its element twin's mode, so the
//! pool semantics are the twin's — repeats allowed under the REPEATABLE
//! form, the uniqueness pool on under the DIFFERENT form.

mod common;

use common::{connection, err_text};

#[test]
fn the_bindings_spellings_run_their_twins_pool() {
    let mut c = connection();
    c.query("CREATE ()-[:R]->()").unwrap();
    for (mode, expected) in [
        ("REPEATABLE ELEMENTS", 1),
        ("REPEATABLE ELEMENT", 1),
        ("REPEATABLE ELEMENT BINDINGS", 1),
        ("REPEATABLE ELEMENTS BINDINGS", 1),
        ("DIFFERENT RELATIONSHIPS", 0),
        ("DIFFERENT RELATIONSHIP", 0),
        ("DIFFERENT RELATIONSHIP BINDINGS", 0),
        ("DIFFERENT RELATIONSHIPS BINDINGS", 0),
    ] {
        let result = c
            .query(&format!(
                "MATCH {mode} ()-->()--() RETURN count(*) AS result"
            ))
            .unwrap();
        assert_eq!(
            result.rows[0][0],
            relate::Value::Int(expected),
            "{mode}: the single relationship repeat is the twin's verdict"
        );
    }
}

/// The repeated-variable rows of the match-mode corpus, on both BINDINGS
/// twins' sides: an equijoin under the REPEATABLE form, no solutions under
/// the DIFFERENT form (the element forms' rows, verbatim).
#[test]
fn the_bindings_spellings_carry_the_repeated_variable_rules() {
    let mut c = connection();
    c.query("CREATE (:A)-[:R]->(:B)-[:S]->(:C)").unwrap();
    let mut count = |query: &str| -> i64 {
        let result = c.query(query).unwrap();
        match &result.rows[0][0] {
            relate::Value::Int(n) => *n,
            other => panic!("{query}: expected a count, got {other:?}"),
        }
    };
    assert_eq!(
        count("MATCH REPEATABLE ELEMENT BINDINGS ()-[q]-()-[q]-() RETURN count(*) AS result"),
        4,
        "the equijoin row count"
    );
    assert_eq!(
        count(
            "MATCH DIFFERENT RELATIONSHIP BINDINGS ()-[q]-()-[q]-() \
             RETURN count(*) AS result"
        ),
        0,
        "the uniqueness pool rejects the equijoin"
    );
}

/// The combination rules follow the twin as well: the REPEATABLE form
/// still rejects an unbounded quantified path pattern, and still refuses
/// the TRAIL path mode.
#[test]
fn the_bindings_spellings_run_the_twins_combination_rules() {
    let mut c = connection();
    // `None` = the statement runs; `Some(needle)` = the expected error
    // detail — the typed shape of the outcome column, no sentinel string.
    for (query, expected) in [
        (
            "MATCH REPEATABLE ELEMENT BINDINGS (a)--{1,}(b) RETURN 1",
            Some("UnsafeRepeatableElements"),
        ),
        (
            "MATCH DIFFERENT RELATIONSHIP BINDINGS (a)-->(b) RETURN 1",
            None,
        ),
    ] {
        let outcome = c.query(query);
        match expected {
            None => assert!(outcome.is_ok(), "{query}: {}", err_text(outcome)),
            Some(needle) => {
                let error = err_text(outcome);
                assert!(error.contains(needle), "{query}: {error}");
            }
        }
    }
}
