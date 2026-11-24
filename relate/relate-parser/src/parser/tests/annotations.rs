use super::*;
use crate::lexer::ParseOptions;

// ---- CIP-281 Cypher annotations (the `cypher-annotations` gate) ----

fn on() -> ParseOptions {
    ParseOptions {
        cypher_annotations: true,
        ..ParseOptions::default()
    }
}

fn accepted(query: &str) -> Statement {
    parse_ok_with(query, on())
}

fn rejected(query: &str) -> String {
    parse_with_options(query, on())
        .expect_err(&format!("{query:?} must be rejected"))
        .message
}

#[test]
fn annotations_are_discarded_and_leave_the_clauses_unchanged() {
    let plain = accepted("MATCH (n) RETURN n");
    for query in [
        "@profile() MATCH (n) RETURN n",
        "@a(\"v\") MATCH (n) RETURN n",
        "@a(name: \"x\", type: 2) MATCH (n) RETURN n",
        "@a() @b(1) MATCH (n) @c(true) RETURN n",
    ] {
        assert_eq!(
            accepted(query).clauses.len(),
            plain.clauses.len(),
            "{query}"
        );
    }
}

#[test]
fn every_literal_shape_is_accepted() {
    for value in [
        "true",
        "\"s\"",
        "3",
        "-3",
        "+3",
        "1.5",
        "-1.5",
        "[]",
        "[1, 2]",
        "[[1], [2]]",
        "{}",
        "{a: 1, b: [1, 2]}",
    ] {
        accepted(&format!("@a({value}) RETURN 1"));
        accepted(&format!("@a(k: {value}) RETURN 1"));
    }
}

#[test]
fn annotations_apply_to_clauses_inside_subqueries_and_unions() {
    accepted("CALL { @a() MATCH (n) RETURN n } RETURN 1");
    accepted("@a() RETURN 1 UNION @b() RETURN 2");
    accepted("MATCH (n) @a(x: 1) WITH n @b() RETURN n");
}

#[test]
fn a_non_literal_parameter_is_rejected() {
    for value in [
        "null",
        "$p",
        "n",
        "1 + 2",
        "f(1)",
        "[n]",
        "{a: {b: 1}}",
        "[{a: 1}]",
    ] {
        let message = rejected(&format!("@a({value}) RETURN 1"));
        assert!(
            message.starts_with("InvalidAnnotation"),
            "{value}: {message}"
        );
    }
}

#[test]
fn two_or_more_parameters_must_all_be_named() {
    for query in [
        "@a(1, 2) RETURN 1",
        "@a(1, k: 2) RETURN 1",
        "@a(k: 1, 2) RETURN 1",
    ] {
        assert!(rejected(query).starts_with("InvalidAnnotation"), "{query}");
    }
    accepted("@a(k: 1) RETURN 1");
    accepted("@a(1) RETURN 1");
}

#[test]
fn a_parameter_name_is_unique_within_one_annotation() {
    assert!(rejected("@a(k: 1, k: 2) RETURN 1").starts_with("InvalidAnnotation"));
    accepted("@a(k: 1) @b(k: 2) RETURN 1");
}

#[test]
fn annotations_belong_before_a_clause_only() {
    for query in [
        "MATCH (n @a()) RETURN n",
        "MATCH (n) RETURN n @a()",
        "RETURN 1 + @a()",
        "@a()",
    ] {
        rejected(query);
    }
}

#[test]
fn the_gate_closed_keeps_the_unexpected_character_error() {
    for query in [
        "@profile() MATCH (n) RETURN n",
        "@a(1) RETURN 1",
        "@a(b c) RETURN 1",
    ] {
        let message = parse(query).unwrap_err().message;
        assert!(
            message.contains("unexpected character: @"),
            "{query}: {message}"
        );
    }
}

#[test]
fn an_annotation_never_splits_a_clause_from_its_paging_tail() {
    for query in [
        "MATCH (n) WITH n @a() ORDER BY n.x RETURN n",
        "MATCH (n) WITH n @a() LIMIT 2 RETURN n",
        "MATCH (n) WITH n @a() SKIP 1 RETURN n",
        "MATCH (n) WITH n @a() OFFSET 1 RETURN n",
        "MATCH (n) RETURN n @a() LIMIT 1",
        "MATCH (n) RETURN n @a() UNION MATCH (m) RETURN m",
    ] {
        let message = rejected(query);
        assert!(
            message.contains("InvalidAnnotation: an annotation must directly precede a clause"),
            "{query}: {message}"
        );
    }
    // The tail unannotated, and an annotation before a real clause, still parse.
    accepted("MATCH (n) WITH n ORDER BY n.x LIMIT 2 RETURN n");
    accepted("MATCH (n) WITH n @a() MATCH (m) RETURN n, m");
}

// ---- ADR-0610's reserved `@txMetadata`: the capture (not the discard) ----

#[test]
fn only_the_reserved_name_is_captured() {
    // Every other annotation is checked and dropped, whatever its params.
    for query in [
        "@txmetadata({a: 1}) RETURN 1",
        "@txMeta({a: 1}) RETURN 1",
        "@txmetadatax({a: 1}) RETURN 1",
    ] {
        assert!(accepted(query).tx_metadata.is_empty(), "{query}");
    }
}

#[test]
fn the_positional_map_spelling_is_captured_as_parsed() {
    let stmt = accepted("@txMetadata({agent: 'a', n: 2}) CREATE (n)");
    assert_eq!(stmt.tx_metadata.len(), 1);
    assert!(matches!(
        &stmt.tx_metadata[0],
        TxMetadataAnnotation::Positional(Expr::Map(_))
    ));
}

#[test]
fn the_named_spelling_is_captured_as_parsed() {
    let stmt = accepted("@txMetadata(agent: 'a', n: 2) CREATE (n)");
    assert_eq!(stmt.tx_metadata.len(), 1);
    match &stmt.tx_metadata[0] {
        TxMetadataAnnotation::Named(pairs) => {
            assert_eq!(pairs.len(), 2);
            assert_eq!(pairs[0].0.to_string(), "agent");
            assert_eq!(pairs[1].0.to_string(), "n");
        }
        other => panic!("named spelling captured as {other:?}"),
    }
}

#[test]
fn the_capture_rides_the_top_level_statement_in_clause_order() {
    let stmt = accepted(
        "@txMetadata({a: 1}) CREATE (n) \
         @txMetadata(b: 2) CALL { @txMetadata({c: 3}) CREATE (m) } \
         @txMetadata({d: 4}) RETURN 1 UNION @txMetadata({e: 5}) RETURN 2",
    );
    assert_eq!(stmt.tx_metadata.len(), 5);
    // A statement without the reserved annotation captures nothing, and
    // the zero-parameter spelling captures the empty map (the vacuous
    // all-named form).
    assert!(accepted("@a(1) RETURN 1").tx_metadata.is_empty());
    assert!(matches!(
        accepted("@txMetadata() RETURN 1").tx_metadata.as_slice(),
        [TxMetadataAnnotation::Named(pairs)] if pairs.is_empty()
    ));
}

#[test]
fn a_positional_non_map_is_captured_not_rejected() {
    // The CIP-281 literal checks govern every annotation equally; whether
    // a positional parameter must be a map is the channel's grammar, not
    // the annotation grammar -- the parser captures it as parsed and the
    // channel's gate decides (ADR-0610: `@txMetadata(5)` is the plain
    // discard while `value-provenance` is off).
    for query in [
        "@txMetadata(5) RETURN 1",
        "@txMetadata('x') RETURN 1",
        "@txMetadata([1, 2]) RETURN 1",
    ] {
        assert_eq!(accepted(query).tx_metadata.len(), 1, "{query}");
    }
}
