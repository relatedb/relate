use super::*;
use relate_parser::parse;

fn body_of(query: &str) -> GraphTypeBody {
    let stmt = parse(query).expect("should parse");
    match stmt.clauses.into_iter().next().unwrap() {
        relate_parser::Clause::CreateGraphType(cgt) => cgt.body,
        relate_parser::Clause::AlterCurrentGraphType(agt) => match agt.target {
            relate_parser::AlterCurrentGraphTypeTarget::Body(body) => body,
            relate_parser::AlterCurrentGraphTypeTarget::Named(name) => {
                panic!("expected an inline body, got named type {name}")
            }
        },
        other => panic!("expected a graph-type clause, got {other:?}"),
    }
}

#[test]
fn validate_accepts_the_cip178_explanatory_example() {
    let body = body_of(
            "CREATE GRAPH TYPE gt {
                (s:Student => :Person { name :: STRING NOT NULL, birthday :: DATE, studId :: INTEGER IS KEY })
                REQUIRE (s.name, s.birthday) IS UNIQUE OPTIONS {indexProvider: 'range-1.0'},
                (:City => :Location { name :: STRING IS KEY }),
                (:Site => :Location { name :: STRING }),
                (s)-[:LIVES_IN =>]->(:City =>),
                (s)-[:VISITED =>]->(:Location),
                CONSTRAINT FOR ()-[x:LegacyRel]->() REQUIRE x.foo IS UNIQUE,
                CONSTRAINT mySiteConstraint FOR (st:Site =>) REQUIRE st.name IS KEY,
                CONSTRAINT FOR (p:Person) REQUIRE p.age :: INTEGER
            }",
        );
    assert!(validate_graph_type_body(
        &body,
        relate_core::PreviewFeatures::none(),
        GraphTypeScope::Declaration,
        false
    )
    .is_ok());
}

#[test]
fn validate_rejects_duplicate_identifying_label() {
    let body = body_of(
        "CREATE GRAPH TYPE gt {
                (p:Person => {name :: STRING}),
                (q:Person => {age :: INTEGER})
            }",
    );
    assert!(validate_graph_type_body(
        &body,
        relate_core::PreviewFeatures::none(),
        GraphTypeScope::Declaration,
        false
    )
    .is_err());
}

#[test]
fn validate_rejects_ineffectual_edge_type() {
    let body = body_of("CREATE GRAPH TYPE gt { ()-[l:RELATED =>]->() }");
    assert!(validate_graph_type_body(
        &body,
        relate_core::PreviewFeatures::none(),
        GraphTypeScope::Declaration,
        false
    )
    .is_err());
}

#[test]
fn via_as_exempts_only_the_canonicalizer_input_rules() {
    // The `AS` spelling still gets every other body rule: duplicate
    // identifying labels are rejected even with the exemption on.
    let body = body_of(
        "CREATE GRAPH TYPE gt AS {
                (p:Person => {name :: STRING}),
                (q:Person => {age :: INTEGER})
            }",
    );
    assert!(validate_graph_type_body(
        &body,
        relate_core::PreviewFeatures::none(),
        GraphTypeScope::Declaration,
        true
    )
    .is_err());
    // ... while the two exempted CIP-180 shapes pass only with it.
    for body in [
        "CREATE GRAPH TYPE gt AS { ()-[:REL =>]->() }",
        "CREATE GRAPH TYPE gt AS { CONSTRAINT FOR (p:Foo) REQUIRE p.a IS UNIQUE, \
         CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY }",
    ] {
        let body = body_of(body);
        assert!(validate_graph_type_body(
            &body,
            relate_core::PreviewFeatures::none(),
            GraphTypeScope::Declaration,
            true
        )
        .is_ok());
        assert!(validate_graph_type_body(
            &body,
            relate_core::PreviewFeatures::none(),
            GraphTypeScope::Declaration,
            false
        )
        .is_err());
    }
}

#[test]
fn validate_rejects_any_union_member_without_the_optional_property_gate() {
    // CIP-283's storable check must run at every recursion level: a bare
    // `ANY` nested in a union is not a storable property type.
    let body = body_of("ALTER CURRENT GRAPH TYPE SET { (p:Person => {name :: STRING | ANY}) }");
    let err = validate_graph_type_body(
        &body,
        relate_core::PreviewFeatures::none(),
        GraphTypeScope::Alteration,
        false,
    )
    .expect_err("a union member ANY must be rejected without the preview gate");
    assert!(err.to_string().contains("non-storable value type"), "{err}");
}

#[test]
fn validate_accepts_ineffectual_looking_edge_with_a_labelled_source() {
    let body = body_of(
        "CREATE GRAPH TYPE gt {
                (p:Person => {name :: STRING}),
                (p)-[l:RELATED =>]->()
            }",
    );
    assert!(validate_graph_type_body(
        &body,
        relate_core::PreviewFeatures::none(),
        GraphTypeScope::Declaration,
        false
    )
    .is_ok());
}

#[test]
fn a_drop_endpoint_naming_an_undeclared_alias_is_not_the_wildcard() {
    // `(nope)` parses as an alias reference; with no declared alias it must
    // not match the stored `()` endpoint (it used to collapse to the same
    // `None` the any type produces).
    let spec_body = body_of("ALTER CURRENT GRAPH TYPE DROP { ()-[:REL =>]->(nope) }");
    let stored_body = body_of("ALTER CURRENT GRAPH TYPE DROP { ()-[:REL =>]->() }");
    let (node_aliases, _edge_aliases) = alias_tables(&spec_body);
    assert!(!edge_type_matches_exactly(
        &spec_body.edge_types[0],
        &stored_body.edge_types[0],
        &node_aliases
    ));
    // The genuine any-to-any shape still matches.
    assert!(edge_type_matches_exactly(
        &stored_body.edge_types[0],
        &stored_body.edge_types[0],
        &node_aliases
    ));
}

/// A DROP naming an unnamed bare-alias rule by its generated hash must remove
/// it even when the same command drops the aliased element type: the hash
/// resolution has to consult the *pre-drop* alias tables, because the alias
/// is gone once the element type has been retained away.
#[test]
fn drop_by_generated_name_resolves_the_pre_drop_alias() {
    let current = body_of(
        "CREATE GRAPH TYPE gt AS {
                (n:Label => {prop :: STRING}),
                CONSTRAINT FOR (n) REQUIRE n.prop IS KEY
            }",
    );
    let generated = super::canonicalize::auto_constraint_name(&current.constraints[0], "Label");
    let drop_body = body_of(&format!(
        "ALTER CURRENT GRAPH TYPE DROP {{ (:Label =>), CONSTRAINT {generated} }}"
    ));
    let after = apply_drop(current, &drop_body).expect("drop applies");
    assert!(
        after.constraints.is_empty(),
        "the generated-name drop must remove the rule: {:?}",
        after.constraints
    );
}

#[test]
fn derive_constraints_produces_property_type_and_endpoint_constraints() {
    let body = body_of(
        "ALTER CURRENT GRAPH TYPE SET {
                (p :Person => {name :: STRING, age :: INTEGER}),
                (c :City => {name :: STRING}),
                (p)-[:LIVES_IN => {since :: DATE}]->(c)
            }",
    );
    let derived = derive_constraints(&body).expect("validated body derives");
    assert_eq!(derived.len(), 6);
    assert!(derived.iter().any(
        |c| c.constraint_type == "sourceLabel" && c.enforced_label.as_deref() == Some("Person")
    ));
    assert!(
        derived
            .iter()
            .any(|c| c.constraint_type == "targetLabel"
                && c.enforced_label.as_deref() == Some("City"))
    );
    assert!(derived.iter().any(|c| c.constraint_type == "propertyType"
        && c.label == "LIVES_IN"
        && c.properties == vec!["since".to_string()]));
}

/// A `NOT NULL` property derives TWO constraints — the property type
/// (without the outer `NOT NULL` in its `propertyType` text) and the
/// separate existence constraint Neo4j reports — and every derived name
/// is Neo4j's generated `constraint_<hash>`.
#[test]
fn derive_constraints_splits_not_null_and_generates_neo4j_names() {
    let body = body_of(
        "ALTER CURRENT GRAPH TYPE SET {
                (:Label1 => :Label2 {prop :: STRING NOT NULL})
            }",
    );
    let derived = derive_constraints(&body).expect("validated body derives");
    assert_eq!(derived.len(), 3, "{derived:?}");
    let type_constraint = derived
        .iter()
        .find(|c| c.constraint_type == "propertyType")
        .expect("a property type constraint");
    assert_eq!(type_constraint.property_type.as_deref(), Some("STRING"));
    assert_eq!(type_constraint.name, "constraint_f7912a5d");
    let exists = derived
        .iter()
        .find(|c| c.constraint_type == "exists")
        .expect("a property existence constraint");
    assert_eq!(exists.name, "constraint_b75b74fc");
    let label = derived
        .iter()
        .find(|c| c.constraint_type == "labelExistence")
        .expect("the implied-label constraint");
    assert_eq!(label.name, "constraint_69e8c8ea");
}

/// A child property that further restricts its parent's
/// declared type derives its own property-type constraint, carrying the bare
/// `BOOLEAN` (the outer `NOT NULL` is the separate existence constraint).
/// Identified by label + property, not by the generated name, so the
/// scenario's observable cannot drift with the naming scheme.
#[test]
fn derive_constraints_child_restriction_derives_its_own_bare_type() {
    let body = body_of(
        "ALTER CURRENT GRAPH TYPE SET {
                (:A => {p1 :: STRING, p2 :: INTEGER | FLOAT, p3 :: ANY NOT NULL}),
                (:D => :A {p1 :: STRING NOT NULL, p2 :: INTEGER, p3 :: BOOLEAN NOT NULL})
            }",
    );
    let derived = derive_constraints(&body).expect("validated body derives");
    let restricted = derived
        .iter()
        .filter(|c| c.constraint_type == "propertyType" && c.label == "D")
        .filter(|c| c.properties == vec!["p3".to_string()])
        .collect::<Vec<_>>();
    assert_eq!(restricted.len(), 1, "{derived:?}");
    assert_eq!(restricted[0].property_type.as_deref(), Some("BOOLEAN"));
    assert!(
        derived.iter().any(|c| c.constraint_type == "exists"
            && c.label == "D"
            && c.properties == vec!["p3".to_string()]),
        "the NOT NULL tail derives a separate existence constraint: {derived:?}"
    );
}

#[test]
fn canonicalize_empty_body_is_empty_string() {
    let body = GraphTypeBody::default();
    assert_eq!(
        canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
        ""
    );
}

#[test]
fn canonicalize_simple_node_type() {
    let body = body_of("CREATE GRAPH TYPE gt AS { (:City => :Location { name :: STRING }) }");
    assert_eq!(
        canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
        "(:`City` => :`Location` {`name` :: STRING})"
    );
}

#[test]
fn canonicalize_int_and_bool_normalize() {
    let body = body_of("CREATE GRAPH TYPE gt AS { (:A => :Zebra&:Mango { p :: BOOL }) }");
    assert_eq!(
        canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
        "(:`A` => :`Mango`&`Zebra` {`p` :: BOOLEAN})"
    );
}

#[test]
fn canonicalize_orders_node_types_by_identifying_label() {
    let body = body_of(
        "CREATE GRAPH TYPE gt AS {
                (:Student => :Person { name :: STRING }),
                (:City => :Location { name :: STRING })
            }",
    );
    assert_eq!(
        canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
        "(:`City` => :`Location` {`name` :: STRING}), (:`Student` => :`Person` {`name` :: STRING})"
    );
}

#[test]
fn canonicalize_unnamed_unidentified_key_constraint() {
    let body = body_of("CREATE GRAPH TYPE gt AS { CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY }");
    assert_eq!(
        canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
        "CONSTRAINT FOR (`n`:`Foo`) REQUIRE (`n`.`a`) IS KEY"
    );
}

#[test]
fn canonicalize_identified_key_constraint_from_inline_rewrite() {
    let body =
        body_of("CREATE GRAPH TYPE gt AS { (:City => :Location { name :: STRING IS KEY }) }");
    assert_eq!(
            canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
            "(:`City` => :`Location` {`name` :: STRING}), CONSTRAINT FOR (`n`:`City` =>) REQUIRE (`n`.`name`) IS KEY"
        );
}

#[test]
fn canonicalize_empty_options_map_is_omitted() {
    let body =
        body_of("CREATE GRAPH TYPE gt AS { CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY OPTIONS {} }");
    assert_eq!(
        canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
        "CONSTRAINT FOR (`n`:`Foo`) REQUIRE (`n`.`a`) IS KEY"
    );
}

#[test]
fn canonicalize_rank_orders_key_unique_notnull_type() {
    let body = body_of(
        "CREATE GRAPH TYPE gt AS {
                CONSTRAINT FOR (p:Foo) REQUIRE p.a :: INT,
                CONSTRAINT FOR (p:Foo) REQUIRE p.a IS NOT NULL,
                CONSTRAINT FOR (p:Foo) REQUIRE p.a IS UNIQUE,
                CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY
            }",
    );
    assert_eq!(
            canonicalize_graph_type_body(&body).expect("a validated body canonicalizes"),
            "CONSTRAINT FOR (`n`:`Foo`) REQUIRE (`n`.`a`) IS KEY, CONSTRAINT FOR (`n`:`Foo`) REQUIRE (`n`.`a`) IS UNIQUE, CONSTRAINT FOR (`n`:`Foo`) REQUIRE (`n`.`a`) IS NOT NULL, CONSTRAINT FOR (`n`:`Foo`) REQUIRE (`n`.`a`) IS :: INTEGER"
        );
}

#[test]
fn graph_type_spec_roundtrips_through_the_parser_model() {
    let body = body_of(
            "CREATE GRAPH TYPE gt {
                (s:Student => :Person { name :: STRING NOT NULL, birthday :: DATE, studId :: INTEGER IS KEY })
                REQUIRE (s.name, s.birthday) IS UNIQUE OPTIONS {indexProvider: 'range-1.0'},
                (:City => :Location { name :: STRING IS KEY }),
                (:Site => :Location { name :: STRING }),
                (s)-[:LIVES_IN => {since :: DATE}]->(:City =>),
                (s)-[:VISITED =>]->(:Location),
                ()-[:Legacy]->(),
                CONSTRAINT FOR ()-[x:LegacyRel]->() REQUIRE x.foo IS UNIQUE,
                CONSTRAINT mySiteConstraint FOR (st:Site =>) REQUIRE st.name IS KEY,
                CONSTRAINT FOR (p:Person) REQUIRE p.age :: INTEGER,
                CONSTRAINT FOR (p:Person) REQUIRE p.nick IS NOT NULL OPTIONS {a: 1, b: 2.5, c: true, d: null}
            }",
        );
    let spec = GraphTypeSpec::from_body(&body).expect("shape-valid body");
    assert_eq!(spec.to_body(), body);
    let json = serde_json::to_string(&spec).expect("spec serializes");
    let decoded: GraphTypeSpec = serde_json::from_str(&json).expect("spec deserializes");
    assert_eq!(decoded, spec);
    assert_eq!(decoded.to_body(), body);
}

#[test]
fn graph_type_spec_roundtrips_alter_drop_bodies() {
    let body = body_of(
        "ALTER CURRENT GRAPH TYPE DROP {
                (:City), ()-[:LIVES_IN]->(), CONSTRAINT mySiteConstraint
            }",
    );
    let spec = GraphTypeSpec::from_body(&body).expect("shape-valid body");
    assert_eq!(spec.to_body(), body);
    let json = serde_json::to_string(&spec).expect("spec serializes");
    let decoded: GraphTypeSpec = serde_json::from_str(&json).expect("spec deserializes");
    assert_eq!(decoded.to_body(), body);
}

#[test]
fn graph_type_spec_keeps_nonliteral_options_rendering() {
    let body = body_of(
        "CREATE GRAPH TYPE gt AS {
                CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY OPTIONS {x: 1 + 1}
            }",
    );
    let spec = GraphTypeSpec::from_body(&body).expect("shape-valid body");
    // The ASTs differ (a non-literal expression becomes the NonLiteral
    // marker), but the canonical rendering is byte-identical.
    assert_eq!(
        canonicalize_graph_type_body(&spec.to_body()).expect("a validated body canonicalizes"),
        canonicalize_graph_type_body(&body).expect("a validated body canonicalizes")
    );
}

/// The persisted model carries literal options only: an unresolved
/// `OPTIONS $param` fails the capture rather than being dropped.
#[test]
fn graph_type_spec_capture_rejects_an_unresolved_options_parameter() {
    let body = body_of(
        "CREATE GRAPH TYPE gt AS {
                CONSTRAINT FOR (p:Foo) REQUIRE p.a IS KEY OPTIONS $opts
            }",
    );
    let error =
        GraphTypeSpec::from_body(&body).expect_err("an unresolved parameter must not be captured");
    assert!(
        error.to_string().contains("unresolved OPTIONS $opts"),
        "{error}"
    );
}
