//! ADR-0121 as amended by ADR-0227 and revised by ADR-0667: the
//! `db.ontology.set` / `get` / `drop` /
//! `apply` procedures, end to end through the facade — signatures and
//! `SHOW PROCEDURES` listing, the codec envelopes, canonical storage and
//! format rendering, the namespace rules, and `apply`'s delta row with
//! ADR-0226 §3's validate-first data check. The transaction rules live
//! in `ontology_transaction_rules.rs`.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use relate::{Database, Value};
use relate_ontology::PINNED_SCHEMA_URI as SCHEMA_URI;
use relate_storage::{MemoryVfs, Vfs};

use common::err_text;
use common::ontology::{canonical, get_doc, set_doc, DOC_JSON, MOVIES};

/// The one read of the current graph type's specification.
const SHOW_SPEC: &str = "SHOW CURRENT GRAPH TYPE YIELD specification RETURN specification";

/// A small valid document as freely formatted YAML — the twin of
/// `DOC_JSON` (the two denote one document, and the codec's
/// canonical form of either is byte-identical).
const DOC_YAML: &str = "\
# leading comment, dropped with the free formatting
schema: https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json
id: twin
version: \"1\"
name: Twin
nodes:
  Person:
    label: Person
    properties:
      id:
        type: STRING
        key: true
      tags:
        type: LIST<STRING>
relationships:
  KNOWS:
    type: KNOWS
    from:
      node: Person
    to:
      node: Person
      max_count: 5
";

/// A fresh database over a shared in-memory VFS, with the handle kept so
/// the reopen legs can replay the same store through a second open.
fn memory_db() -> (Arc<dyn Vfs>, Database) {
    let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
    let db = Database::open_vfs(Arc::clone(&vfs), Default::default()).unwrap();
    (vfs, db)
}

/// `CALL db.ontology.set($ontology)` with an explicit `null` argument.
fn set_null(conn: &mut relate::Connection) -> relate::Result<relate::QueryResult> {
    conn.query_with_params(
        "CALL db.ontology.set($ontology)",
        BTreeMap::from([("ontology".to_string(), Value::Null)]),
    )
}

/// (1) The codec's storage contract (ADR-0667): a JSON or YAML submission
/// of one document converges to one canonical JSON — the `label`
/// shorthand expanded, the property flags turned into constraint entries —
/// `get` re-renders in the requested format (the names case-insensitive,
/// `null` and absent both `yaml`), an unknown format is the exact
/// argument-error text, and the canonical bytes survive a reopen.
#[test]
fn set_canonicalizes_and_get_renders_both_formats() {
    common::ontology::preview_ontology();
    let (vfs, db) = memory_db();
    let mut conn = db.connect().unwrap();

    // The two syntaxes of one document canonicalize byte-identically.
    let canonical_json = canonical(DOC_JSON);
    assert_eq!(canonical_json, canonical(DOC_YAML));

    // YAML in: the stored form is the canonical JSON, not the submitted
    // bytes — the shorthand is expanded and the flags became constraints.
    set_doc(&mut conn, DOC_YAML).unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology"
        ),
        Value::String(canonical_json.clone().into())
    );
    assert!(
        canonical_json.contains("\"labels\":{\"identifier\":\"Person\"}"),
        "{canonical_json}"
    );
    assert!(canonical_json.contains("id_Person_key"), "{canonical_json}");
    assert!(!canonical_json.contains("\"key\":true"), "{canonical_json}");

    // The default get renders the canonical form as YAML (the block-style
    // rendering carries the YAML document separator); the explicit
    // spellings are case-insensitive, and `null` is the default too.
    let yaml = get_doc(
        &mut conn,
        "CALL db.ontology.get() YIELD ontology RETURN ontology",
    );
    let Value::String(yaml) = &yaml else {
        panic!("expected a string, got {yaml:?}")
    };
    let body = yaml.strip_prefix("---\n").unwrap_or(yaml);
    assert!(body.starts_with("schema: "), "{yaml}");
    assert!(yaml.contains("id: twin"), "{yaml}");
    assert!(yaml.contains("id_Person_key"), "{yaml}");
    for spelling in ["JSON", "Json", "jSoN"] {
        assert_eq!(
            get_doc(
                &mut conn,
                &format!("CALL db.ontology.get('{spelling}') YIELD ontology RETURN ontology")
            ),
            Value::String(canonical_json.clone().into()),
            "{spelling}"
        );
    }
    for spelling in ["YAML", "YamL", "yAmL"] {
        assert_eq!(
            get_doc(
                &mut conn,
                &format!("CALL db.ontology.get('{spelling}') YIELD ontology RETURN ontology")
            ),
            Value::String(yaml.clone()),
            "{spelling}"
        );
    }
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get(null) YIELD ontology RETURN ontology"
        ),
        Value::String(yaml.clone()),
        "a null format argument is the yaml default"
    );

    // JSON in: the same canonical bytes — the storage is deterministic
    // across syntaxes and submissions.
    set_doc(&mut conn, DOC_JSON).unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology"
        ),
        Value::String(canonical_json.clone().into())
    );

    // An unknown format is the exact argument error naming the two.
    let err = err_text(conn.query("CALL db.ontology.get('xml') YIELD ontology RETURN ontology"));
    assert!(
        err.ends_with("Unknown ontology format 'xml'. Valid formats are 'json' and 'yaml'."),
        "{err}"
    );

    // The canonical bytes survive a reopen (WAL replay on the shared VFS;
    // a graceful drop discards a non-durable store, ADR-0105, so leak it).
    drop(conn);
    db.leak_without_checkpoint();
    let db = Database::open_vfs(vfs, Default::default()).unwrap();
    let mut conn = db.connect().unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology"
        ),
        Value::String(canonical_json.into())
    );
}

/// (1b) Nothing stored answers exactly one row with `null`, in every
/// format spelling.
#[test]
fn get_answers_one_null_row_when_nothing_is_stored() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    for cypher in [
        "CALL db.ontology.get() YIELD ontology RETURN ontology",
        "CALL db.ontology.get('json') YIELD ontology RETURN ontology",
        "CALL db.ontology.get('yaml') YIELD ontology RETURN ontology",
    ] {
        assert_eq!(get_doc(&mut conn, cypher), Value::Null, "{cypher}");
    }
}

/// (2) `set` runs the codec before any commit (ADR-0667): each failure
/// form surfaces its envelope in the argument-error class and leaves the
/// stored document untouched — an unparsable document, an unsupported
/// spec version, a schema-invalid document (with the issue list capped),
/// and a document over the 16 MiB envelope.
#[test]
fn set_validates_before_committing() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    set_doc(&mut conn, DOC_JSON).unwrap();
    let stored = canonical(DOC_JSON);

    // Unparsable as either syntax: the parse envelope.
    let err = err_text(set_doc(&mut conn, "{ \"schema\": "));
    assert!(
        err.contains(
            "The ontology document could not be parsed as a graph-spec JSON or YAML document."
        ),
        "{err}"
    );
    assert!(err.contains("JSON:"), "{err}");
    assert!(err.contains("YAML:"), "{err}");
    // A JSON document whose root is not an object parses and fails the
    // structural validation instead.
    let err = err_text(set_doc(&mut conn, "[1, 2, 3]"));
    assert!(
        err.contains("The ontology document failed validation."),
        "{err}"
    );
    assert!(err.contains("the document root must be an object"), "{err}");

    // A newer spec version: the version envelope, naming 1.1.0 as the
    // highest supported.
    let err = err_text(set_doc(
        &mut conn,
        "{\"schema\":\"https://neo4j.com/docs/ontology-graph-spec/9.9.9/schema.json\",\"id\":\"x\",\"version\":\"1\"}",
    ));
    assert!(
        err.contains("The ontology document uses an unsupported graph-spec version."),
        "{err}"
    );
    assert!(err.contains("9.9.9"), "{err}");
    assert!(
        err.contains("supports up to Ontology Graph Spec 1.1.0"),
        "{err}"
    );

    // A schema-invalid document: the validation envelope, the issue list
    // capped at five with the ellipsis-and-count suffix.
    let err = err_text(set_doc(&mut conn, &six_invalid_type_nodes()));
    assert!(
        err.contains("The ontology document failed validation."),
        "{err}"
    );
    assert!(err.contains("invalid_enum_token"), "{err}");
    assert!(err.contains("node0"), "{err}");
    assert!(err.contains("node4"), "{err}");
    assert!(err.contains("… and 1 more"), "{err}");
    assert!(!err.contains("node5"), "{err}");

    // Over the 16 MiB envelope (ADR-0120 §3): refused before any commit.
    let huge = format!(
        "{{\"schema\":\"{SCHEMA_URI}\",\"id\":\"x\",\"version\":\"1\",\"description\":\"{}\"}}",
        "a".repeat(16 * 1024 * 1024)
    );
    let err = err_text(set_doc(&mut conn, &huge));
    assert!(err.contains("16 MiB"), "{err}");

    // Nothing was replaced.
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology"
        ),
        Value::String(stored.into())
    );
}

/// One document with six schema-invalid members — one per node, so the
/// validation envelope's five-issue cap leaves one behind the suffix.
fn six_invalid_type_nodes() -> String {
    let nodes: Vec<String> = (0..6)
        .map(|n| {
            format!(
                "\"node{n}\": {{\"label\": \"N{n}\", \"properties\": \
                 {{\"p\": {{\"type\": \"STRIN{n}\"}}}}}}"
            )
        })
        .collect();
    format!(
        "{{\"schema\":\"{SCHEMA_URI}\",\"id\":\"six\",\"version\":\"1\",\
         \"nodes\":{{{}}}}}",
        nodes.join(",")
    )
}

/// (2b) Unknown members are dropped, not rejected (ADR-0667): a typo'd
/// root member never reaches the store, and the canonical form the
/// document's own members survive in is what `get` serves.
#[test]
fn set_drops_unknown_members() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let typo = format!(
        "{{\"schema\":\"{SCHEMA_URI}\",\"id\":\"typo\",\"version\":\"1\",\
         \"noods\":{{\"book\":{{\"label\":\"Book\"}}}}}}"
    );
    set_doc(&mut conn, &typo).unwrap();
    let stored = get_doc(
        &mut conn,
        "CALL db.ontology.get('json') YIELD ontology RETURN ontology",
    );
    let Value::String(stored) = &stored else {
        panic!("expected a string")
    };
    assert!(stored.contains("\"id\":\"typo\""), "{stored}");
    assert!(!stored.contains("noods"), "{stored}");
}

/// (3) `set(null)`, `set('')` and `drop` all push the clear;
/// a later `set` replaces whatever is stored (last-set-wins); `drop` is
/// idempotent; and the empty state survives a reopen.
#[test]
fn set_null_and_empty_clear_and_replace() {
    common::ontology::preview_ontology();
    let (vfs, db) = memory_db();
    let mut conn = db.connect().unwrap();

    // A null argument clears: nothing is stored after it.
    set_doc(&mut conn, DOC_JSON).unwrap();
    set_null(&mut conn).unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get() YIELD ontology RETURN ontology"
        ),
        Value::Null
    );

    // The empty string clears too — the same redo the null argument and
    // `drop` push.
    set_doc(&mut conn, DOC_JSON).unwrap();
    set_doc(&mut conn, "").unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get() YIELD ontology RETURN ontology"
        ),
        Value::Null
    );

    // Last set wins.
    set_doc(&mut conn, DOC_JSON).unwrap();
    set_doc(&mut conn, MOVIES).unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology"
        ),
        Value::String(canonical(MOVIES).into())
    );

    // Drop with none set succeeds, and drop is idempotent.
    conn.query("CALL db.ontology.drop()").unwrap();
    conn.query("CALL db.ontology.drop()").unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get() YIELD ontology RETURN ontology"
        ),
        Value::Null
    );

    drop(conn);
    db.leak_without_checkpoint();
    let db = Database::open_vfs(vfs, Default::default()).unwrap();
    let mut conn = db.connect().unwrap();
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get() YIELD ontology RETURN ontology"
        ),
        Value::Null
    );
}

/// (6) `SHOW PROCEDURES` lists all four with the revised signatures and
/// modes (ADR-0121 §7, ADR-0667): the three writers render SCHEMA, `get`
/// READ, and `apply`'s four result columns are the rendered signature.
#[test]
fn show_procedures_lists_the_ontology_procedures() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let result = conn
        .query(
            "SHOW PROCEDURES YIELD name, signature, mode \
             WHERE name STARTS WITH 'db.ontology.' \
             RETURN name, signature, mode ORDER BY name",
        )
        .unwrap();
    let rows: Vec<(&str, &str, &str)> = result
        .rows
        .iter()
        .map(|row| {
            (
                row[0].as_string().unwrap(),
                row[1].as_string().unwrap(),
                row[2].as_string().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "db.ontology.apply",
                "db.ontology.apply() :: (ontology :: STRING, constraintsCreated :: INTEGER, \
                 constraintsDropped :: INTEGER, skipped :: LIST<STRING>)",
                "SCHEMA"
            ),
            ("db.ontology.drop", "db.ontology.drop()", "SCHEMA"),
            (
                "db.ontology.get",
                "db.ontology.get(format = \"yaml\" :: STRING) :: (ontology :: STRING)",
                "READ"
            ),
            (
                "db.ontology.set",
                "db.ontology.set(ontology :: STRING)",
                "SCHEMA"
            ),
        ]
    );
}

/// (7) Namespace gating (ADR-0227 §1, ADR-0229 §1): routed to
/// `system_ontologies` every procedure — `get` included — fails the
/// namespace's not-queryable gate with the 42N17 text, BEFORE the
/// neo4j-only text of ADR-0227 §1, which now fires only on `system`.
#[test]
fn namespace_gating() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    set_doc(&mut db.connect().unwrap(), DOC_JSON).unwrap();

    let mut ontologies = db.connect_namespace("system_ontologies").unwrap();
    for cypher in [
        "CALL db.ontology.set('x')",
        "CALL db.ontology.drop()",
        "CALL db.ontology.apply()",
        "CALL db.ontology.get() YIELD ontology RETURN ontology",
    ] {
        let err = err_text(ontologies.query(cypher));
        assert!(
            err.ends_with(
                "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead."
            ),
            "{cypher}: {err}"
        );
    }
    // Administration commands still run from there (ADR-0116 §2).
    ontologies.query("SHOW DATABASES").unwrap();

    let mut system = db.connect_namespace("system").unwrap();
    // The existing system gate (42N17) still rejects data clauses.
    let err = err_text(system.query("MATCH (n) RETURN n"));
    assert!(
        err.contains("The following unsupported clauses were used: MATCH."),
        "{err}"
    );
    // The ontology procedures (standalone calls pass the 42N17 shape gate)
    // then fail at the target-namespace check.
    for cypher in [
        "CALL db.ontology.get() YIELD ontology RETURN ontology",
        "CALL db.ontology.drop()",
    ] {
        let err = err_text(system.query(cypher));
        assert!(
            err.contains("Ontologies can only be stored for the user database 'neo4j'."),
            "{cypher}: {err}"
        );
    }

    // The same gate fires through USE routing on a neo4j session — before
    // the ontology target text there too.
    let mut conn = db.connect().unwrap();
    let err = err_text(conn.query("USE system_ontologies CALL db.ontology.drop()"));
    assert!(
        err.ends_with(
            "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead."
        ),
        "{err}"
    );
    let err = err_text(
        conn.query("USE system_ontologies CALL db.ontology.get() YIELD ontology RETURN ontology"),
    );
    assert!(
        err.ends_with(
            "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead."
        ),
        "{err}"
    );
}

/// (8) ADR-0229 §1/§3: after `set` the `system_ontologies` namespace stays
/// not queryable — no graph is derived from the stored document — while the
/// procedures against `neo4j` keep working on it.
#[test]
fn system_ontologies_is_not_queryable_after_set() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    set_doc(&mut conn, DOC_JSON).unwrap();

    // The stored ontology is reachable only through the procedures.
    for cypher in [
        "USE system_ontologies MATCH (n:Ontology) RETURN n.ontology AS db, n.id AS id",
        "USE system_ontologies MATCH (n) RETURN count(n) AS c",
        "USE system_ontologies CREATE (:Nope)",
    ] {
        let err = err_text(conn.query(cypher));
        assert!(
            err.ends_with(common::ce_errors::NOT_QUERYABLE_SUFFIX),
            "{cypher}: {err}"
        );
    }

    // The procedures against `neo4j` are unaffected: get returns the
    // stored document and apply still works on it.
    assert_eq!(
        get_doc(
            &mut conn,
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology"
        ),
        Value::String(canonical(DOC_JSON).into())
    );
    conn.query("CALL db.ontology.apply()").unwrap();
    let spec = common::scalar_one(&mut conn, SHOW_SPEC);
    assert!(
        spec.as_string().unwrap().contains("Person"),
        "{}",
        spec.as_string().unwrap()
    );
}

/// A multi-pair document (REL8-2042's shape): `PART_OF` declared across
/// two endpoint pairs, `KNOWS` declared once as the enforcing control.
const MULTI_PAIR_JSON: &str = concat!(
    "{\"schema\":\"https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json\",",
    "\"id\":\"multi-pair\",\"version\":\"1\",",
    "\"nodes\":{",
    "\"LedgerRecord\":{\"label\":\"LedgerRecord\",\"properties\":{",
    "\"k\":{\"type\":\"STRING\",\"key\":true}}},",
    "\"Draft\":{\"label\":\"Draft\",\"properties\":{",
    "\"k\":{\"type\":\"STRING\",\"key\":true}}},",
    "\"Todo\":{\"label\":\"Todo\",\"properties\":{",
    "\"k\":{\"type\":\"STRING\",\"key\":true}}},",
    "\"Source\":{\"label\":\"Source\",\"properties\":{",
    "\"k\":{\"type\":\"STRING\",\"key\":true}}}},",
    "\"relationships\":{",
    "\"part_of_ledgerrecord_to_draft\":{\"type\":\"PART_OF\",",
    "\"from\":{\"node\":\"LedgerRecord\"},",
    "\"to\":{\"node\":\"Draft\"}},",
    "\"part_of_todo_to_source\":{\"type\":\"PART_OF\",",
    "\"from\":{\"node\":\"Todo\"},",
    "\"to\":{\"node\":\"Source\"}},",
    "\"knows\":{\"type\":\"KNOWS\",",
    "\"from\":{\"node\":\"Todo\"},",
    "\"to\":{\"node\":\"Source\"}}}}"
);

/// (5c) A relationship type declared across several endpoint pairs
/// defines no graph type (ADR-0668): `apply` accepts the document — the
/// pre-fix refusal (`duplicate identifying label`) is gone — no
/// endpoint rows derive for the type, its relationships stay writable
/// under any labels, and the singly-declared type beside it enforces as
/// before.
#[test]
fn apply_accepts_multi_pair_relationship_types_as_vocabulary_only() {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();

    // Existing data already spanning the pairs — including an
    // undeclared cross pair — does not block the apply: the type
    // carries no constraints to violate.
    conn.query(
        "CREATE (:LedgerRecord {k: 'lr'}), (:Draft {k: 'd'}), \
         (:Todo {k: 't'}), (:Source {k: 's'}), (:Odd {k: 'o'}), \
         (:LedgerRecord {k: 'lr2'})-[:PART_OF]->(:Draft {k: 'd2'}), \
         (:Todo {k: 't2'})-[:PART_OF]->(:Source {k: 's2'})",
    )
    .unwrap();
    // The undeclared cross pair, onto an existing Draft.
    conn.query("MATCH (o:Odd), (d:Draft {k: 'd'}) CREATE (o)-[:PART_OF]->(d)")
        .unwrap();
    set_doc(&mut conn, MULTI_PAIR_JSON).unwrap();
    conn.query("CALL db.ontology.apply()").unwrap();

    // The type carries no endpoint rows and stays writable.
    assert_eq!(
        common::scalar_one(
            &mut conn,
            "SHOW CONSTRAINTS YIELD labelsOrTypes, type WHERE labelsOrTypes = ['PART_OF'] RETURN count(*)"
        ),
        Value::Int(0)
    );
    conn.query("MATCH (a:Todo {k: 't'}), (b:Source {k: 's'}) CREATE (a)-[:PART_OF]->(b)")
        .unwrap();
    conn.query("MATCH (a:Todo {k: 't'}), (b:Draft {k: 'd'}) CREATE (a)-[:PART_OF]->(b)")
        .unwrap();

    // The singly-declared type beside it enforces both endpoints.
    let refused =
        err_text(conn.query("MATCH (o:Odd), (s:Source {k: 's'}) CREATE (o)-[:KNOWS]->(s)"));
    assert!(
        refused.starts_with("ConstraintValidationFailed: endpoint label presence verification"),
        "the singly-declared type still enforces: {refused}"
    );
}
