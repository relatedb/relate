//! DDL command validation errors carry the error *class* Neo4j surfaces
//! (ADR-0489): the statement-shape rules (an unsupported property type, an
//! unknown `OPTIONS` key, the wrong property count for a single-property
//! index kind, a malformed graph-type body) are Neo4j front-end
//! `SemanticError.*` constructions and therefore surface as `SyntaxError`;
//! a rejected `indexConfig` value is the argument validator's
//! `ArgumentError`.
//!
//! The same rules carry a different class by *provenance*: a whole-map
//! `OPTIONS $param` is inspected at run time, so its rejections are the
//! argument validator's. Each parameter spelling is asserted beside its
//! literal twin here rather than in a second class table.
//!
//! Every expectation here was verified against
//! `neo4j:2026.09-enterprise` (Cypher 25) through the Bolt driver's
//! `Neo4jError.code`: the shape rules report
//! `Neo.ClientError.Statement.SyntaxError`, the config rules
//! `Neo.ClientError.General.InvalidArguments`.

mod common;

use relate::Value;

use common::{connection, empty_map, error_class_literal, map, params};

/// Live's pinned key-rejection text, byte-for-byte: `key` as the rejecting
/// spelling renders it (the parameter spelling lowercases it).
fn key_rejection(key: &str) -> String {
    format!("Invalid input '{key}' for 'OPTIONS'. Expected 'indexConfig'.")
}

#[test]
fn property_type_constraint_shape_rules_are_syntax_errors() {
    let mut conn = connection();
    for statement in [
        // CIP-85: lists cannot have a nullable inner type.
        "CREATE CONSTRAINT c1 FOR (n:Person) REQUIRE n.name IS TYPED LIST<STRING>",
        // No top-level NOT NULL.
        "CREATE CONSTRAINT c2 FOR (n:Person) REQUIRE n.name IS TYPED STRING NOT NULL",
        // Composite kinds are not property-storable.
        "CREATE CONSTRAINT c3 FOR (n:Person) REQUIRE n.name IS TYPED NODE",
        "CREATE CONSTRAINT c4 FOR (n:Person) REQUIRE n.name IS TYPED MAP",
        // Lists cannot nest.
        "CREATE CONSTRAINT c5 FOR (n:Person) REQUIRE n.name IS TYPED LIST<LIST<STRING NOT NULL> NOT NULL>",
    ] {
        assert_eq!(error_class_literal(&mut conn, statement), "SyntaxError", "{statement}");
    }
}

#[test]
fn constraint_options_key_is_a_syntax_error() {
    let mut conn = connection();
    // Cypher 25 admits only `indexConfig`; a literal invalid key is the
    // front end's rejection, carrying live 2026.10.1's message text (its
    // driver code stays SyntaxError — the 22N04 GQL status rides inside
    // the message).
    let error = conn
        .query(
            "CREATE CONSTRAINT c FOR (n:Person) REQUIRE n.name IS UNIQUE \
             OPTIONS {indexProvider: 'text-1.0'}",
        )
        .unwrap_err();
    common::assert_syntax_error(&error);
    assert!(
        error.to_string().contains(&key_rejection("indexProvider")),
        "{error}"
    );
}

/// The parameter twin of the shape rules: a whole-map `OPTIONS $param` is
/// inspected at run time, so an unknown key, a rejected provider, and a
/// non-map config value are all the argument validator's class. A non-map
/// parameter itself is the same class.
#[test]
fn constraint_options_parameter_rejections_are_argument_errors() {
    let mut conn = connection();
    for (options, rejected_key) in [
        // The whole map is not a MAP.
        (Value::Int(23), None),
        // An admitted key with a rejected value shape.
        (map(&[("indexConfig", Value::Int(2))]), None),
        // A key the constraint does not admit keeps the argument validator's
        // class in the parameter spelling, with live's message text (its key
        // lowercased, as live does).
        (
            map(&[("indexProvider", Value::String("range-1.0".into()))]),
            Some("indexprovider"),
        ),
    ] {
        let error = conn
            .query_with_params(
                "CREATE CONSTRAINT c FOR (n:L) REQUIRE (n.p) IS UNIQUE OPTIONS $opts",
                params(&[("opts", options)]),
            )
            .unwrap_err();
        common::assert_argument_error(&error);
        if let Some(key) = rejected_key {
            assert!(error.to_string().contains(&key_rejection(key)), "{error}");
        }
    }
}

#[test]
fn constraint_index_config_value_is_an_argument_error() {
    let mut conn = connection();
    for statement in [
        "CREATE CONSTRAINT c1 FOR (n:Person) REQUIRE n.name IS UNIQUE \
         OPTIONS {indexConfig: {`vector.dimensions`: 3}}",
        "CREATE CONSTRAINT c2 FOR (n:Person) REQUIRE n.name IS UNIQUE \
         OPTIONS {indexConfig: 'not-a-map'}",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "ArgumentError",
            "{statement}"
        );
    }
}

/// Neo4j validates the `OPTIONS` shape at compile time, ahead of the
/// schema-conflict checks: an invalid key wins over an equivalent
/// constraint that already exists.
#[test]
fn constraint_options_shape_wins_over_the_equivalent_rule_gate() {
    let mut conn = connection();
    conn.query("CREATE CONSTRAINT seed FOR (n:Person) REQUIRE n.name IS UNIQUE")
        .unwrap();
    assert_eq!(
        error_class_literal(
            &mut conn,
            "CREATE CONSTRAINT seed2 FOR (n:Person) REQUIRE n.name IS UNIQUE \
             OPTIONS {indexProvider: 'range-1.0'}"
        ),
        "SyntaxError"
    );
}

/// An existence constraint takes no options at all, and the parameter map
/// is validated before the `IF NOT EXISTS` short-circuit (the same ordering
/// the literal form has).
#[test]
fn constraint_options_parameter_is_validated_before_if_not_exists() {
    let mut conn = connection();
    conn.query("CREATE CONSTRAINT c FOR (n:L) REQUIRE (n.p) IS NOT NULL")
        .unwrap();
    let error = conn
        .query_with_params(
            "CREATE CONSTRAINT c IF NOT EXISTS FOR (n:L) REQUIRE (n.p) IS NOT NULL OPTIONS $opts",
            params(&[("opts", map(&[("indexConfig", empty_map())]))]),
        )
        .unwrap_err();
    common::assert_argument_error(&error);
}

/// The KEY is a compile-time shape rule: an invalid key is rejected before
/// its value expression is lowered, so an absent parameter in the value slot
/// cannot turn the syntax-class rejection into `ParameterMissing`.
#[test]
fn constraint_options_key_is_rejected_before_the_value_is_evaluated() {
    let mut conn = connection();
    assert_eq!(
        error_class_literal(
            &mut conn,
            "CREATE CONSTRAINT c FOR (n:Person) REQUIRE n.x IS UNIQUE \
             OPTIONS {indexProvider: $missing}"
        ),
        "SyntaxError"
    );
}

/// The index twin of the constraint key test: the literal key allowlist is
/// checked before any value expression is lowered, so a failing value cannot
/// mask the syntax-class shape rejection — for an unknown key and for the
/// Cypher-25-removed `indexProvider` alike.
#[test]
fn index_options_key_is_rejected_before_the_value_is_evaluated() {
    let mut conn = connection();
    for statement in [
        "CREATE RANGE INDEX i FOR (n:Label) ON (n.prop) OPTIONS {bogus: $missing}",
        "CREATE RANGE INDEX i FOR (n:Label) ON (n.prop) OPTIONS {indexProvider: $missing}",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "SyntaxError",
            "{statement}"
        );
    }
}

/// The index twin of the constraint ordering test: the shape block runs
/// before the conflict gate, so an equivalent-rule skip cannot mask an
/// invalid OPTIONS map.
#[test]
fn index_options_shape_wins_over_the_equivalent_rule_gate() {
    let mut conn = connection();
    conn.query("CREATE RANGE INDEX seed FOR (n:Label) ON (n.prop)")
        .unwrap();
    assert_eq!(
        error_class_literal(
            &mut conn,
            "CREATE RANGE INDEX seed2 FOR (n:Label) ON (n.prop) OPTIONS {bogus: 1}"
        ),
        "SyntaxError"
    );
}

#[test]
fn index_shape_rules_are_syntax_errors() {
    let mut conn = connection();
    for statement in [
        // A single-property index kind given two properties.
        "CREATE TEXT INDEX i1 FOR (n:Label) ON (n.a, n.b)",
        "CREATE POINT INDEX i2 FOR (n:Label) ON (n.a, n.b)",
        // An `ON EACH [...]` bracket list is the parser's own duplicate rule.
        "CREATE FULLTEXT INDEX i6 FOR (n:Label) ON EACH [n.p, n.p]",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "SyntaxError",
            "{statement}"
        );
    }
}

/// Literal OPTIONS key rejections keep the syntax class and carry live
/// 2026.10.1's message text — pinned byte-for-byte for an unknown key and
/// for the removed `indexProvider` alike.
#[test]
fn index_options_key_rejections_name_the_key() {
    let mut conn = connection();
    for (statement, key) in [
        (
            "CREATE RANGE INDEX i3 FOR (n:Label) ON (n.prop) OPTIONS {bogus: 1}",
            "bogus",
        ),
        (
            "CREATE RANGE INDEX i4 FOR (n:Label) ON (n.prop) \
             OPTIONS {indexProvider: 'native-btree-1.0'}",
            "indexProvider",
        ),
        (
            "CREATE LOOKUP INDEX i5 FOR (n) ON EACH labels(n) \
             OPTIONS {indexProvider: 'vector-2026.08'}",
            "indexProvider",
        ),
    ] {
        let error = conn.query(statement).unwrap_err();
        common::assert_syntax_error(&error);
        assert!(error.to_string().contains(&key_rejection(key)), "{error}");
    }
}

/// The same rejections reached through a whole-map `OPTIONS $param` are the
/// run-time argument validator's, not the parser's: the removed
/// `indexProvider` key (whatever its payload — the key alone is the
/// rejection, its message key lowercased as live does) and a non-map
/// parameter.
#[test]
fn index_options_parameter_rejections_are_argument_errors() {
    let mut conn = connection();
    for payload in [Value::String("fulltext-2.0".into()), Value::Null] {
        let error = conn
            .query_with_params(
                "CREATE TEXT INDEX i FOR (n:Label) ON (n.prop) OPTIONS $options",
                params(&[("options", map(&[("indexProvider", payload)]))]),
            )
            .unwrap_err();
        common::assert_argument_error(&error);
        assert!(
            error.to_string().contains(&key_rejection("indexprovider")),
            "{error}"
        );
    }
    // A non-map parameter stays the argument validator's rejection (live
    // 2026.10.1 words it 22N27 with a different message — a documented
    // divergence, error-class wave backlog).
    let error = conn
        .query_with_params(
            "CREATE TEXT INDEX i2 FOR (n:Label) ON (n.prop) OPTIONS $options",
            params(&[("options", Value::String("nope".into()))]),
        )
        .unwrap_err();
    common::assert_argument_error(&error);
}

#[test]
fn index_config_value_rejections_are_argument_errors() {
    let mut conn = connection();
    for statement in [
        // A range index cannot consume a point setting.
        "CREATE RANGE INDEX i1 FOR (n:Label) ON (n.prop) \
         OPTIONS {indexConfig: {`spatial.cartesian.max`: true}}",
        // Vector config: unknown key, out-of-range dimension, wrong leaf type.
        "CREATE VECTOR INDEX i2 FOR (n:Label) ON (n.emb) \
         OPTIONS {indexConfig: {`vector.bogus`: 1}}",
        "CREATE VECTOR INDEX i3 FOR (n:Label) ON (n.emb) \
         OPTIONS {indexConfig: {`vector.dimensions`: 9999}}",
        "CREATE VECTOR INDEX i4 FOR (n:Label) ON (n.emb) \
         OPTIONS {indexConfig: {`vector.quantization.enabled`: 1}}",
        "CREATE VECTOR INDEX i5 FOR (n:Label) ON (n.emb) \
         OPTIONS {indexConfig: {`vector.similarity_function`: 'bogus'}}",
        // FULLTEXT: an unknown setting and a family another kind owns.
        "CREATE FULLTEXT INDEX i6 FOR (n:L) ON EACH [n.p] \
         OPTIONS {indexConfig: {`not.a.setting`: false}}",
        "CREATE FULLTEXT INDEX i7 FOR (n:L) ON EACH [n.p] \
         OPTIONS {indexConfig: {`spatial.cartesian.max`: true}}",
        // POINT: an unknown setting.
        "CREATE POINT INDEX i8 FOR (n:L) ON (n.p) \
         OPTIONS {indexConfig: {`not.a.setting`: [4.0, 2.0]}}",
        // LOOKUP: any non-empty config.
        "CREATE LOOKUP INDEX i9 FOR (n) ON EACH labels(n) \
         OPTIONS {indexConfig: {`not.a.setting`: false}}",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "ArgumentError",
            "{statement}"
        );
    }
}

/// The duplicated-token rule (22N76) is the executor's semantic rejection
/// for a plain `ON (…, …)` list, while the bracket-list spelling is the
/// parser's own duplicate rule (a syntax class).
#[test]
fn index_duplicated_tokens_are_semantic_errors() {
    let mut conn = connection();
    for statement in [
        "CREATE INDEX i FOR (n:L) ON (n.p, n.p)",
        "CREATE RANGE INDEX i FOR (n:L) ON (n.p, n.p)",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "SemanticError",
            "{statement}"
        );
    }
}

/// Index and constraint names share one namespace (a semantic rejection);
/// an index-backed constraint's own index name is the one `IF NOT EXISTS`
/// no-op.
#[test]
fn index_name_shares_the_constraint_namespace() {
    let mut conn = connection();
    conn.query("CREATE CONSTRAINT shared FOR (n:L) REQUIRE n.p IS NOT NULL")
        .unwrap();
    assert_eq!(
        error_class_literal(
            &mut conn,
            "CREATE INDEX shared IF NOT EXISTS FOR (n:L) ON (n.p)"
        ),
        "SemanticError"
    );
    conn.query("CREATE CONSTRAINT backed FOR (n:M) REQUIRE n.p IS KEY")
        .unwrap();
    conn.query("CREATE INDEX backed IF NOT EXISTS FOR (n:M) ON (n.q)")
        .unwrap();
    assert_eq!(
        error_class_literal(&mut conn, "CREATE INDEX backed FOR (n:M) ON (n.q)"),
        "SemanticError"
    );
}

/// A graph-type constraint's whole-map `OPTIONS $param` must be a MAP; a
/// non-map value is the argument validator's failure, not a silent capture.
#[test]
fn graph_type_constraint_options_parameter_must_be_a_map() {
    let mut conn = connection();
    let error = conn
        .query_with_params(
            "CREATE GRAPH TYPE gt AS {
                CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY OPTIONS $opts
            }",
            params(&[("opts", Value::Int(7))]),
        )
        .unwrap_err();
    common::assert_argument_error(&error);
}

/// A LOOKUP index's `ON EACH labels(...)`/`type(...)` argument must be the
/// pattern's own variable. The rejection is the undefined-variable
/// condition and is tagged as such, so it surfaces `UndefinedVariable`
/// rather than the generic parse fallback (`EngineError::kind()` reads the
/// tag; the rendered public error carries it verbatim).
#[test]
fn lookup_index_foreign_variable_is_tagged_undefined_variable() {
    let mut conn = connection();
    let error = conn
        .query("CREATE LOOKUP INDEX i FOR (n) ON EACH labels(x)")
        .unwrap_err();
    common::assert_syntax_error(&error);
    assert!(error.to_string().contains("UndefinedVariable"), "{error}");
}

#[test]
fn graph_type_drop_shape_rules_are_syntax_errors() {
    let mut conn = connection();
    for statement in [
        // An element type the graph type does not contain.
        "ALTER CURRENT GRAPH TYPE DROP { (:Person => {name :: STRING}) }",
        "ALTER CURRENT GRAPH TYPE DROP { ()-[:RELATED => {since :: INTEGER}]->() }",
        // A constraint the graph type does not contain.
        "ALTER CURRENT GRAPH TYPE DROP { CONSTRAINT nope }",
        // Constraint definitions are not alterable.
        "ALTER CURRENT GRAPH TYPE ALTER { CONSTRAINT c FOR (n:P) REQUIRE n.x IS KEY }",
    ] {
        assert_eq!(
            error_class_literal(&mut conn, statement),
            "SyntaxError",
            "{statement}"
        );
    }
}

#[test]
fn graph_type_alter_missing_element_is_a_syntax_error() {
    let mut conn = connection();
    assert_eq!(
        error_class_literal(
            &mut conn,
            "ALTER CURRENT GRAPH TYPE ALTER { (:Person => {name :: STRING}) }"
        ),
        "SyntaxError"
    );
}
