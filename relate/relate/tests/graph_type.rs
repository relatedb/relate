//! ADR-0211 (LABS-265): graph-type elements with GQL property declarations
//! (`(:Tag => {name IS STRING})`), `SHOW CURRENT GRAPH TYPE AS GRAPH`
//! (CIP-257), and `ALTER CURRENT GRAPH TYPE SET GRAPH TYPE <name>` -- the
//! supported path from a `CREATE GRAPH TYPE`-declared named type to the
//! database's current graph type.

mod common;

use relate::Value;

use common::{connection, err_text, list, map, params, rows, run, scalar_one, string};

/// Property-declared node and edge type elements: the `=>` marker after
/// the identifying label is mandatory before a property block (current
/// Neo4j behavior; CIP-178/181/183), and `IS <value type>` declares the
/// property type.
const SO_GRAPH: &str = "CREATE GRAPH TYPE SOGraph AS {
    (t:Tag => {name IS STRING}),
    (q:Question => {title IS STRING}),
    (q)-[:TAGGED]->(t)
}";

#[test]
fn property_declared_elements_parse_and_show_canonically() {
    let mut c = connection();
    run(&mut c, SO_GRAPH);
    let spec = scalar_one(&mut c, "SHOW GRAPH TYPE SOGraph YIELD body RETURN body");
    assert_eq!(
        spec,
        Value::String(
            "(:`Question` => {`title` :: STRING}), (:`Tag` => {`name` :: STRING}), \
             (:`Question` =>)-[:`TAGGED` =>]->(:`Tag` =>)"
                .into()
        )
    );
}

#[test]
fn property_block_without_implies_is_a_syntax_error() {
    // The LABS-265 ticket's original spelling — `(:Tag {name IS STRING})`
    // without the `=>` marker — is invalid in current Neo4j too: the
    // identifying label must be explicitly denoted (CIP-178/181/183).
    let mut c = connection();
    let err = err_text(c.query("CREATE GRAPH TYPE bad AS { (:Tag {name IS STRING}) }"));
    assert!(err.contains("identifying label"), "{err}");
    let err = err_text(c.query(
        "CREATE GRAPH TYPE bad AS { (t:Tag), (q:Question), (q)-[:TAGGED {w IS FLOAT}]->(t) }",
    ));
    assert!(err.contains("identifying label"), "{err}");
}

#[test]
fn create_graph_type_does_not_make_the_type_current() {
    let mut c = connection();
    run(&mut c, SO_GRAPH);
    // Declared and current stay separate (ADR-0211): the current graph
    // type is still empty until promoted.
    let spec = scalar_one(&mut c, "SHOW CURRENT GRAPH TYPE");
    assert_eq!(spec, string("{}"));
}

#[test]
fn alter_current_graph_type_set_graph_type_promotes_a_declared_type() {
    let mut c = connection();
    run(&mut c, SO_GRAPH);
    run(&mut c, "ALTER CURRENT GRAPH TYPE SET GRAPH TYPE SOGraph");
    let spec = scalar_one(&mut c, "SHOW CURRENT GRAPH TYPE");
    let Value::String(spec) = spec else {
        panic!("expected a specification string, got {spec:?}");
    };
    assert!(spec.starts_with('{') && spec.ends_with('}'), "{spec}");
    assert!(
        spec.contains(":`Question` => {`title` :: STRING}"),
        "{spec}"
    );
    assert!(spec.contains(":`Tag` => {`name` :: STRING}"), "{spec}");
    assert!(spec.contains("-[:`TAGGED` =>]->"), "{spec}");

    // The promoted type's property types become derived constraints
    // (the TAGGED edge type also derives endpoint label constraints).
    let constraints = rows(
        &mut c,
        "SHOW CONSTRAINTS YIELD type, labelsOrTypes, properties \
         WHERE type = 'NODE_PROPERTY_TYPE' \
         RETURN type, labelsOrTypes, properties ORDER BY labelsOrTypes",
    );
    assert_eq!(
        constraints,
        vec![
            vec![
                string("NODE_PROPERTY_TYPE"),
                list(vec![string("Question")]),
                list(vec![string("title")]),
            ],
            vec![
                string("NODE_PROPERTY_TYPE"),
                list(vec![string("Tag")]),
                list(vec![string("name")]),
            ],
        ]
    );
}

#[test]
fn set_graph_type_unknown_name_fails() {
    let mut c = connection();
    let err = err_text(c.query("ALTER CURRENT GRAPH TYPE SET GRAPH TYPE nope"));
    assert!(err.contains("GraphTypeNotFound"), "{err}");
}

#[test]
fn show_current_graph_type_as_graph_returns_elements() {
    let mut c = connection();
    run(&mut c, SO_GRAPH);
    run(&mut c, "ALTER CURRENT GRAPH TYPE SET GRAPH TYPE SOGraph");
    let result = run(&mut c, "SHOW CURRENT GRAPH TYPE AS GRAPH");
    assert_eq!(result.header.columns, vec!["nodes", "relationships"]);
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    let Value::List(nodes) = &row[0] else {
        panic!("expected a nodes list, got {:?}", row[0]);
    };
    let Value::List(rels) = &row[1] else {
        panic!("expected a relationships list, got {:?}", row[1]);
    };
    // One synthetic node per declared node type (Question, Tag), each
    // labelled NodeElementType with its declared property list.
    assert_eq!(nodes.len(), 2);
    let mut labels = nodes
        .iter()
        .map(|v| {
            let Value::Node(n) = v else {
                panic!("expected a node, got {v:?}");
            };
            assert_eq!(
                n.labels.as_slice(),
                ["NodeElementType".to_string()].as_slice()
            );
            let Some(Value::String(label)) = n.properties.get("label") else {
                panic!("node misses its label property: {n:?}");
            };
            label.to_string()
        })
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, vec!["Question", "Tag"]);
    // One synthetic relationship for the declared TAGGED edge type,
    // connecting the Question and Tag virtual nodes.
    assert_eq!(rels.len(), 1);
    let Value::Relationship(r) = &rels[0] else {
        panic!("expected a relationship, got {:?}", rels[0]);
    };
    assert_eq!(r.rel_type, "RELATIONSHIP_ELEMENT_TYPE");
    assert_eq!(
        r.properties.get("relationshipType"),
        Some(&string("TAGGED"))
    );
    assert_eq!(r.properties.get("properties"), Some(&list(vec![])));
}

#[test]
fn show_current_graph_type_as_graph_of_an_empty_type_is_empty() {
    let mut c = connection();
    let result = run(&mut c, "SHOW CURRENT GRAPH TYPE AS GRAPH");
    assert_eq!(result.header.columns, vec!["nodes", "relationships"]);
    assert_eq!(result.rows, vec![vec![list(vec![]), list(vec![])]]);
}

/// The visualization's display form uses the parser's **shape-only** quoting
/// policy: `when` is a keyword spelling but a plain identifier shape, so it
/// stays bare (CIP-257's `composability.feature` expects
/// `properties: ['when :: STRING']`), while a hyphenated name renders
/// backticked (CIP-257's `nodes.feature` expects `` `prop-3` ``). The
/// canonical graph-type text keeps the always-quoted canonical spelling, so a
/// keyword property name is still `` `when` `` where that text is re-parsed.
#[test]
fn show_current_graph_type_as_graph_quotes_by_shape_not_keyword() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE GRAPH TYPE KeywordProps AS { (t:Tag => {`when` IS STRING, `prop-3` IS STRING}) }",
    );
    let canonical = scalar_one(
        &mut c,
        "SHOW GRAPH TYPE KeywordProps YIELD body RETURN body",
    );
    let Value::String(canonical) = canonical else {
        panic!("expected a canonical body string, got {canonical:?}");
    };
    assert!(
        canonical.contains("`when` :: STRING"),
        "canonical text keeps a keyword property quoted: {canonical}"
    );
    assert!(
        canonical.contains("`prop-3` :: STRING"),
        "canonical text keeps a shaped name quoted: {canonical}"
    );

    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET GRAPH TYPE KeywordProps",
    );
    let result = run(&mut c, "SHOW CURRENT GRAPH TYPE AS GRAPH");
    let Value::List(nodes) = &result.rows[0][0] else {
        panic!("expected a nodes list, got {:?}", result.rows[0][0]);
    };
    let Value::Node(node) = &nodes[0] else {
        panic!("expected a node, got {:?}", nodes[0]);
    };
    assert_eq!(
        node.properties.get("properties"),
        Some(&list(vec![
            string("when :: STRING"),
            string("`prop-3` :: STRING"),
        ])),
        "display quotes by shape only: `when` bare, `prop-3` backticked"
    );
}

// ---------------------------------------------------------------------
// ADR-0371: the CIP graph-type trio's preview gates, ungated (this
// process never sets RELATE_PREVIEW; the gated behavior lives in
// `tests/graph_type_trio.rs`, its own gated process)
// ---------------------------------------------------------------------

/// The authored fail-closed gate error for one disabled preview feature.
fn gated_off_error(feature: &str) -> String {
    format!(
        "SemanticError: preview feature '{feature}' is not enabled: \
         set RELATE_PREVIEW={feature}"
    )
}

#[test]
fn the_graph_type_trio_fails_closed_ungated() {
    let mut c = connection();
    // CIP-283: a bare `ANY` element-type property (the optional property).
    let err = err_text(c.query("ALTER CURRENT GRAPH TYPE SET { (:Label => {prop :: ANY}) }"));
    assert!(
        err.contains(&gated_off_error("graph-type-opt-props")),
        "{err}"
    );
    // A `DEFAULT ON COMMIT` clause.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET { (:Label => {id :: UUID NOT NULL DEFAULT ON COMMIT uuid()}) }",
    ));
    assert!(
        err.contains(&gated_off_error("graph-type-default-values")),
        "{err}"
    );
    // CIP-266: an implied label naming another in-body node element
    // type's identifying label (inheritance).
    let err = err_text(c.query("ALTER CURRENT GRAPH TYPE SET { (:A => {}), (:B => :A) }"));
    assert!(
        err.contains(&gated_off_error("graph-type-implied-labels")),
        "{err}"
    );
    // The ADD half of the CIP-266 gate: an ADD whose identifying label a
    // stored node element type already implies -- the construct only
    // materializes against stored state, so the executor's graph-type
    // layer fails closed with the same authored error (ungated).
    let mut c = connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => :B {p1 :: STRING}) }",
    );
    let err = err_text(c.query("ALTER CURRENT GRAPH TYPE ADD { (:B => {p2 :: INTEGER}) }"));
    assert!(
        err.contains(&gated_off_error("graph-type-implied-labels")),
        "{err}"
    );
    // One feature at a time: the three flags are independently
    // enableable, and a construct of one gated feature stays rejected
    // when only another feature is enabled (this process enables
    // none, so every construct above fails closed with its own
    // feature's authored error).
    let mut c = connection();
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:A => {p :: ANY}),
            (:B => :A {p :: STRING})
        }",
    ));
    // The front gate reports the first construct it finds: the bare ANY.
    assert!(
        err.contains(&gated_off_error("graph-type-opt-props")),
        "{err}"
    );
}

// ---------------------------------------------------------------------
// Graph-type DDL fidelity: constraint OPTIONS, element-type property
// types, ALTER ADD/DROP algebra, and SHOW of the resulting schema.
// ---------------------------------------------------------------------

#[test]
fn graph_type_constraint_options_follow_the_create_constraint_policy() {
    let mut c = connection();
    // An index-backed constraint admits only an empty `indexConfig`.
    let err = err_text(
        c.query("ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n:A) REQUIRE n.p IS KEY OPTIONS {badger: 1} }"),
    );
    assert!(err.contains("for 'OPTIONS'"), "{err}");
    let err = err_text(
        c.query("ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n:A) REQUIRE n.p IS KEY OPTIONS {indexConfig: {x: 1}} }"),
    );
    assert!(err.contains("index config"), "{err}");
    // A non index-backed constraint admits no options at all.
    let err = err_text(
        c.query("ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n:A) REQUIRE n.p IS NOT NULL OPTIONS {indexConfig: {}} }"),
    );
    assert!(err.contains("not supported"), "{err}");
    // The whole-map parameter spelling is resolved and checked too.
    let err = c
        .query_with_params(
            "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n:A) REQUIRE n.p IS KEY OPTIONS $opts }",
            params(&[("opts", map(&[("badger", Value::Bool(true))]))]),
        )
        .expect_err("an unknown parameter-map key must be rejected");
    assert!(err.to_string().contains("for 'OPTIONS'"), "{err}");
    // An admitted empty config is accepted and the rule is stored.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n:A) REQUIRE n.p IS KEY OPTIONS {indexConfig: {}} }",
    );
    assert_eq!(
        scalar_one(&mut c, "SHOW CONSTRAINTS YIELD type RETURN type"),
        string("NODE_KEY")
    );
}

#[test]
fn element_type_property_types_are_validated() {
    let mut c = connection();
    for (statement, expected) in [
        (
            "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: NODE}) }",
            "Invalid property type",
        ),
        (
            "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: MAP}) }",
            "Invalid property type",
        ),
        (
            "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: VECTOR}) }",
            "Invalid property type",
        ),
        (
            "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: VECTOR(4)}) }",
            "Invalid property type",
        ),
        (
            "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: LIST<NODE NOT NULL>}) }",
            "not supported",
        ),
    ] {
        let err = err_text(c.query(statement));
        assert!(err.contains(expected), "{statement}: {err}");
    }
    // A closed union whose members all agree on NOT NULL is a valid
    // element-type property type.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: INT NOT NULL | FLOAT NOT NULL}) }",
    );
    let spec = scalar_one(&mut c, "SHOW CURRENT GRAPH TYPE");
    let Value::String(spec) = spec else {
        panic!("expected a specification string");
    };
    assert!(spec.contains("INTEGER NOT NULL | FLOAT NOT NULL"), "{spec}");
}

#[test]
fn multi_property_existence_and_property_type_constraints_are_rejected() {
    let mut c = connection();
    for statement in [
        "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n:A) REQUIRE (n.a, n.b) IS NOT NULL }",
        "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n:A) REQUIRE (n.a, n.b) IS :: INT }",
    ] {
        let err = err_text(c.query(statement));
        assert!(err.contains("does not allow multiple properties"), "{err}");
    }
}

#[test]
fn add_restates_a_rule_on_an_element_type_but_rejects_an_equivalent_copy() {
    let mut c = connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Person => {name :: STRING IS UNIQUE}) }",
    );
    // The plain reference to the element type re-states the inline rule: a
    // no-op, not a duplicate.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE ADD { CONSTRAINT FOR (n:Person) REQUIRE n.name IS UNIQUE }",
    );
    // A `=>`-qualified copy of the inline rule is an equivalent constraint.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE ADD { CONSTRAINT FOR (p:Person =>) REQUIRE p.name IS UNIQUE }",
    ));
    assert!(err.contains("duplicate constraint key"), "{err}");
    // So is an equivalent copy on a subject that is not an element type.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (s:Student) REQUIRE s.id IS NOT NULL }",
    );
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE ADD { CONSTRAINT FOR (s:Student) REQUIRE s.id IS NOT NULL }",
    ));
    assert!(err.contains("duplicate constraint key"), "{err}");
}

#[test]
fn add_constraint_reference_binds_within_the_statement_body() {
    let mut c = connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (p:Person => {name :: STRING}) }",
    );
    // `(p)` names a stored alias; an ADD alias is statement-local.
    let err = err_text(
        c.query("ALTER CURRENT GRAPH TYPE ADD { CONSTRAINT FOR (p) REQUIRE (p.name) IS KEY }"),
    );
    assert!(err.contains("referenced by 'p' not found"), "{err}");
}

#[test]
fn drop_rejects_constraint_definitions_and_keeps_undesignated_rules() {
    let mut c = connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (n:Label => {p :: STRING IS KEY}) }",
    );
    let err =
        err_text(c.query("ALTER CURRENT GRAPH TYPE DROP { (:Label => {p :: STRING IS KEY}) }"));
    assert!(
        err.contains("Constraint definitions are not supported"),
        "{err}"
    );
    // Dropping the element type keeps the undesignated key rule.
    run(&mut c, "ALTER CURRENT GRAPH TYPE DROP { (:Label =>) }");
    assert_eq!(
        scalar_one(&mut c, "SHOW CONSTRAINTS YIELD type RETURN type"),
        string("NODE_KEY")
    );
}

#[test]
fn drop_takes_an_independent_catalog_constraint_with_the_element_type() {
    let mut c = connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (p:Person => {name :: STRING}) }",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT myConstraint FOR (p:Person) REQUIRE p.name IS KEY",
    );
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE DROP { (:Person =>), CONSTRAINT myConstraint }",
    );
    assert_eq!(
        scalar_one(&mut c, "SHOW CONSTRAINTS YIELD name RETURN count(*)"),
        Value::Int(0)
    );
}

#[test]
fn setting_rejects_replacing_an_index_backed_rule_in_one_command() {
    let mut c = connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT name1 FOR (n:Label) REQUIRE n.prop IS KEY }",
    );
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT name2 FOR (n:Label) REQUIRE n.prop IS KEY }",
    ));
    assert!(err.contains("same transaction as dropping"), "{err}");
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT name1 FOR (n:Label) REQUIRE n.prop IS UNIQUE }",
    ));
    assert!(err.contains("same transaction as dropping"), "{err}");
}

#[test]
fn create_constraint_if_not_exists_is_a_noop_for_a_derived_rule() {
    let mut c = connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Label => {prop :: STRING}) }",
    );
    run(
        &mut c,
        "CREATE CONSTRAINT c IF NOT EXISTS FOR (e:Label) REQUIRE e.prop IS :: STRING",
    );
    assert_eq!(
        scalar_one(&mut c, "SHOW CONSTRAINTS YIELD type RETURN type"),
        string("NODE_PROPERTY_TYPE")
    );
}

#[test]
fn constraint_property_expressions_must_name_the_for_variable() {
    let mut c = connection();
    let err = err_text(
        c.query("CREATE CONSTRAINT my_constraint FOR (n:Label) REQUIRE (r.prop) IS NOT NULL"),
    );
    assert!(err.contains("Variable `r` not defined"), "{err}");
    let err = err_text(c.query(
        "CREATE CONSTRAINT my_constraint FOR ()-[r:relType]-() REQUIRE (r.prop, r.prop) IS REL KEY",
    ));
    assert!(err.contains("duplicated tokens"), "{err}");
}

#[test]
fn show_yield_keeps_source_names_in_scope_and_rejects_unknown_ones() {
    let mut c = connection();
    run(
        &mut c,
        "CREATE CONSTRAINT constraint1 FOR (n:Label) REQUIRE (n.prop) IS UNIQUE",
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "SHOW CONSTRAINTS YIELD name AS alias ORDER BY name WHERE name = 'constraint1' RETURN alias",
        ),
        string("constraint1")
    );
    let err = err_text(c.query("SHOW CONSTRAINTS YIELD * ORDER BY nope"));
    assert!(err.contains("not defined"), "{err}");
    let err = err_text(c.query("SHOW CONSTRAINTS RETURN *"));
    assert!(err.contains("MissingYield"), "{err}");
    let err = err_text(c.query("USE system SHOW CONSTRAINTS YIELD name RETURN name"));
    assert!(err.contains("NotAllowedOnSystemDatabase"), "{err}");
}
