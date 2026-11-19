//! The pattern-position policy's tests, split from `expressions.rs`'s
//! module (LABS-311 size gate).

use super::*;
use crate::validate::parse_and_validate_expression_constraints;

/// A pattern expression is a predicate in the boolean positions
/// (`NOT`, `AND`/`OR`/`XOR`, a `CASE WHEN` condition, a comprehension
/// filter) and stays rejected as a value (a bare projection item).
#[test]
fn pattern_predicates_are_legal_in_boolean_positions_only() {
    for query in [
        "MATCH (n) RETURN NOT (n)--() AS x",
        "MATCH (n) RETURN (n)--() OR (n)--()--() AS x",
        "MATCH (n) RETURN CASE WHEN (n)--() THEN 1 ELSE 0 END AS x",
        "MATCH (n) RETURN [(n)-->(m) WHERE (m)--() | m] AS x",
    ] {
        parse_and_validate_expression_constraints(query).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
    let bare = parse_and_validate_expression_constraints("MATCH (n) RETURN (n)--() AS x")
        .expect_err("a bare pattern projection item stays rejected");
    assert_eq!(bare.kind(), ErrorKind::UnexpectedSyntax);
}

/// Regression: the boolean-operand exemption covers the pattern operand
/// itself, not a whole value subexpression. A pattern buried under a
/// value position inside a boolean operand — `size(<pattern>)` here —
/// stays rejected; pruning the operand's whole subtree had let it
/// escape.
#[test]
fn a_pattern_in_a_value_subexpression_of_a_boolean_operand_is_rejected() {
    let error =
        parse_and_validate_expression_constraints("MATCH (n) RETURN (n)--() OR size((n)--()) AS x")
            .expect_err("a pattern in a value subexpression stays rejected");
    assert_eq!(error.kind(), ErrorKind::UnexpectedSyntax);
}

/// A SET value rides the same value-position policy as a projection
/// item: `SET n.p1 = exists((n)--())` plans (the repro's shape) and so
/// does a pattern inside a quantifier filter's slot, while a bare
/// pattern value and a pattern buried in a value subexpression keep
/// their rejections with the SET spelling.
#[test]
fn set_values_share_the_projection_pattern_policy() {
    parse_and_validate_expression_constraints(
        "MATCH (n:L1) SET n.p1 = exists((n)-[:R]->(:L2)) RETURN n",
    )
    .expect("the legacy exists(<pattern>) form is legal in a SET value");
    parse_and_validate_expression_constraints(
        "MATCH (n:L1) SET n.p1 = all(x IN ['a', 'b'] WHERE (n)-[:R]->(:L2)) RETURN n",
    )
    .expect("a pattern in a quantifier filter is legal in a SET value");
    for query in [
        "MATCH (n:L1) SET n.p1 = (n)-[:R]->(:L2) RETURN n",
        "MATCH (n:L1) SET n.p1 = size((n)-[:R]->(:L2)) RETURN n",
        "MATCH (n:L1) SET n.p1 = exists(1 + (n)-[:R]->(:L2)) RETURN n",
    ] {
        let error = parse_and_validate_expression_constraints(query)
            .expect_err(&format!("{query}: expected the SET-value rejection"));
        assert_eq!(
            error.kind(),
            ErrorKind::UnexpectedSyntax,
            "{query}: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("pattern expressions are not allowed in SET values"),
            "{query}: {error}"
        );
    }
}

/// A parenthesized group is transparent for its *position*: a pattern
/// behind parentheses is a predicate when the group is one, and stays a
/// value when the group is a bare projection item (Neo4j accepts
/// `NOT ((n)--())` and rejects `RETURN ((n)--())` with 42I34).
#[test]
fn parens_are_transparent_for_a_patterns_position() {
    parse_and_validate_expression_constraints("MATCH (n) RETURN NOT ((n)--()) AS x")
        .expect("a parenthesized pattern predicate stays legal under NOT");
    parse_and_validate_expression_constraints("MATCH (n) RETURN (n)--() OR (((n)--())) AS x")
        .expect("a parenthesized pattern predicate stays legal as an OR operand");
    let error = parse_and_validate_expression_constraints("MATCH (n) RETURN ((n)--()) AS x")
        .expect_err("a parenthesized pattern is still a value as a bare projection item");
    assert_eq!(error.kind(), ErrorKind::UnexpectedSyntax);
}

/// Regression: a `CASE WHEN` condition is a predicate position, so its
/// pattern predicate must be validated in a WHERE too (unbound pattern
/// variables rejected), not skipped by the WHERE walk.
#[test]
fn a_case_when_pattern_in_a_where_is_validated() {
    parse_and_validate_expression_constraints(
        "MATCH (n)--(m) WHERE CASE WHEN (n)--(m) THEN true ELSE false END RETURN n",
    )
    .expect("a bound CASE WHEN pattern predicate is legal in a WHERE");
    let error = parse_and_validate_expression_constraints(
        "MATCH (n) WHERE CASE WHEN (n)-->(m) THEN true ELSE false END RETURN n",
    )
    .expect_err("an unbound variable in a CASE WHEN pattern must be rejected");
    assert_eq!(error.kind(), ErrorKind::UndefinedVariable);
}

/// The `exists(<pattern>)` exemption covers only a pattern that *is* the
/// call's sole argument. A pattern buried inside a larger argument is
/// not the legacy form, so the value-position walk descends into it and
/// rejects it like any other value-position pattern.
#[test]
fn exists_exemption_covers_only_a_direct_pattern_argument() {
    parse_and_validate_expression_constraints("MATCH (n) RETURN exists((n)--()) AS x")
        .expect("the legacy exists(<pattern>) form is legal in a projection");
    // Paren transparency: the argument wrapped in a group is still the
    // legacy form, exactly as `NOT ((n)--())` is still a predicate.
    parse_and_validate_expression_constraints("MATCH (n) RETURN exists(((n)--())) AS x")
        .expect("a paren-wrapped exists(<pattern>) argument is the legacy form");
    let error =
        parse_and_validate_expression_constraints("MATCH (n) RETURN exists(1 + (n)--()) AS x")
            .expect_err("a pattern under a non-pattern exists() argument is rejected");
    assert_eq!(error.kind(), ErrorKind::UnexpectedSyntax);
    assert_eq!(
        error.to_string(),
        "UnexpectedSyntax: pattern expressions are not allowed in RETURN projection"
    );
}

/// Regression: a parenthesized variable in a predicate position of a
/// WHERE is the single-node predicate rejection — the descent into
/// predicate positions is shape-agnostic, so the visitor reaches the
/// group even though it is not a pattern home.
#[test]
fn a_parenthesized_variable_in_a_where_predicate_position_is_rejected() {
    let error =
        parse_and_validate_expression_constraints("MATCH (n) WHERE (n) AND (n)--() RETURN n")
            .expect_err("a lone parenthesized variable is not a predicate");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
    assert_eq!(
        error.to_string(),
        "InvalidArgumentType: a single node is not a valid pattern predicate"
    );
}

/// A pattern predicate inside a comprehension's WHERE filter binds
/// against the comprehension's OWN variable, not the enclosing scope —
/// the WHERE walk must not descend into that slot and validate it there
/// (regression: the shape-agnostic descent round added the comprehension
/// filter as a descent home, false-rejecting the bound variable).
#[test]
fn a_pattern_predicate_in_a_comprehension_filter_binds_locally() {
    for query in [
        "MATCH p=(a:L1)-[:R]->(b:L2) \
         WHERE size([n IN nodes(p) WHERE (n)-[:R]->(b) | n]) > 0 RETURN p",
        "MATCH (a:L1) WHERE NOT [n IN [a] WHERE (n)-[:R]->(a) | 1] RETURN a",
    ] {
        parse_and_validate_expression_constraints(query).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
}

/// A quantifier's WHERE predicate — `all/any/none/single(x IN list WHERE
/// pred)` — is a predicate home of the comprehension-filter kind: it binds
/// its own loop variable, so a pattern predicate inside it is legal in a
/// projection (the LABS-1610 repro's shape) and in a WITH item, while the
/// quantifier's LIST slot stays a value position (LABS-1610).
#[test]
fn quantifier_predicates_are_pattern_homes_in_projections() {
    for query in [
        "RETURN all(x IN $p1 WHERE (:L1 {p1: x})<-[:R]-(:L2 {p1: $p2})) AS result",
        "RETURN all(x IN ['a', 'b'] WHERE (:L1 {p1: x})<-[:R]-(:L2 {p1: 'z'})) AS result",
        "RETURN any(x IN ['a', 'b'] WHERE (:L1 {p1: x})<-[:R]-(:L2 {p1: 'z'})) AS result",
        "RETURN none(x IN ['a', 'b'] WHERE (:L1 {p1: x})<-[:R]-(:L2 {p1: 'z'})) AS result",
        "RETURN single(x IN ['a', 'b'] WHERE (:L1 {p1: x})<-[:R]-(:L2 {p1: 'z'})) AS result",
        "MATCH (n:L1) WITH all(x IN ['a'] WHERE (n)-[:R]->(:L2)) AS result RETURN result",
    ] {
        parse_and_validate_expression_constraints(query).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
    // The exemption is the predicate slot only: the quantifier's LIST is a
    // value position, so a pattern buried in a list element keeps the
    // projection rejection.
    let error = parse_and_validate_expression_constraints(
        "MATCH (n) RETURN all(x IN [size((n)--())] WHERE x) AS y",
    )
    .expect_err("a pattern in the quantifier's list slot stays a value position");
    assert_eq!(error.kind(), ErrorKind::UnexpectedSyntax);
    assert_eq!(
        error.to_string(),
        "UnexpectedSyntax: pattern expressions are not allowed in RETURN projection"
    );
}

/// A pattern predicate inside a quantifier's WHERE predicate binds against
/// the quantifier's OWN loop variable, not the enclosing scope — the WHERE
/// walk must not descend into that slot and validate it there (the
/// comprehension-filter precedent; a descent would false-reject the loop
/// variable of the second query). An unbound name inside the pattern keeps
/// its rejection from the planner's binder, pinned engine-side (LABS-1610).
#[test]
fn a_pattern_predicate_in_a_quantifier_filter_binds_locally() {
    for query in [
        "MATCH (n) WHERE all(x IN [1] WHERE (n)-[:R]->(n)) RETURN n",
        "MATCH (a:L1) WHERE NOT all(x IN [a] WHERE (x)-[:R]->(a)) RETURN a",
    ] {
        parse_and_validate_expression_constraints(query).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
}

/// `allReduce`'s trailing predicate is the quantifier-filter shape too —
/// a non-optional slot evaluated per element with the accumulator and the
/// loop variable bound — so a pattern predicate inside it is a predicate
/// home in a projection and in a WITH item (the one table
/// `binds_own_variable_slot` carries beside the comprehension filters and
/// quantifier predicates), and binds its own loop variable in a WHERE.
#[test]
fn allreduce_predicates_are_pattern_homes() {
    for query in [
        "MATCH (n) RETURN allReduce(acc = 0, x IN [1] | acc + x, (n)--()) AS r",
        "MATCH (n:L1) WITH allReduce(acc = 0, x IN [1] | acc, (n)-[:R]->(:L2)) AS r RETURN r",
        "MATCH (a:L1) WHERE NOT allReduce(acc = 0, x IN [a] | acc + 1, (x)-[:R]->(a)) RETURN a",
    ] {
        parse_and_validate_expression_constraints(query).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
}
