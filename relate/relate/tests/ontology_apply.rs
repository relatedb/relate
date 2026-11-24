//! `db.ontology.apply` end to end through the facade (ADR-0226 §2/§3,
//! revised by ADR-0667): the delta row the
//! call answers — the applied model's name else id, the constraints
//! created and dropped, the skip reports — the SET semantics behind the
//! counts, and the validate-first data check (ADR-0226 §3) that fails a
//! violating-data apply before any catalog redo. The rest of the family
//! is pinned in `ontology_procedures.rs`.

mod common;

use relate::{Database, Value};
use relate_ontology::PINNED_SCHEMA_URI as SCHEMA_URI;

use common::err_text;
use common::ontology::{apply_row, set_doc, string_list, MOVIES};

/// The one read of the current graph type's specification.
const SHOW_SPEC: &str = "SHOW CURRENT GRAPH TYPE YIELD specification RETURN specification";

/// (5) `apply` (ADR-0226 §2/§3, revised by ADR-0667): fails when no ontology
/// is stored; fails naming the constraint when existing data violates the
/// resulting graph type (ADR-0226 §3, before any catalog redo); applies
/// the movies fixture as 21 created constraints with the seven skip
/// reports; a second apply of the same document is 0/0; the delta of a
/// changed ontology is exactly the difference; and an empty-but-valid
/// ontology drops everything.
#[test]
fn apply_semantics() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();

    // No ontology set.
    let err = err_text(conn.query("CALL db.ontology.apply()"));
    assert!(
        err.contains("This database has no stored ontology."),
        "{err}"
    );

    // Violating data: an Actor without the key property `id`. The
    // validate-first scan fails before any catalog redo.
    conn.query("CREATE (:Actor {name: 'noid'})").unwrap();
    set_doc(&mut conn, MOVIES).unwrap();
    let err = err_text(conn.query("CALL db.ontology.apply()"));
    assert!(err.contains("ConstraintValidationFailed"), "{err}");
    assert!(err.contains("id_Actor_key"), "{err}");
    assert_eq!(
        common::scalar_one(&mut conn, SHOW_SPEC),
        Value::String("{}".into()),
        "the failed apply changed nothing"
    );

    // Fix the data; apply succeeds and answers the delta row.
    conn.query("MATCH (a:Actor) SET a.id = 'a0'").unwrap();
    let (ontology, created, dropped, skipped) = apply_row(&mut conn);
    assert_eq!(ontology, "Movies");
    assert_eq!(created, 21, "the movies graph type derives 21 constraints");
    assert_eq!(dropped, 0);
    assert_eq!(
        skipped,
        [
            "aliases on DIRECTED",
            "cardinality on ACTED_IN.to",
            "description on DIRECTED",
            "description on DIRECTED.score",
            "one_of on Movie.genre",
            "optional labels on Movie",
            "reference on DIRECTED",
        ]
    );
    let graph_type = common::scalar_one(&mut conn, SHOW_SPEC)
        .as_string()
        .unwrap()
        .to_string();
    assert!(graph_type.contains("Actor"), "{graph_type}");
    assert!(graph_type.contains("id_Actor_key"), "{graph_type}");
    // And the derived key constraint now gates writes.
    let err = err_text(conn.query("CREATE (:Actor)"));
    assert!(err.contains("ConstraintValidationFailed"), "{err}");

    // A second apply of the same document is 0/0.
    let (ontology, created, dropped, skipped) = apply_row(&mut conn);
    assert_eq!(ontology, "Movies");
    assert_eq!(created, 0);
    assert_eq!(dropped, 0);
    assert_eq!(skipped.len(), 7);

    // The delta of a changed ontology: adding the Genre node adds exactly
    // its two constraints.
    set_doc(&mut conn, &movies_with_genre()).unwrap();
    let (ontology, created, dropped, _) = apply_row(&mut conn);
    assert_eq!(ontology, "Movies");
    assert_eq!(created, 2, "Genre's property type and its key");
    assert_eq!(dropped, 0);

    // An empty-but-valid ontology drops every constraint and empties the
    // graph type.
    set_doc(
        &mut conn,
        &format!("{{\"schema\":\"{SCHEMA_URI}\",\"id\":\"empty\",\"version\":\"1\"}}"),
    )
    .unwrap();
    let (ontology, created, dropped, skipped) = apply_row(&mut conn);
    assert_eq!(ontology, "empty", "the id answers when no name is set");
    assert_eq!(created, 0);
    assert_eq!(dropped, 23, "the 21 movies constraints and Genre's 2");
    assert!(skipped.is_empty());
    assert_eq!(
        common::scalar_one(&mut conn, SHOW_SPEC),
        Value::String("{}".into())
    );

    // apply inside an explicit transaction (ADR-0121 §3), then rollback
    // leaves the committed graph type alone.
    set_doc(&mut conn, MOVIES).unwrap();
    conn.begin().unwrap();
    let (_, created, _, _) = apply_row(&mut conn);
    assert_eq!(created, 21);
    conn.rollback().unwrap();
    assert_eq!(
        common::scalar_one(&mut conn, SHOW_SPEC),
        Value::String("{}".into()),
        "rollback discards the applied graph type"
    );
    conn.begin().unwrap();
    apply_row(&mut conn);
    conn.commit().unwrap();
    assert!(
        common::scalar_one(&mut conn, SHOW_SPEC)
            .as_string()
            .unwrap()
            .contains("Actor"),
        "the committed apply's graph type is current"
    );
}

/// (5b) `SET` semantics (ADR-0667): constraints not implied by the
/// ontology are dropped and counted, and an equivalent constraint under a
/// different name is refused up front with the remediation.
#[test]
fn apply_set_semantics_drops_and_refuses_renames() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();

    // An unrelated named constraint is dropped and counted beside the 21.
    conn.query("CREATE CONSTRAINT person_email_unique FOR (p:Person) REQUIRE p.email IS UNIQUE")
        .unwrap();
    set_doc(&mut conn, MOVIES).unwrap();
    let (_, created, dropped, _) = apply_row(&mut conn);
    assert_eq!(created, 21);
    assert_eq!(dropped, 1);
    let result = conn
        .query("SHOW CONSTRAINTS YIELD name RETURN collect(name) AS names")
        .unwrap();
    let names = string_list(&result.rows[0][0]);
    assert_eq!(names.len(), 21);
    assert!(!names.contains(&"person_email_unique".to_string()));

    // An equivalent constraint under a different name: refused before any
    // schema change, naming both and the remediation.
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE CONSTRAINT my_actor_id_key FOR (a:Actor) REQUIRE a.id IS KEY")
        .unwrap();
    set_doc(&mut conn, MOVIES).unwrap();
    let err = err_text(conn.query("CALL db.ontology.apply()"));
    assert!(err.contains("'id_Actor_key'"), "{err}");
    assert!(err.contains("'my_actor_id_key'"), "{err}");
    assert!(err.contains("DROP CONSTRAINT `my_actor_id_key`"), "{err}");
    let result = conn
        .query("SHOW CONSTRAINTS YIELD name RETURN collect(name) AS names")
        .unwrap();
    let names = string_list(&result.rows[0][0]);
    assert_eq!(names.len(), 1, "the refused apply changed nothing");
    assert_eq!(names[0], "my_actor_id_key");
}

/// (5c) Ontology parts without a graph-type equivalent are reported in
/// `skipped`, not failed (ADR-0667): untyped properties keep only their
/// existence requirement, a node left empty by the omissions is dropped
/// but keeps its key constraint's definition.
#[test]
fn apply_reports_unrepresentable_parts_in_skipped() {
    common::ontology::preview_ontology();
    let unrepresentable = format!(
        "{{\"schema\":\"{SCHEMA_URI}\",\"id\":\"unrepresentable\",\"name\":\"Unrepresentable\",\
         \"version\":\"1\",\
         \"nodes\":{{\
         \"Untyped\":{{\"label\":\"Untyped\",\"properties\":{{\
         \"name\":{{\"type\":\"STRING\"}},\
         \"misc\":{{}},\
         \"tags\":{{\"type\":\"LIST<ANY>\"}},\
         \"ids\":{{\"type\":\"LIST<ANY>\",\"must_exist\":true}}}}}},\
         \"Blob\":{{\"label\":\"Blob\",\"properties\":{{\
         \"id\":{{\"type\":\"ANY\",\"key\":true}}}}}}}}}}"
    );
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    set_doc(&mut conn, &unrepresentable).unwrap();
    let (ontology, created, dropped, skipped) = apply_row(&mut conn);
    assert_eq!(ontology, "Unrepresentable");
    // The property type on Untyped.name, the existence on Untyped.ids
    // (a required unrepresentable type keeps its existence requirement)
    // and Blob's named key.
    assert_eq!(created, 3);
    assert_eq!(dropped, 0);
    assert_eq!(
        skipped,
        [
            "element type Blob has no representable content",
            "untyped list property Untyped.ids (not representable)",
            "untyped list property Untyped.tags (not representable)",
            "untyped property Blob.id (not representable)",
            "untyped property Untyped.misc (not representable)",
        ]
    );
    let spec = common::scalar_one(&mut conn, SHOW_SPEC)
        .as_string()
        .unwrap()
        .to_string();
    assert!(spec.contains("`name` :: STRING"), "{spec}");
    assert!(spec.contains("`ids` :: ANY NOT NULL"), "{spec}");
}

/// (5d) Main-valid documents the graph-type body cannot express are
/// stored and readable, but `apply` fails loudly naming the construct —
/// never a silent drop (ADR-0364 §3/§5.4, kept by ADR-0667).
#[test]
fn apply_rejects_main_valid_but_unconvertible_documents() {
    common::ontology::preview_ontology();
    let header = |id: &str| {
        serde_json::json!({
            "schema": SCHEMA_URI,
            "id": id,
            "version": "1",
        })
    };
    let mut property_type = header("pt");
    property_type["nodes"] = serde_json::json!({
        "N": {
            "label": "N",
            "properties": {"p": {"type": "STRING"}},
            "constraints": {
                "c": {"type": "PROPERTY_TYPE", "properties": ["p"]},
            },
        },
    });
    let mut bare_endpoint = header("bare");
    bare_endpoint["nodes"] = serde_json::json!({"N": {"label": "N"}});
    bare_endpoint["relationships"] = serde_json::json!({
        "R": {"type": "R", "from": {}, "to": {"node": "N"}},
    });
    let mut no_label = header("nolabel");
    no_label["nodes"] = serde_json::json!({"N": {"properties": {"p": {"type": "STRING"}}}});
    let property_type = property_type.to_string();
    let bare_endpoint = bare_endpoint.to_string();
    let no_label = no_label.to_string();
    // A `PROPERTY_TYPE` constraint object, a node entry with no
    // identifying label and an endpoint with neither a node nor a
    // label: all spec-valid, none expressible (ADR-0364 §5.4, the
    // endpoint and label rules of ADR-0667).
    for (doc, needle) in [
        (property_type, "PROPERTY_TYPE"),
        (bare_endpoint, "references unknown 'from' node ''."),
        (no_label, "has no identifying label."),
    ] {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        // `set` accepts every one of these: they are main-valid.
        set_doc(&mut conn, &doc).unwrap();
        let err = err_text(conn.query("CALL db.ontology.apply()"));
        assert!(err.contains(needle), "{doc}: {err}");
        // The failed apply changed nothing.
        assert_eq!(
            common::scalar_one(&mut conn, SHOW_SPEC),
            Value::String("{}".into()),
            "{doc}"
        );
    }
}

/// The movies ontology with the `Genre` node spliced in — the delta
/// fixture, built from the committed fixture so the two cannot drift.
fn movies_with_genre() -> String {
    let mut doc: serde_json::Value = serde_json::from_str(MOVIES).unwrap();
    doc["nodes"]["Genre"] = serde_json::json!({
        "label": "Genre",
        "properties": { "id": { "type": "STRING", "key": true } }
    });
    doc.to_string()
}
