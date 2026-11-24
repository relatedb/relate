//! The `db.ontology.*` preview opt-in and document helpers.

/// Opt this test process into the `ontology` preview feature (ADR-0378,
/// ADR-0593): the `db.ontology.*` procedures are gated behind
/// `RELATE_PREVIEW=ontology`, and the same row gates the
/// `system_ontologies` namespace's visibility; `Database::open` reads the
/// variable once per open. Every test in the enabled-gate ontology crates
/// and the visible-namespace suites calls this before its first open.
pub fn preview_ontology() {
    relate_test_support::preview::preview_ontology();
}

/// The Relate-authored Ontology Graph Spec 1.1.0 example
/// (`relate-ontology/schema/example.json`, the published shape
/// ADR-0487 §1 pins) — the shared valid document the `db.ontology.*`
/// fixtures pass as `$ontology`. `set` validates it; `apply` refuses the
/// constructs the derived graph cannot express, which the fixtures that
/// need `apply` assert on rather than rely on.
pub const ONTOLOGY_DOC: &str = include_str!("../../../relate-ontology/schema/example.json");

/// The movies fixture (ADR-0667): the apply-delta document whose graph
/// type derives 21 constraints and whose conversion skips the seven
/// decorative members.
pub const MOVIES: &str = include_str!("../../../relate-ontology/schema/movies.json");

/// A small valid document as pretty-printed JSON — the family's shared
/// small fixture. `DOC_YAML` in `ontology_procedures.rs` denotes the same
/// document, and the codec's canonical form of either is byte-identical.
pub const DOC_JSON: &str = r#"{
  "schema": "https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json",
  "id": "twin",
  "version": "1",
  "name": "Twin",
  "nodes": {
    "Person": {
      "label": "Person",
      "properties": {
        "id": { "type": "STRING", "key": true },
        "tags": { "type": "LIST<STRING>" }
      }
    }
  },
  "relationships": {
    "KNOWS": {
      "type": "KNOWS",
      "from": { "node": "Person" },
      "to": { "node": "Person", "max_count": 5 }
    }
  }
}"#;

/// The codec's canonical JSON of `doc` — the stored form every `set`
/// converges to, computed once per fixture so the storage assertions are
/// byte-exact.
pub fn canonical(doc: &str) -> String {
    relate_ontology::canonicalize_ontology_document(doc.as_bytes()).unwrap()
}

/// `CALL db.ontology.set($ontology)` with `doc` — Result-returning, so a
/// test can assert on the error text of a refused document as well as on
/// a successful set.
pub fn set_doc(conn: &mut relate::Connection, doc: &str) -> relate::Result<relate::QueryResult> {
    conn.query_with_params(
        "CALL db.ontology.set($ontology)",
        std::collections::BTreeMap::from([(
            "ontology".to_string(),
            relate::Value::String(doc.into()),
        )]),
    )
}

/// The single `ontology` cell of a `db.ontology.get` query, which must
/// succeed with exactly one row.
pub fn get_doc(conn: &mut relate::Connection, cypher: &str) -> relate::Value {
    let result = conn.query(cypher).unwrap();
    assert_eq!(result.header.columns, ["ontology"]);
    assert_eq!(result.rows.len(), 1);
    result.rows[0][0].clone()
}

/// `db.ontology.apply`'s one result row (ADR-0667): the applied model's
/// name else id, the created and dropped counts, and the skip reports
/// (sorted, as the converter reports them).
pub fn apply_row(conn: &mut relate::Connection) -> (String, i64, i64, Vec<String>) {
    let result = conn
        .query(
            "CALL db.ontology.apply() \
             YIELD ontology, constraintsCreated, constraintsDropped, skipped",
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1, "apply answers exactly one row");
    let row = &result.rows[0];
    (
        row[0].as_string().unwrap().to_string(),
        row[1].as_int().unwrap(),
        row[2].as_int().unwrap(),
        string_list(&row[3]),
    )
}

/// The strings of a LIST<STRING> result cell.
pub fn string_list(value: &relate::Value) -> Vec<String> {
    let relate::Value::List(items) = value else {
        panic!("expected a LIST<STRING>, got {value:?}")
    };
    items
        .iter()
        .map(|item| item.as_string().expect("a string item").to_string())
        .collect()
}
