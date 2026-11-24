//! Per-clause validator fixtures: node/labels/property/relationship,
//! `RelationshipTarget`, id-keyed constraints, the typed extension
//! envelopes and the value model (ADR-0364 §2, on the 1.1.0 shapes of
//! ADR-0487 §2).

use relate_ontology::{
    parse_ontology_document, to_graph_type_body, ConstraintType, OntologyError, PINNED_SCHEMA_URI,
};

use crate::support::{at, json_doc, neo4j_type_tokens};

// ---------------------------------------------------------------------------
// Node entry and labels clauses
// ---------------------------------------------------------------------------

#[test]
fn node_entry_clause_fixtures() {
    // `label` shorthand, the `labels` object, or both when they agree.
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\
             \"shorthand\":{\"label\":\"Person\"},\
             \"object\":{\"labels\":{\"identifier\":\"Company\",\"implied\":[\"Org\"],\
                                \"optional\":[\"Vendor\"],\"extensions\":{\"custom\":{}}}},\
             \"both\":{\"label\":\"Same\",\"labels\":{\"identifier\":\"Same\"}},\
             \"display\":{\"label\":\"Display\",\
                          \"description\":\"d\",\"reference\":\"https://example.com/n\",\
                          \"aliases\":[\"Alias\"]}}",
        )
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(doc.nodes()[0].identifying_label(), Some("Person"));
    assert_eq!(doc.nodes()[1].identifying_label(), Some("Company"));
    assert_eq!(doc.nodes()[1].labels.implied, ["Org"]);
    assert_eq!(doc.nodes()[2].identifying_label(), Some("Same"));
    assert_eq!(doc.nodes()[3].aliases, ["Alias"]);

    // A conflicting shorthand/identifier pair is an ambiguous identity.
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"label\":\"A\",\"labels\":{\"identifier\":\"B\"}}}")
            .as_bytes(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("must be equal"), "{err}");

    // Node entries are closed and keyed by id; 1.1.0 carries no node
    // `name` member (ADR-0487 §2).
    for (rest, path) in [
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"bogus\":1}}",
            "/nodes/N/bogus",
        ),
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"name\":\"N\"}}",
            "/nodes/N/name",
        ),
    ] {
        let err = parse_ontology_document(json_doc(rest).as_bytes()).unwrap_err();
        assert!(err.to_string().contains(path), "{path}: {err}");
        assert!(err.to_string().contains("unknown field"), "{path}: {err}");
    }
}

#[test]
fn labels_clause_fixtures() {
    // Both the identifier and the arrays are optional; `identifier` is
    // nullable.
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\
             \"null-id\":{\"labels\":{\"identifier\":null}},\
             \"empty\":{\"labels\":{}},\
             \"arrays\":{\"labels\":{\"identifier\":\"I\",\"implied\":[],\"optional\":[]}}}",
        )
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(doc.nodes()[0].identifying_label(), None);
    assert_eq!(doc.nodes()[1].identifying_label(), None);
    assert!(doc.nodes()[2].labels.implied.is_empty());

    // `labels` is closed and its arrays are string-only.
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"labels\":{\"bogus\":1}}}").as_bytes(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("/nodes/N/labels/bogus"), "{err}");
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"labels\":{\"implied\":[\"A\",1]}}}").as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("/nodes/N/labels/implied/1"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// Property clause and the closed Neo4jType vocabulary
// ---------------------------------------------------------------------------

#[test]
fn property_clause_fixtures() {
    // Every optional member in the 1.1.0 shape, plus the nullable forms
    // and an empty property name (accepted since
    // `propertyNames.minLength` is gone).
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\
             \"p\":{\"type\":\"STRING\",\"dimension\":null,\"must_exist\":true,\
                  \"unique\":null,\"key\":null,\"one_of\":[\"a\",\"b\"],\
                  \"pattern\":\"^a$\",\"name\":null,\"aliases\":[\"P\",\"p1\"],\
                  \"reference\":\"r\",\
                  \"description\":\"d\",\"extensions\":{\"custom\":{\"x\":1}}},\
             \"\":{\"type\":\"ANY\"}}}}",
        )
        .as_bytes(),
    )
    .unwrap();
    let p = &doc.nodes()[0].properties[0];
    assert_eq!(p.must_exist, Some(true));
    assert_eq!(p.dimension, None);
    assert_eq!(p.unique, None);
    assert_eq!(p.name, None);
    assert_eq!(p.aliases, ["P", "p1"]);
    assert_eq!(
        p.one_of.as_deref(),
        Some(&["a".to_owned(), "b".to_owned()][..])
    );
    assert_eq!(doc.nodes()[0].properties[1].id, "");
    assert_eq!(doc.nodes()[0].properties[1].one_of, None);

    // The pinned draft's `mustExist` is an unknown member; property
    // `aliases` is back in 1.1.0 and accepted (ADR-0487 §2).
    let err = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"p\":{\"mustExist\":true}}}}",
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");

    // `one_of` items are strings only.
    let err = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"p\":{\"one_of\":[\"a\",1]}}}}",
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("/nodes/N/properties/p/one_of/1"),
        "{err}"
    );
    assert!(err.to_string().contains("strings"), "{err}");

    // A non-array `one_of` is a type error on the field, not an item error.
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"p\":{\"one_of\":\"a\"}}}}")
            .as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("/nodes/N/properties/p/one_of"),
        "{err}"
    );
    assert!(
        err.to_string()
            .contains("'one_of' must be an array, found a string"),
        "{err}"
    );

    // Flags are nullable booleans; a non-boolean or a string dimension is a
    // type error on the field.
    let err = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"p\":{\"must_exist\":\"yes\"}}}}",
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("/nodes/N/properties/p/must_exist"),
        "{err}"
    );
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"p\":{\"dimension\":3.5}}}}")
            .as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("/nodes/N/properties/p/dimension"),
        "{err}"
    );
}

#[test]
fn neo4j_type_vocabulary_accepts_every_schema_token() {
    // Walk the vendored schema's `$defs.Neo4jType.enum`, the canonical
    // source the crate's closed table mirrors. The length pin catches a
    // schema bump that changes the vocabulary.
    let tokens = neo4j_type_tokens();
    assert_eq!(tokens.len(), 39);
    for token in &tokens {
        let doc = parse_ontology_document(
            json_doc(&format!(
                ",\"nodes\":{{\"N\":{{\"label\":\"N\",\"properties\":{{\"p\":{{\"type\":\"{token}\"}}}}}}}}"
            ))
            .as_bytes(),
        )
        .unwrap_or_else(|e| panic!("{token}: {e}"));
        let ty = doc.nodes()[0].properties[0]
            .property_type
            .expect("type parsed");
        assert_eq!(ty.as_str(), token.as_str());
    }
}

#[test]
fn property_type_tokens_outside_the_closed_enum_fail() {
    // The draft's bare spellings and `BYTES` are gone; lower-case,
    // unions, nested lists and unknown tokens are rejected too.
    for token in [
        "TIME",
        "DATETIME",
        "LOCALTIME",
        "LOCALDATETIME",
        "BYTES",
        "string",
        "STRINGY",
        "INTEGER | FLOAT",
        "LIST<LIST<STRING>>",
        "VECTOR<TIME>",
        "LIST<>",
        "",
    ] {
        let err = parse_ontology_document(
            json_doc(&format!(
                ",\"nodes\":{{\"N\":{{\"label\":\"N\",\"properties\":{{\"p\":{{\"type\":\"{token}\"}}}}}}}}"
            ))
            .as_bytes(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("/nodes/N/properties/p/type"),
            "{token}: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// Relationship entry and RelationshipTarget clauses
// ---------------------------------------------------------------------------

#[test]
fn relationship_clause_fixtures() {
    // `from` and `to` are required at validation; `type` is optional
    // (ADR-0667): a type-less entry parses and fails at the converter,
    // naming the entry.
    for (missing, body) in [
        ("from", "\"type\":\"R\",\"to\":{\"node\":\"N\"}"),
        ("to", "\"type\":\"R\",\"from\":{\"node\":\"N\"}"),
    ] {
        let doc = json_doc(&format!(
            ",\"nodes\":{{\"N\":{{\"label\":\"N\"}}}},\"relationships\":{{\"R\":{{{body}}}}}"
        ));
        let err = parse_ontology_document(doc.as_bytes()).unwrap_err();
        assert!(
            err.to_string().contains(missing),
            "missing {missing}: {err}"
        );
        assert!(err.to_string().contains("/relationships/R"), "{err}");
    }
    let doc = json_doc(
        ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":\
         {\"R\":{\"from\":{\"node\":\"N\"},\"to\":{\"node\":\"N\"}}}",
    );
    let doc = parse_ontology_document(doc.as_bytes())
        .expect("a type-less relationship entry is spec-valid");
    assert_eq!(doc.relationships()[0].rel_type, None);
    let err = to_graph_type_body(&doc).expect_err("the converter is the type gate");
    assert!(
        err.to_string()
            .contains("Ontology relationship 'R' has no type."),
        "{err}"
    );

    // Aliases, reference and description are carried; 1.1.0 carries no
    // relationship `name` member (ADR-0487 §2).
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\"R\":{\
             \"type\":\"R\",\"from\":{\"node\":\"N\"},\"to\":{\"node\":\"N\"},\
             \"aliases\":[\"A\"],\"reference\":\"r\",\"description\":\"d\"}}",
        )
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(doc.relationships()[0].aliases, ["A"]);

    // Relationship entries are closed; the 1.0.0 `name` spelling is
    // rejected as unknown.
    for (rest, path) in [
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\"R\":{\
             \"type\":\"R\",\"from\":{\"node\":\"N\"},\"to\":{\"node\":\"N\"},\"bogus\":1}}",
            "/relationships/R/bogus",
        ),
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\"R\":{\
             \"type\":\"R\",\"from\":{\"node\":\"N\"},\"to\":{\"node\":\"N\"},\"name\":\"R\"}}",
            "/relationships/R/name",
        ),
    ] {
        let err = parse_ontology_document(json_doc(rest).as_bytes()).unwrap_err();
        assert!(err.to_string().contains(path), "{path}: {err}");
        assert!(err.to_string().contains("unknown field"), "{path}: {err}");
    }
}

#[test]
fn relationship_target_clause_fixtures() {
    // Main requires no member: empty, node-only, label-only and nullable
    // counts are all accepted, with no minimums (a negative count is
    // carried verbatim).
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\
             \"empty\":{\"type\":\"E\",\"from\":{},\"to\":{}},\
             \"forms\":{\"type\":\"F\",\"from\":{\"node\":\"N\",\"label\":\"N\",\
                        \"count\":null},\
                       \"to\":{\"label\":\"Only\",\"min_count\":-1,\"max_count\":null}},\
             \"exact\":{\"type\":\"X\",\"from\":{\"node\":\"N\",\"count\":2},\
                        \"to\":{\"node\":\"N\",\"max_count\":5}}}",
        )
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(doc.relationships()[0].from.node, None);
    assert_eq!(doc.relationships()[1].from.count, None);
    assert_eq!(doc.relationships()[1].to.node, None);
    assert_eq!(doc.relationships()[1].to.min_count, Some(-1));
    assert_eq!(doc.relationships()[2].from.count, Some(2));

    // The one cardinality conflict the model cannot represent is still
    // rejected: `count` with a range bound.
    for extra in ["\"min_count\":1", "\"max_count\":2"] {
        let doc = json_doc(&format!(
            ",\"nodes\":{{\"N\":{{\"label\":\"N\"}}}},\"relationships\":{{\"R\":{{\"type\":\"R\",\
             \"from\":{{\"node\":\"N\",\"count\":1,{extra}}},\"to\":{{\"node\":\"N\"}}}}}}"
        ));
        let err = parse_ontology_document(doc.as_bytes()).unwrap_err();
        assert!(
            err.to_string().contains("cannot be combined"),
            "{extra}: {err}"
        );
        assert!(
            err.to_string().contains("/relationships/R/from"),
            "{extra}: {err}"
        );
    }

    // Targets are closed and counts are integers.
    let err = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\"R\":{\"type\":\"R\",\
             \"from\":{\"node\":\"N\",\"property\":\"p\"},\"to\":{\"node\":\"N\"}}}",
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("/relationships/R/from/property"),
        "{err}"
    );
    let err = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\"R\":{\"type\":\"R\",\
             \"from\":{\"node\":\"N\",\"count\":\"two\"},\"to\":{\"node\":\"N\"}}}",
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("/relationships/R/from/count"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// Constraint clause
// ---------------------------------------------------------------------------

#[test]
fn constraint_clause_fixtures() {
    // Id-keyed maps with the uppercase type enum, nullable name, an
    // empty property list and all four kinds; one closed shape for both
    // owners (ADR-0487 §2).
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\
             \"a\":{\"type\":\"STRING\"},\"b\":{\"type\":\"INTEGER\"}},\
             \"constraints\":{\
             \"c-exists\":{\"type\":\"EXISTS\",\"properties\":[\"a\"],\
                          \"name\":null},\
             \"c-key\":{\"type\":\"KEY\",\"properties\":[\"a\",\"b\"],\
                       \"name\":\"c_key\"},\
             \"c-type\":{\"type\":\"PROPERTY_TYPE\",\"properties\":[\"a\"]},\
             \"c-unique\":{\"type\":\"UNIQUE\",\"properties\":[],\
                          \"extensions\":{\"custom\":{}}}}}}",
        )
        .as_bytes(),
    )
    .unwrap();
    let constraints = &doc.nodes()[0].constraints;
    assert_eq!(constraints[0].id, "c-exists");
    assert_eq!(constraints[1].constraint_type, ConstraintType::Key);
    assert_eq!(constraints[1].properties, ["a", "b"]);
    assert_eq!(constraints[2].constraint_type, ConstraintType::PropertyType);
    assert!(constraints[3].properties.is_empty());

    // Required members and the enum.
    for (missing, body) in [
        ("type", "\"properties\":[\"a\"]"),
        ("properties", "\"type\":\"UNIQUE\""),
    ] {
        let err = parse_ontology_document(
            json_doc(&format!(
                ",\"nodes\":{{\"N\":{{\"label\":\"N\",\"constraints\":{{\"c\":{{{body}}}}}}}}}"
            ))
            .as_bytes(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains(missing),
            "missing {missing}: {err}"
        );
    }
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"label\":\"N\",\"constraints\":{\"c\":{\"type\":\"mandatory\",\"properties\":[\"p\"]}}}}")
            .as_bytes(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("invalid constraint type"), "{err}");

    // The draft's array-of-objects and `constraint_type` are gone.
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"label\":\"N\",\"constraints\":[{\"type\":\"KEY\",\"properties\":[\"a\"]}]}}")
            .as_bytes(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("/nodes/N/constraints"), "{err}");
    assert!(err.to_string().contains("must be an object"), "{err}");
    let err = parse_ontology_document(
        json_doc(",\"nodes\":{\"N\":{\"label\":\"N\",\"constraints\":{\"c\":{\"constraint_type\":\"key\",\"properties\":[\"a\"]}}}}")
            .as_bytes(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");

    // No constraint owner has a `label` member in 1.1.0: the node
    // constraint lost it with the 1.0.0 → 1.1.0 bump (ADR-0487 §2), and
    // relationship constraints never had one.
    for (rest, path) in [
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"constraints\":{\"c\":{\"type\":\"UNIQUE\",\"properties\":[\"a\"],\"label\":\"N\"}}}}",
            "/nodes/N/constraints/c/label",
        ),
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\"}},\"relationships\":{\"R\":{\"type\":\"R\",\
             \"from\":{\"node\":\"N\"},\"to\":{\"node\":\"N\"},\
             \"constraints\":{\"c\":{\"type\":\"UNIQUE\",\"properties\":[\"a\"],\"label\":\"N\"}}}}",
            "/relationships/R/constraints/c/label",
        ),
    ] {
        let err = parse_ontology_document(json_doc(rest).as_bytes()).unwrap_err();
        assert!(err.to_string().contains(path), "{path}: {err}");
        assert!(err.to_string().contains("unknown field"), "{path}: {err}");
    }
}

// ---------------------------------------------------------------------------
// Extension clauses
// ---------------------------------------------------------------------------

#[test]
fn typed_extensions_are_accepted() {
    let doc = parse_ontology_document(
        json_doc(
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"extensions\":{\
             \"indexes\":{\
             \"i-range\":{\"type\":\"RANGE\",\"labels\":[\"N\"],\"properties\":[\"p\"],\
                         \"options\":null,\"custom\":{},\"name\":null},\
             \"i-fulltext\":{\"type\":\"FULLTEXT\",\"labels\":[\"N\"],\"properties\":[\"p\"],\
                            \"options\":{\"type\":\"FULLTEXT\",\
                                        \"fulltext.default_analyzer\":\"standard\",\
                                        \"fulltext.eventually_consistent\":true}}},\
             \"display\":{\"x\":1,\"y\":2.5,\"colour\":\"#fff\",\"custom\":{\"icon\":\"n\"}},\
             \"custom\":{\"owner\":\"relate\"}}}},\
             \"relationships\":{\"R\":{\"type\":\"R\",\"from\":{\"node\":\"N\"},\
                    \"to\":{\"node\":\"N\"},\
                    \"extensions\":{\"indexes\":{\"i\":{\"type\":\"LOOKUP\",\
                    \"properties\":[\"p\"],\"custom\":{}}}}}}",
        )
        .as_bytes(),
    )
    .unwrap();
    // The payloads stay on the root value tree, in source order.
    let root = doc.root();
    let node_extensions = at(root, &["nodes", "N", "extensions"]);
    assert_eq!(
        at(node_extensions, &["indexes"])
            .as_object()
            .expect("indexes is an object")
            .len(),
        2
    );
    assert_eq!(
        at(node_extensions, &["indexes", "i-fulltext", "type"]).as_str(),
        Some("FULLTEXT")
    );
    assert!(!at(node_extensions, &["display"]).is_null());
    assert!(!at(node_extensions, &["custom"]).is_null());
    assert_eq!(
        at(
            root,
            &["relationships", "R", "extensions", "indexes", "i", "type"]
        )
        .as_str(),
        Some("LOOKUP")
    );
}

#[test]
fn document_extensions_are_carried_structurally() {
    // Document `data_source_tables` / `data_source_mappings` / `custom`
    // are carried structurally (the 1.1.0 spellings, ADR-0487 §2).
    let doc = parse_ontology_document(
        json_doc(
            ",\"extensions\":{\
             \"data_source_tables\":{\"t\":{\"source\":\"csv\",\"columns\":[{\"name\":\"id\",\
             \"type\":\"varchar\",\"suggested\":\"STRING\",\"supported\":[\"STRING\",\"UUID\"],\
             \"nullable\":false}],\
             \"primary_keys\":[\"id\"],\"foreign_keys\":{\"fk\":{\"columns\":[\"id\"],\
             \"references\":{\"table\":\"t\",\"columns\":[\"id\"]}}}}},\
             \"data_source_mappings\":{\"m\":[{\"type\":\"NODE\",\"node\":\"N\",\"table\":\"t\",\
             \"properties\":{\"id\":{\"column\":\"id\"}},\"mode\":\"MERGE\",\
             \"key\":[\"id\"],\"depends_on\":\"\",\"id\":\"n\"},\
             {\"type\":\"QUERY\",\"table\":\"t\",\"query\":\"RETURN 1\",\"depends_on\":\"\"},\
             {\"type\":\"RELATIONSHIP\",\"relationship\":\"R\",\"table\":\"t\",\
              \"from_node\":{\"label\":\"N\",\"properties\":{\"id\":{\"column\":\"id\"}}},\
              \"to_node\":{},\"properties\":{},\"mode\":\"CREATE\",\"key\":[],\
              \"depends_on\":\"\"}]},\
             \"custom\":{\"owner\":\"relate\"}}",
        )
        .as_bytes(),
    )
    .unwrap();
    let root = doc.root();
    assert_eq!(
        at(root, &["extensions", "data_source_tables", "t"])
            .as_object()
            .expect("table t is an object")
            .len(),
        4
    );
    assert_eq!(
        at(root, &["extensions", "data_source_mappings", "m"])
            .as_array()
            .expect("data_source_mappings m is an array")
            .len(),
        3
    );
    assert!(!at(root, &["extensions", "custom"]).is_null());
}

#[test]
fn extension_envelopes_are_closed_and_typed_payloads_are_checked() {
    // One rejection table: every row is a document fragment and the
    // needles its error must carry. The envelopes are closed (only the
    // schema's members are accepted) and the typed payloads are validated
    // deeply, not carried as opaque keys.
    for (rest, needles) in [
        // Closed envelopes: only the schema's members are accepted.
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"extensions\":{\"neo4j:index\":{}}}}",
            ["/nodes/N/extensions/neo4j:index", "unknown field"].as_slice(),
        ),
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"properties\":{\"p\":{\"extensions\":{\"alias\":{}}}}}}",
            ["/nodes/N/properties/p/extensions/alias", "unknown field"].as_slice(),
        ),
        // A bad index type token, and a display payload missing `y`.
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"extensions\":{\"indexes\":{\"i\":{\"type\":\"NOPE\",\"properties\":[]}}}}}",
            ["/nodes/N/extensions/indexes/i/type"].as_slice(),
        ),
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"extensions\":{\"display\":{\"x\":1}}}}",
            ["missing required field 'y'"].as_slice(),
        ),
        // A bad `suggested` token, the 1.1.0 shape's new plain-boolean
        // column member (ADR-0487 §2), and an unknown table member.
        (
            ",\"extensions\":{\"data_source_tables\":{\"t\":{\"source\":\"csv\",\"columns\":[{\"suggested\":\"NOPE\"}]}}}",
            [
                "/extensions/data_source_tables/t/columns/0/suggested",
                "invalid Neo4jType token",
            ]
            .as_slice(),
        ),
        (
            ",\"extensions\":{\"data_source_tables\":{\"t\":{\"source\":\"csv\",\"columns\":[{\"nullable\":\"yes\"}]}}}",
            [
                "/extensions/data_source_tables/t/columns/0/nullable",
                "must be a boolean",
            ]
            .as_slice(),
        ),
        (
            ",\"extensions\":{\"data_source_tables\":{\"t\":{\"source\":\"csv\",\"unknown\":1}}}",
            ["/extensions/data_source_tables/t/unknown", "unknown field"].as_slice(),
        ),
        // A node mapping without its `node` reference.
        (
            ",\"extensions\":{\"data_source_mappings\":{\"m\":[{\"type\":\"NODE\",\"table\":\"t\",\"properties\":{}}]}}",
            ["node", "missing required field"].as_slice(),
        ),
        // The 1.0.0 key spellings are unknown members now (ADR-0487 §2).
        (
            ",\"extensions\":{\"tables\":{\"t\":{\"source\":\"csv\"}}}",
            ["/extensions/tables", "unknown field"].as_slice(),
        ),
        (
            ",\"extensions\":{\"mappings\":{}}",
            ["/extensions/mappings", "unknown field"].as_slice(),
        ),
        // `custom` is the one free-form member — but still an object.
        (
            ",\"nodes\":{\"N\":{\"label\":\"N\",\"extensions\":{\"custom\":[]}}}",
            ["'custom' must be an object"].as_slice(),
        ),
    ] {
        let err = parse_ontology_document(json_doc(rest).as_bytes()).unwrap_err();
        for needle in needles {
            assert!(err.to_string().contains(needle), "{needle}: {err}");
        }
    }

    // `custom` is never interpreted: an arbitrary object is accepted.
    let doc = parse_ontology_document(
        json_doc(",\"extensions\":{\"custom\":{\"anything\":[1,{\"nested\":true}],\"null\":null}}")
            .as_bytes(),
    )
    .unwrap();
    assert!(!at(doc.root(), &["extensions", "custom"]).is_null());
}

// ---------------------------------------------------------------------------
// Value-model restrictions (ADR-0227 §4)
// ---------------------------------------------------------------------------

#[test]
fn constructs_outside_the_value_model_are_rejected() {
    // Non-string mapping key.
    let err = parse_ontology_document(b"{? [1]: x}").unwrap_err();
    assert!(err.to_string().contains("non-string"), "{err}");

    // Custom tag.
    let yaml = format!("schema: !custom {PINNED_SCHEMA_URI}\nid: t\nversion: \"1\"\n");
    let err = parse_ontology_document(yaml.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("tag"), "{err}");

    // Non-finite float.
    let yaml = format!("schema: {PINNED_SCHEMA_URI}\nid: t\nversion: \"1\"\ndescription: .inf\n");
    let err = parse_ontology_document(yaml.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("non-finite"), "{err}");

    // Multi-document YAML.
    let yaml = format!("schema: {PINNED_SCHEMA_URI}\nid: t\nversion: \"1\"\n---\nid: other\n");
    let err = parse_ontology_document(yaml.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("single YAML document"), "{err}");

    // Garbage.
    let err = parse_ontology_document(b"{ not json [").unwrap_err();
    assert!(matches!(err, OntologyError::Syntax { .. }), "{err}");
}
