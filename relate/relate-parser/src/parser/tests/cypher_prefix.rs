use super::*;

// ---- CIP-150: CYPHER version prefix (ADR-0584: CYPHER 25 only) ----

#[test]
fn test_cypher_25_prefix_is_transparent() {
    let stmt = parse("CYPHER 25 RETURN 1 AS n").unwrap();
    assert_eq!(stmt.clauses.len(), 1);
    assert!(matches!(stmt.clauses[0], Clause::Return(_)));
}

#[test]
fn test_non_25_integer_preambles_fail_with_the_authored_error() {
    // ADR-0584: CYPHER 25 is the one supported version, so every other
    // `CYPHER <n>` integer -- `CYPHER 5` included, previously accepted as
    // a no-op -- fails at parse with the one authored unsupported-version
    // error naming the requested value.
    for (src, requested) in [
        ("CYPHER 5 RETURN 1 AS n", "5"),
        ("CYPHER 99 RETURN 1 AS n", "99"),
        ("CYPHER 4 RETURN 1 AS n", "4"),
    ] {
        let err = parse(src).expect_err(&format!("{src} must be rejected"));
        assert_eq!(
            err.message,
            format!(
                "UnsupportedCypherVersion: CYPHER {requested} is not supported; \
                 the only supported Cypher version is CYPHER 25"
            ),
            "{src}"
        );
    }
}

#[test]
fn test_cypher_no_prefix_parses_the_same_body() {
    let stmt = parse("RETURN 1 AS n").unwrap();
    assert_eq!(
        stmt.clauses,
        parse("CYPHER 25 RETURN 1 AS n").unwrap().clauses
    );
}

#[test]
fn test_cypher_decimal_version_is_a_grammar_error() {
    // A non-integer version token keeps the 22N10 grammar rejection -- it
    // is not a version the engine could support or refuse by name.
    let err = parse("CYPHER 5.0 RETURN 1 AS n").expect_err("CYPHER 5.0 must be rejected");
    assert!(
        err.message
            .starts_with("SyntaxError: 22N10 invalid pre-parser option value"),
        "wrong error: {}",
        err.message
    );
}

#[test]
fn test_cypher_variable_named_cypher_is_unaffected() {
    // The CYPHER-prefix check only fires once, at the very start of
    // the whole query -- `cypher` used as an ordinary variable name
    // anywhere else must keep working.
    let stmt = parse("WITH 1 AS cypher RETURN cypher").unwrap();
    assert_eq!(stmt.clauses.len(), 2);
}

#[test]
fn test_cypher_same_version_repeated_is_accepted() {
    let stmt = parse("CYPHER 25 CYPHER 25 RETURN 1 AS n").unwrap();
    assert_eq!(stmt.clauses, parse("RETURN 1 AS n").unwrap().clauses);
}

#[test]
fn test_cypher_options_clause_without_a_version_is_accepted() {
    let stmt = parse("CYPHER planner=cost RETURN 1 AS n").unwrap();
    assert_eq!(stmt.clauses, parse("RETURN 1 AS n").unwrap().clauses);
}

#[test]
fn test_cypher_unrecognized_option_key_is_rejected() {
    assert!(parse("CYPHER foo=bar RETURN 1 AS n").is_err());
}

#[test]
fn test_cypher_conflicting_option_values_are_rejected() {
    assert!(parse("CYPHER 25 runtime=slotted runtime=pipelined RETURN 1 AS n").is_err());
}

#[test]
fn test_cypher_explain_before_version_is_accepted() {
    let stmt = parse("EXPLAIN CYPHER 25 runtime=slotted RETURN 1 AS n").unwrap();
    assert_eq!(stmt.preamble, PreambleMode::Explain);
    assert_eq!(stmt.clauses, parse("RETURN 1 AS n").unwrap().clauses);
}

#[test]
fn test_cypher_preamble_before_explain_is_accepted() {
    let stmt = parse("CYPHER 25 runtime=slotted EXPLAIN RETURN 1 AS n").unwrap();
    assert_eq!(stmt.preamble, PreambleMode::Explain);
    assert_eq!(stmt.clauses, parse("RETURN 1 AS n").unwrap().clauses);
}

#[test]
fn test_cypher_explain_inside_options_clause_is_rejected() {
    // Once EXPLAIN has ended a CYPHER options clause, a bare `key=value`
    // with no fresh CYPHER keyword falls through to ordinary statement
    // parsing and is a generic syntax error there.
    assert!(parse("CYPHER 25 EXPLAIN runtime=slotted RETURN 1 AS n").is_err());
}

// ---- Cypher 25 header + runtime option value validation (regression pins) ----

#[test]
fn test_cypher_25_runtime_parallel_is_accepted_and_ignored() {
    // The documented parallel-runtime header: accepted and ignored
    // (Relate has a single engine).
    let stmt = parse("CYPHER 25 runtime=parallel RETURN 1 AS n").unwrap();
    assert!(matches!(stmt.clauses[0], Clause::Return(_)));
}

#[test]
fn test_cypher_25_runtime_unknown_value_is_rejected_naming_valid_values() {
    // The value-side twin of the 22N07 key check: an unknown runtime
    // value fails with an authored 22N10 error naming the valid values.
    let err = parse("CYPHER 25 runtime=bogus RETURN 1 AS n").expect_err("expected a value error");
    assert!(
        err.message
            .starts_with("SyntaxError: 22N10 invalid pre-parser option value"),
        "wrong error: {}",
        err.message
    );
    assert!(
        err.message.contains("parallel"),
        "error must name the valid values: {}",
        err.message
    );
}

#[test]
fn test_cypher_runtime_option_accepts_the_runtime_vocabulary() {
    // The full runtime vocabulary of Neo4j's current line (its manual's
    // three runtimes); all accepted-and-ignored, under the one admitted
    // version and without one.
    for q in [
        "CYPHER 25 runtime=slotted RETURN 1 AS n",
        "CYPHER 25 runtime=pipelined RETURN 1 AS n",
        "CYPHER 25 runtime=parallel RETURN 1 AS n",
        "CYPHER runtime=parallel RETURN 1 AS n",
    ] {
        let stmt = parse(q).unwrap_or_else(|e| panic!("{q} must parse: {e}"));
        assert!(matches!(stmt.clauses[0], Clause::Return(_)), "{q}");
    }
}

#[test]
fn test_cypher_25_unrecognized_option_key_is_rejected() {
    // The 22N07 key check under the Cypher 25 preamble too.
    let err = parse("CYPHER 25 foo=bar RETURN 1 AS n").expect_err("expected a key error");
    assert!(
        err.message
            .starts_with("SyntaxError: 22N07 invalid pre-parser option key"),
        "wrong error: {}",
        err.message
    );
}
