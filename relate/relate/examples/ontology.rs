//! Ontology management through the `db.ontology.*` procedures (ADR-0121,
//! ADR-0226, ADR-0227 — revised by ADR-0667):
//! `set` validates a JSON or YAML document and stores its canonical form,
//! `get` returns it in the requested format (`yaml` by default), `apply`
//! turns it into the database's graph type and answers its delta row (the
//! applied model's name, the constraints created and dropped, and the
//! parts without a graph-type equivalent), and `drop` removes it — with
//! `set(null)` and `set('')` clearing it like `drop`. The stored document
//! is the sole source of truth: the `system_ontologies` namespace is
//! visible under this opt-in (ADR-0593) and not queryable — every other
//! statement routed to it fails with 42N17 (ADR-0229).
//!
//! Run with `cargo run -p relate --example ontology`.

use std::collections::BTreeMap;

use relate::{Database, Value};

/// A small ontology in YAML: one node type with a key property.
const ONTOLOGY: &str = "\
schema: https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json
id: example
version: \"1\"
nodes:
  Person:
    label: Person
    properties:
      id:
        type: STRING
        key: true
";

fn main() -> relate::Result<()> {
    // The `db.ontology.*` family is gated behind the preview opt-in
    // (ADR-0378). This in-process mutation is example-only: a library must
    // not set the process environment. Library callers instead have the
    // process owner set RELATE_PREVIEW=ontology before open — the
    // documented deployment path. The variable is read once, at open, so
    // it must be set before the call below.
    std::env::set_var("RELATE_PREVIEW", "ontology");
    let db = Database::open(":memory:")?;
    let mut conn = db.connect()?;

    // set validates the document (pinned spec version, schema shape) and
    // stores its canonical form: get returns that form re-rendered in the
    // requested format — the default yaml, or json.
    conn.query_with_params(
        "CALL db.ontology.set($ontology)",
        BTreeMap::from([("ontology".to_string(), Value::String(ONTOLOGY.into()))]),
    )?;
    let result = conn.query("CALL db.ontology.get() YIELD ontology RETURN ontology")?;
    let Value::String(yaml) = &result.rows[0][0] else {
        panic!("get answers the document as a string")
    };
    assert!(yaml.contains("id: example"), "{yaml}");
    let result = conn.query("CALL db.ontology.get('json') YIELD ontology RETURN ontology")?;
    assert!(matches!(&result.rows[0][0], Value::String(json) if json.starts_with('{')));

    // system_ontologies is visible under this opt-in (ADR-0593) and not
    // queryable: the stored document is reachable only through the
    // procedures, so an ordinary read routed there fails with the 42N17
    // text (ADR-0229 §1/§2). Without the opt-in the namespace is
    // invisible — unlisted, and refused by `USE` as an unknown name.
    let result = conn.query("SHOW DATABASES YIELD name RETURN name")?;
    assert_eq!(result.rows[2][0], Value::String("system_ontologies".into()));
    let err = conn
        .query("USE system_ontologies MATCH (n) RETURN count(n) AS c")
        .unwrap_err();
    assert!(err.to_string().ends_with(
        "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead."
    ));

    // apply sets the current graph type from the ontology and answers its
    // delta row; data violating it (a Person without the key property id)
    // would fail the call before anything is applied.
    let result = conn.query(
        "CALL db.ontology.apply() \
         YIELD ontology, constraintsCreated, constraintsDropped, skipped",
    )?;
    let row = &result.rows[0];
    assert_eq!(row[0].as_string(), Some("example"));
    assert_eq!(row[1].as_int(), Some(2));
    assert_eq!(row[2].as_int(), Some(0));
    assert_eq!(row[3], Value::List(Vec::new().into()));
    let result = conn.query("SHOW CURRENT GRAPH TYPE")?;
    assert!(result.rows[0][0].as_string().unwrap().contains("Person"));
    // The derived key constraint now gates writes.
    assert!(conn.query("CREATE (:Person)").is_err());
    conn.query("CREATE (:Person {id: 'p1'})")?;

    // A null argument clears the document — the same clear the empty
    // string and drop push; get then yields null.
    conn.query_with_params(
        "CALL db.ontology.set($ontology)",
        BTreeMap::from([("ontology".to_string(), Value::Null)]),
    )?;
    let result = conn.query("CALL db.ontology.get() YIELD ontology RETURN ontology")?;
    assert_eq!(result.rows[0][0], Value::Null);
    Ok(())
}
