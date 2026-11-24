//! Unit tests for the canonicalize path (ADR-0667): the canonical form
//! and the three user-facing envelopes.

use crate::canonicalize_ontology_document;
use crate::error::PINNED_SCHEMA_URI;
use crate::parse_stored_ontology_document;
use crate::to_graph_type_body;
use crate::OntologyError;
use relate_parser::{PredefinedKind, TypeExpr};

/// One library document, in both syntaxes — the same logical document.
const LIBRARY_JSON: &str = r#"{
  "schema": "https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json",
  "version": "1",
  "id": "library",
  "name": "Library",
  "description": "A small library model",
  "nodes": {
    "book": {
      "label": "Book",
      "properties": {
        "isbn": { "type": "STRING", "must_exist": true, "key": true }
      }
    },
    "author": {
      "labels": { "identifier": "Author", "implied": [ "Person" ] },
      "properties": { "name": { "type": "STRING", "must_exist": true } }
    }
  },
  "relationships": {
    "wrote": {
      "type": "WROTE",
      "from": { "node": "author" },
      "to": { "node": "book" }
    }
  }
}"#;

const LIBRARY_YAML: &str = r#"schema: "https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json"
version: "1"
id: library
name: Library
description: "A small library model"
nodes:
  book:
    label: Book
    properties:
      isbn:
        type: STRING
        must_exist: true
        key: true
  author:
    labels:
      identifier: Author
      implied:
        - Person
    properties:
      name:
        type: STRING
        must_exist: true
relationships:
  wrote:
    type: WROTE
    from:
      node: author
    to:
      node: book
"#;

/// The library document's canonical form: the schema's members in the
/// schema's order, the `label` shorthand expanded into `labels`, the
/// flags expanded into named constraint entries, and nothing else.
const LIBRARY_CANONICAL: &str = concat!(
    r#"{"schema":"https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json","#,
    r#""version":"1","id":"library","name":"Library","description":"A small library model","#,
    r#""nodes":{"book":{"labels":{"identifier":"Book"},"properties":{"isbn":{"type":"STRING"}},"#,
    r#""constraints":{"isbn_book_key":{"type":"KEY","properties":["isbn"],"name":"isbn_book_key"},"#,
    r#""isbn_book_exists":{"type":"EXISTS","properties":["isbn"],"name":"isbn_book_exists"}}},"#,
    r#""author":{"labels":{"identifier":"Author","implied":["Person"]},"#,
    r#""properties":{"name":{"type":"STRING"}},"#,
    r#""constraints":{"name_author_exists":{"type":"EXISTS","properties":["name"],"#,
    r#""name":"name_author_exists"}}}},"#,
    r#""relationships":{"wrote":{"type":"WROTE","from":{"node":"author"},"to":{"node":"book"}}}}"#
);

/// A minimal document canonicalizes to the header alone: the required
/// members in the schema's order, no optional member written empty.
#[test]
fn a_minimal_document_canonicalizes_to_the_header() {
    let canonical = canonicalize_ontology_document(
        format!(r#"{{"schema":"{PINNED_SCHEMA_URI}","id":"t","version":"1"}}"#).as_bytes(),
    )
    .expect("the minimal document canonicalizes");
    assert_eq!(
        canonical,
        format!(r#"{{"schema":"{PINNED_SCHEMA_URI}","version":"1","id":"t"}}"#)
    );
}

/// Equivalent JSON and YAML submissions of one document derive
/// byte-identical canonical output, and the canonical form is a fixed
/// point: canonicalizing it again returns the same bytes.
#[test]
fn equivalent_inputs_canonicalize_identically_and_the_form_is_a_fixed_point() {
    let from_json = canonicalize_ontology_document(LIBRARY_JSON.as_bytes())
        .expect("the JSON twin canonicalizes");
    let from_yaml = canonicalize_ontology_document(LIBRARY_YAML.as_bytes())
        .expect("the YAML twin canonicalizes");
    assert_eq!(from_json, LIBRARY_CANONICAL);
    assert_eq!(from_json, from_yaml);
    let again = canonicalize_ontology_document(from_json.as_bytes())
        .expect("the canonical form re-canonicalizes");
    assert_eq!(again, LIBRARY_CANONICAL);
}

/// The shorthand is expanded: the `label` shorthand folds into the
/// `labels` object, and the `must_exist`/`key`/`unique` flags become
/// named constraint entries — no flag and no shorthand member survives.
#[test]
fn shorthand_is_expanded() {
    let canonical = canonicalize_ontology_document(LIBRARY_JSON.as_bytes()).unwrap();
    assert!(canonical.contains("\"labels\""), "{canonical}");
    assert!(!canonical.contains("\"label\":"), "{canonical}");
    assert!(canonical.contains("\"constraints\""), "{canonical}");
    assert!(!canonical.contains("must_exist"), "{canonical}");
    assert!(!canonical.contains("\"unique\""), "{canonical}");
    assert!(!canonical.contains("\"key\":"), "{canonical}");
    // The generated entries carry the mapper's names and kinds.
    assert!(
        canonical.contains(
            "\"isbn_book_key\":{\"type\":\"KEY\",\"properties\":[\"isbn\"],\"name\":\"isbn_book_key\"}"
        ),
        "{canonical}"
    );
    assert!(
        canonical.contains(
            "\"isbn_book_exists\":{\"type\":\"EXISTS\",\"properties\":[\"isbn\"],\
             \"name\":\"isbn_book_exists\"}"
        ),
        "{canonical}"
    );
}

/// Unknown members are dropped, not refused: the canonical form carries
/// only the schema's members, at the root, on elements and on
/// properties alike.
#[test]
fn unknown_fields_are_dropped() {
    let typoed = format!(
        concat!(
            r#"{{"schema":"{}","version":"1","id":"clean","noods":{{}}, "#,
            r#""nodes":{{"book":{{"label":"Book","bogus":1,"properties":{{"#,
            r#""isbn":{{"type":"STRING","typo":true}}}}}}}}}}"#
        ),
        PINNED_SCHEMA_URI
    );
    let canonical = canonicalize_ontology_document(typoed.as_bytes())
        .expect("a document with unknown members canonicalizes");
    assert!(!canonical.contains("noods"), "{canonical}");
    assert!(!canonical.contains("bogus"), "{canonical}");
    assert!(!canonical.contains("typo"), "{canonical}");
    assert!(canonical.contains("\"id\":\"clean\""), "{canonical}");
    assert!(
        canonical.contains("\"isbn\":{\"type\":\"STRING\"}"),
        "{canonical}"
    );
}

/// The drop rule's deliberate edge (ADR-0667): a legacy or typo'd
/// optional member — the pinned draft's `mustExist` spelling — never
/// enters the model. The canonical form carries no trace of it and the
/// conversion derives no constraint from it: the drop is recorded
/// behavior, not an accident, and `must_exist` is the only existence
/// spelling.
#[test]
fn a_dropped_legacy_member_derives_nothing() {
    let document = format!(
        concat!(
            r#"{{"schema":"{}","version":"1","id":"legacy","#,
            r#""nodes":{{"n":{{"label":"N","properties":{{"#,
            r#""id":{{"type":"STRING","mustExist":true}}}}}}}}}}"#
        ),
        PINNED_SCHEMA_URI
    );
    let canonical = canonicalize_ontology_document(document.as_bytes())
        .expect("the unknown member is dropped, not refused");
    assert!(!canonical.contains("mustExist"), "{canonical}");
    assert!(
        canonical.contains("\"id\":{\"type\":\"STRING\"}"),
        "{canonical}"
    );
    assert!(!canonical.contains("EXISTS"), "{canonical}");

    // The stored-document reader and the converter agree: the property
    // declares without `NOT NULL` and no constraint is derived from it.
    let doc = parse_stored_ontology_document(document.as_bytes())
        .expect("the stored-document reader drops it too");
    let mapping = to_graph_type_body(&doc).expect("the conversion carries nothing of it");
    assert_eq!(
        mapping.body.node_types[0].properties[0].value_type,
        TypeExpr::Predefined {
            kind: PredefinedKind::String,
            not_null: false,
        }
    );
    assert!(mapping.body.constraints.is_empty());
    assert!(mapping.skipped.is_empty());
}

/// Extension payloads ride the canonical form verbatim: each known
/// envelope member is grafted from the submitted document as-is.
#[test]
fn extension_payloads_are_carried_verbatim() {
    let document = format!(
        concat!(
            r#"{{"schema":"{}","version":"1","id":"ext","#,
            r#""nodes":{{"book":{{"label":"Book","extensions":{{"custom":{{"owner":"relate"}}}}}}}},"#,
            r#""extensions":{{"custom":{{"note":"kept"}}}}}}"#
        ),
        PINNED_SCHEMA_URI
    );
    let canonical = canonicalize_ontology_document(document.as_bytes())
        .expect("extension payloads canonicalize");
    assert!(
        canonical.contains("\"extensions\":{\"custom\":{\"owner\":\"relate\"}}"),
        "{canonical}"
    );
    assert!(
        canonical.contains("\"custom\":{\"note\":\"kept\"}"),
        "{canonical}"
    );
}

/// Input that parses as neither JSON nor YAML is refused with the
/// parse envelope: each attempt's message under its syntax label.
#[test]
fn unparsable_input_renders_the_parse_envelope() {
    for document in ["{ \"schema\": ", "[not json"] {
        let err = canonicalize_ontology_document(document.as_bytes()).unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with(
                "The ontology document could not be parsed as a graph-spec JSON or YAML \
                 document.\nJSON: "
            ),
            "{document}: {text}"
        );
        assert!(text.contains("\nYAML: "), "{document}: {text}");
    }
}

/// A `schema` member naming an unsupported version is refused with the
/// version envelope, naming the offered document and the supported one.
#[test]
fn an_unsupported_version_renders_the_version_envelope() {
    let future = LIBRARY_JSON.replace("/1.1.0/", "/9.9.9/");
    let err = canonicalize_ontology_document(future.as_bytes()).unwrap_err();
    let text = err.to_string();
    assert!(
        text.starts_with("The ontology document uses an unsupported graph-spec version. "),
        "{text}"
    );
    assert!(text.contains("9.9.9"), "{text}");
    assert!(!text.contains("could not be parsed"), "{text}");
    // The same refusal for the YAML twin: the JSON parse of a YAML
    // document fails first, the YAML parse reaches the gate.
    let err = canonicalize_ontology_document(LIBRARY_YAML.replace("/1.1.0/", "/9.9.9/").as_bytes())
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("The ontology document uses an unsupported graph-spec version."),
        "{err}"
    );
}

/// A document with six bad members renders the validation envelope: the
/// first five issues as `[<code>] <message> (<path>)` rows,
/// semicolon-joined, then the ellipsis-and-count suffix — the sixth
/// issue stays behind the cap.
#[test]
fn validation_issues_render_the_capped_envelope() {
    let document = issues_document(6);
    let err = canonicalize_ontology_document(document.as_bytes()).unwrap_err();
    let text = err.to_string();
    assert!(
        text.starts_with("The ontology document failed validation. "),
        "{text}"
    );
    for i in 0..5 {
        assert!(
            text.contains(&format!(
                "[invalid_type] a relationship target must be an object, found an integer \
                 (/relationships/r{i}/to)"
            )),
            "{text}"
        );
    }
    assert!(text.contains(" … and 1 more"), "{text}");
    assert!(!text.contains("r5"), "{text}");
    // Up to five issues, every issue renders and no suffix appears.
    let err = canonicalize_ontology_document(issues_document(3).as_bytes()).unwrap_err();
    assert_eq!(
        err.to_string(),
        concat!(
            "The ontology document failed validation. ",
            "[invalid_type] a relationship target must be an object, found an integer ",
            "(/relationships/r0/to); ",
            "[invalid_type] a relationship target must be an object, found an integer ",
            "(/relationships/r1/to); ",
            "[invalid_type] a relationship target must be an object, found an integer ",
            "(/relationships/r2/to)"
        )
    );
}

/// A document whose every relationship carries one invalid member: one
/// issue per entry, in entry order.
fn issues_document(count: usize) -> String {
    let mut document = format!(
        r#"{{"schema":"{PINNED_SCHEMA_URI}","version":"1","id":"issues","relationships":{{"#
    );
    for i in 0..count {
        document.push_str(&format!(
            r#""r{i}":{{"type":"R{i}","from":{{}},"to":{i}}}{}"#,
            if i + 1 < count { "," } else { "" }
        ));
    }
    document.push_str("}}");
    document
}

/// A property flag's generated constraint name colliding with an
/// explicit constraint's id is refused, never silently resolved: the
/// canonical form keys constraints by id and cannot carry both, so the
/// collision names the element and the colliding id instead of letting
/// either entry win and dropping the other's authored content.
#[test]
fn a_flag_generated_constraint_id_colliding_with_an_explicit_one_is_refused() {
    let document = format!(
        r#"{{"schema":"{PINNED_SCHEMA_URI}","version":"1","id":"t","nodes":{{"person":{{"label":"Person","properties":{{"ssn":{{"type":"STRING","key":true}}}},"constraints":{{"ssn_person_key":{{"type":"KEY","properties":["ssn"]}}}}}}}}}}"#
    );
    let Err(OntologyError::Mapping(message)) = canonicalize_ontology_document(document.as_bytes())
    else {
        panic!("the colliding document is refused, not canonicalized");
    };
    assert_eq!(
        message,
        "Ontology node 'person' declares an explicit constraint whose id \
         'ssn_person_key' collides with the name a property's key flag \
         generates."
    );
}
