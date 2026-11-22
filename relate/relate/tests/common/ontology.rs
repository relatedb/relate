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
