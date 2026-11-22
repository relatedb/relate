//! The CIP-51/CIP-60 match-mode and path-mode pins: where the mode
//! phrases are legal (directly after `MATCH`, before the graph pattern --
//! the path-variable-first spelling matches live Neo4j in rejecting),
//! what they resolve to, how they bind the path variable, and the
//! combination rules an explicit mode runs -- split out of `patterns.rs`
//! per the tests/ per-topic organization.

use super::*;

#[test]
fn test_unbounded_qpp_allowed_inside_exists_but_not_at_top_level() {
    // REPEATABLE ELEMENTS rejects an unbounded quantified path pattern in a
    // top-level match, but an EXISTS body is an existence probe and accepts
    // it (PathModeHourglassGraph).
    parse(
        "MATCH REPEATABLE ELEMENTS q = (x)-->{3,6}(x) \
           WHERE EXISTS { MATCH REPEATABLE ELEMENTS p = (x)-->+({i:2})-->+({i:1}) } \
           RETURN q",
    )
    .expect("unbounded QPP inside EXISTS must parse");
    let err = parse("MATCH REPEATABLE ELEMENTS ()--+() RETURN 1")
        .expect_err("expected UnsafeRepeatableElements at the top level");
    assert!(
        err.message.starts_with("UnsafeRepeatableElements"),
        "wrong error: {}",
        err.message
    );
}

#[test]
fn test_match_mode_spellings_resolve_to_their_mode() {
    // Every position a match mode can be written: directly after `MATCH`
    // (the order the CIP-51 corpus's Neo4j-verified scenarios use), and
    // after `MATCH` but before a path variable declaration
    // (`MATCH REPEATABLE ELEMENTS p = ...`). The path-variable-first
    // spelling (`MATCH p = REPEATABLE ELEMENTS (...)`) is not a position
    // at all — Neo4j rejects it, and
    // `test_match_mode_phrase_after_a_path_variable_is_rejected` pins
    // that rejection. A bare pattern keeps the implicit DIFFERENT
    // RELATIONSHIPS default.
    for (q, want_mode) in [
        (
            "MATCH REPEATABLE ELEMENTS (a)-[r]->(b) RETURN a",
            MatchMode::RepeatableElements,
        ),
        (
            "MATCH REPEATABLE ELEMENTS p = (a)-[r]->(b) RETURN p",
            MatchMode::RepeatableElements,
        ),
        // The singular spelling of the same mode, in every position the
        // plural is admitted (live Neo4j accepts it like the plural).
        (
            "MATCH REPEATABLE ELEMENT (a)-[r]->(b) RETURN a",
            MatchMode::RepeatableElements,
        ),
        (
            "MATCH REPEATABLE ELEMENT p = (a)-[r]->(b) RETURN p",
            MatchMode::RepeatableElements,
        ),
        (
            "MATCH DIFFERENT RELATIONSHIPS (a)-[r]->(b) RETURN a",
            MatchMode::DifferentRelationships,
        ),
        (
            "MATCH DIFFERENT RELATIONSHIPS p = (a)-[r]->(b) RETURN p",
            MatchMode::DifferentRelationships,
        ),
        // The other mode's singular spelling, in the clause-initial
        // position.
        (
            "MATCH DIFFERENT RELATIONSHIP (a)-[r]->(b) RETURN a",
            MatchMode::DifferentRelationships,
        ),
        (
            "MATCH (a)-[r]->(b) RETURN a",
            MatchMode::DifferentRelationships,
        ),
    ] {
        let stmt = parse_ok(q);
        assert_eq!(match_clause(&stmt).match_mode, want_mode, "{q}");
    }
}

#[test]
fn test_match_mode_before_a_path_variable_binds_the_path_variable() {
    // `MATCH REPEATABLE ELEMENTS p = (pattern)`: the declared variable
    // lands on the first parsed path pattern.
    for q in [
        "MATCH REPEATABLE ELEMENTS p = (a)-[r]->(b) RETURN p",
        "MATCH DIFFERENT RELATIONSHIPS p = (a)-[r]->(b) RETURN p",
    ] {
        let stmt = parse_ok(q);
        assert_eq!(
            match_clause(&stmt).pattern[0].variable.as_ref(),
            Some(&SmallName::from_name("p")),
            "{q}: path variable"
        );
    }
}

/// A match-mode phrase after a path variable declaration — or starting
/// any later path of the comma list — is not a position the phrase can
/// be written in: Neo4j rejects `MATCH p = REPEATABLE ELEMENTS (...)`
/// with a syntax error, so the phrase is rejected with the authored
/// position error instead of over-accepting it as the clause's mode.
#[test]
fn test_match_mode_phrase_after_a_path_variable_is_rejected() {
    for q in [
        "MATCH p = REPEATABLE ELEMENTS (a)-[r]->(b) RETURN p",
        "MATCH p = REPEATABLE ELEMENT (a)-[r]->(b) RETURN p",
        "MATCH p = DIFFERENT RELATIONSHIPS (a)-[r]->(b) RETURN p",
        "MATCH p = DIFFERENT RELATIONSHIP (a)-[r]->(b) RETURN p",
        // The later paths of a comma list get the same rule.
        "MATCH (a), p = REPEATABLE ELEMENTS (b)-[r2]->(c) RETURN p",
        "MATCH (a), REPEATABLE ELEMENTS (b)-[r2]->(c) RETURN a",
        // OPTIONAL MATCH routes through the same reader.
        "OPTIONAL MATCH p = REPEATABLE ELEMENTS (a)-[r]->(b) RETURN p",
        // A shortestPath() pattern after the phrase cannot rescue it --
        // the position error fires before any combination rule.
        "MATCH p = REPEATABLE ELEMENTS shortestPath((a)-[r]->(b)) RETURN p",
    ] {
        let err = parse(q).unwrap_err();
        assert!(
            err.message.starts_with("InvalidPathModePosition"),
            "{q}: wrong error: {}",
            err.message
        );
    }
}

#[test]
fn test_explicit_match_mode_still_rejects_a_shortest_path_function() {
    // The explicitness of a written match-mode keyword is observable
    // through CIP-40's mixing check -- in the clause-initial spelling
    // and the before-path-variable spelling alike.
    for q in [
        "MATCH REPEATABLE ELEMENTS shortestPath((a)-[r]->(b)) RETURN p",
        "MATCH REPEATABLE ELEMENTS p = shortestPath((a)-[r]->(b)) RETURN p",
    ] {
        let err = parse(q).expect_err("expected a mixing error");
        assert!(
            err.message.starts_with("MixingShortestPathAndMatchMode"),
            "{q}: wrong error: {}",
            err.message
        );
    }
    // The implicit default never trips the check.
    assert!(parse("MATCH p = shortestPath((a)-[r]->(b)) RETURN p").is_ok());
}

#[test]
fn test_match_mode_phrase_before_path_variable_is_not_a_path_mode() {
    // A match-mode phrase never satisfies the *path*-mode prefix, and a
    // real path mode keeps its own order rule: `MATCH TRAIL p = ...`
    // stays an authored InvalidPathModePosition error (the spec harness's
    // `cip-eval` suite, Neo4j-verified) -- the same error family the
    // after-a-path-variable match-mode phrase rejects through.
    let err = parse("MATCH TRAIL p = (x)--{2}(x) RETURN count(*) AS count")
        .expect_err("expected InvalidPathModePosition");
    assert!(
        err.message.starts_with("InvalidPathModePosition"),
        "wrong error: {}",
        err.message
    );
}

#[test]
fn test_match_mode_runs_the_combination_rules_in_every_legal_spelling() {
    // Both legal spellings run through the same combination checks:
    // REPEATABLE ELEMENTS only combines with WALK and rejects unbounded
    // quantified path patterns.
    for (q, expected) in [
        (
            "MATCH REPEATABLE ELEMENTS p = TRAIL (a)-[r]->(b) RETURN p",
            "UnsupportedCombinationOfMatchModeAndPathMode",
        ),
        (
            "MATCH REPEATABLE ELEMENTS TRAIL (a)-[r]->(b) RETURN p",
            "UnsupportedCombinationOfMatchModeAndPathMode",
        ),
        (
            "MATCH REPEATABLE ELEMENTS p = ((a)-[r]->(b))+ RETURN p",
            "UnsafeRepeatableElements",
        ),
        (
            "MATCH REPEATABLE ELEMENTS ((a)-[r]->(b))+ RETURN p",
            "UnsafeRepeatableElements",
        ),
    ] {
        let err = parse(q).unwrap_err();
        assert!(
            err.message.starts_with(expected),
            "{q}: wrong error: {}",
            err.message
        );
    }
}

#[test]
fn test_path_mode_keywords_set_the_path_mode() {
    // CIP-51: the four path modes, in the variable-first order the CIP's
    // own grammar specifies (`<path variable declaration>` before the
    // `<path pattern prefix>`).
    for (q, want) in [
        ("MATCH p = WALK (a)-[r]->(b) RETURN p", PathMode::Walk),
        ("MATCH p = TRAIL (a)-[r]->(b) RETURN p", PathMode::Trail),
        ("MATCH p = ACYCLIC (a)-[r]->(b) RETURN p", PathMode::Acyclic),
        ("MATCH p = SIMPLE (a)-[r]->(b) RETURN p", PathMode::Simple),
        // And without a path variable, mode-only prefix.
        ("MATCH TRAIL (a)-[r]->(b) RETURN a", PathMode::Trail),
    ] {
        let stmt = parse_ok(q);
        assert_eq!(match_clause(&stmt).pattern[0].mode, Some(want), "{q}");
    }
    // No keyword: the implicit WALK default is left unset here and
    // resolved at validation.
    let stmt = parse_ok("MATCH (a)-[r]->(b) RETURN a");
    assert_eq!(match_clause(&stmt).pattern[0].mode, None);
}
