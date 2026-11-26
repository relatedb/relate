//! ADR-0503's correctness bar, end to end: a cached run is indistinguishable
//! in result, error and phase split from an uncached one — same rows for the
//! same statement, the same template's own literals for each statement, and a
//! hit's PARSE arm is the lookup, not a parse.

use super::*;

/// The one correctness property the whole design turns on: a template hit
/// binds THIS statement's literals as parameters — the same template with
/// different values returns each statement's own rows, in any order, over
/// any number of repeats.
#[test]
fn same_template_different_literals_return_the_right_rows() {
    let (_db, mut c) = users();
    for round in 0..3 {
        let one = c
            .query("MATCH (n:User {id: 1}) RETURN n.name AS name")
            .unwrap();
        assert_eq!(
            rows(&one),
            vec![vec![Value::String("ada".into())]],
            "round {round}"
        );
        let two = c
            .query("MATCH (n:User {id: 2}) RETURN n.name AS name")
            .unwrap();
        assert_eq!(
            rows(&two),
            vec![vec![Value::String("bob".into())]],
            "round {round}"
        );
    }
    // The string-typed key shares the same mechanics.
    let by_name = c
        .query("MATCH (n:User {name: 'bob'}) RETURN n.id AS id")
        .unwrap();
    assert_eq!(rows(&by_name), vec![vec![Value::Int(2)]]);
    let by_other_name = c
        .query("MATCH (n:User {name: 'cy'}) RETURN n.id AS id")
        .unwrap();
    assert_eq!(rows(&by_other_name), vec![vec![Value::Int(3)]]);
}

/// An exact repeat resolves through the raw-string fast path (the entry
/// remembers the last raw text and its values) and stays correct.
#[test]
fn exact_repeats_stay_correct() {
    let (_db, mut c) = users();
    for _ in 0..4 {
        let r = c
            .query("MATCH (n:User) WHERE n.id > 1 RETURN n.id AS id ORDER BY id")
            .unwrap();
        assert_eq!(rows(&r), vec![vec![Value::Int(2)], vec![Value::Int(3)]]);
    }
}

/// A list literal collapses to one parameter: same template, different
/// lists, each statement's own rows.
#[test]
fn in_lists_bind_this_statements_values() {
    let (_db, mut c) = users();
    let one = c
        .query("MATCH (n:User) WHERE n.id IN [1, 2] RETURN count(n) AS c")
        .unwrap();
    assert_eq!(rows(&one), vec![vec![Value::Int(2)]]);
    let two = c
        .query("MATCH (n:User) WHERE n.id IN [3] RETURN count(n) AS c")
        .unwrap();
    assert_eq!(rows(&two), vec![vec![Value::Int(1)]]);
    let none = c
        .query("MATCH (n:User) WHERE n.id IN [] RETURN count(n) AS c")
        .unwrap();
    assert_eq!(rows(&none), vec![vec![Value::Int(0)]]);
}

/// REL8-1120: a deeply nested literal list rides the payload boundary
/// (`plan_cache::engine_values`), whose depth guard is the hit path's one
/// value-dependent literal check beside the normalizer's aligned nesting
/// cap (`MAX_NESTING_DEPTH` == `MAX_VALUE_DEPTH`). A list well under both
/// caps caches and serves each same-template statement's own value; the
/// unit test in `plan_cache` pins the refusal past the cap.
#[test]
fn deep_literal_lists_bind_through_the_payload_boundary() {
    let (_db, mut c) = users();
    let deep = |core: &str| {
        let mut s = String::new();
        for _ in 0..400 {
            s.push('[');
        }
        s.push_str(core);
        for _ in 0..400 {
            s.push(']');
        }
        format!("RETURN {s} AS l")
    };
    let one = c.query(&deep("1")).unwrap();
    let one_again = c.query(&deep("1")).unwrap();
    assert_eq!(
        rows(&one),
        rows(&one_again),
        "a repeat serves its own value"
    );
    let two = c.query(&deep("2")).unwrap();
    assert_ne!(
        rows(&one),
        rows(&two),
        "a same-template statement binds its own core"
    );
    let two_again = c.query(&deep("2")).unwrap();
    assert_eq!(rows(&two), rows(&two_again));
}

/// REL8-1120's retry leg, pinned: the same template with a literal list
/// whose element types the compile rejects (a quantifier `%` over a
/// string list) is served by the cached plan and fails with the runtime
/// type error first — the retry re-runs the statement uncached, and the
/// caller sees the compile's own verdict and error class. The suite's
/// own bar (a cached run indistinguishable in result and error) holds
/// only through this leg.
#[test]
fn a_type_error_hit_retries_uncached_and_reports_the_compile_verdict() {
    let (_db, mut c) = users();
    // Warm the template: the numeric list inserts, the second statement
    // is a template hit.
    c.query("RETURN all(x IN [1, 2] WHERE x % 2 = 0) AS n")
        .unwrap();
    let warm = c
        .query("RETURN all(x IN [1, 2] WHERE x % 2 = 0) AS n")
        .unwrap();
    assert_eq!(rows(&warm), vec![vec![Value::Bool(false)]]);
    let err = c
        .query("RETURN all(x IN ['Clara'] WHERE x % 2 = 0) AS n")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Type mismatch") && err.contains("non-numeric"),
        "the compile's own verdict, not the runtime twin: {err}"
    );
}

/// Errors keep their class and text on repeats: a statement that fails
/// keeps failing the same way, cached or not.
#[test]
fn failing_statements_fail_identically_on_repeats() {
    let (_db, mut c) = users();
    for _ in 0..3 {
        let err = c
            .query("MATCH (n:User {id: 1}) RETURN n.missing AS x, n.missing AS x")
            .unwrap_err();
        assert!(err.to_string().contains("same name"), "{err}");
        // A pure parse error never caches anything.
        let syntax = c.query("MATCH (n:User {id: }) RETURN n").unwrap_err();
        assert!(syntax.to_string().contains("expected"), "{syntax}");
    }
}

/// The phase-split stamp stays honest: a hit's PARSE arm is the lookup,
/// not a parse — measurably below the first run's. The threshold is
/// generous (a cold parse is tens of µs; a lookup pass is single-digit),
/// so the assertion is about the order of magnitude, not a benchmark.
#[test]
fn hit_parse_phase_is_the_lookup_not_a_parse() {
    let (_db, mut c) = users();
    let statement = "MATCH (n:User {id: 1}) RETURN n.name AS name, n.id AS id ORDER BY id LIMIT 1";
    let first_parse_ns = c.query(statement).unwrap().header.phases.parse().as_nanos();
    let hit_parse_ns = min_parse(&mut c, statement);
    assert!(
        hit_parse_ns < first_parse_ns,
        "hit PARSE {hit_parse_ns} ns vs first {first_parse_ns} ns"
    );
}
