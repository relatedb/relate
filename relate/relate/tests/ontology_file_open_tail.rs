//! The vault-committed ontology's open tail (ADR-0649), end to end
//! through the facade: the reserved file's document becomes the
//! store's ontology state (`db.ontology.get` serves it, set/drop
//! refuse), its extension-envelope indexes install through the
//! replay path (`SHOW INDEXES` lists them, kinds and options
//! included), its graph type becomes the current one, and the whole
//! application is idempotent and drift-free across file edits — the
//! schema text mirror's open-time verification never reads a
//! committed change as drift.

mod common;

use std::collections::BTreeMap;

use common::ontology::preview_ontology;
use relate::{Database, OpenOptions, SnapshotCodecKind, Value};

/// One node, one label directory — the minimal vault that carries a
/// committed ontology.
fn vault_with_ontology(dir: &std::path::Path) {
    let person = dir.join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\ntitle: Ada\n---\nbody",
    )
    .unwrap();
}

/// The committed ontology: Person keyed by `title`, one RANGE index on
/// it, one VECTOR index with options on `embedding`, and a `summary`
/// property the file-edit test's extra index uses.
const ONTOLOGY: &str = r#"schema: https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json
id: vault-schema
version: "1"
nodes:
  person:
    label: Person
    properties:
      title: { type: STRING, key: true }
      embedding: { type: FLOAT }
      summary: { type: STRING }
    extensions:
      indexes:
        title_range:
          type: RANGE
          labels: [Person]
          properties: [title]
        embedding_vec:
          type: VECTOR
          labels: [Person]
          properties: [embedding]
          options:
            type: VECTOR
            vector.dimensions: 3
relationships:
  knows:
    type: KNOWS
    from: { node: person }
    to: { node: person }
"#;

fn opts() -> OpenOptions {
    OpenOptions {
        snapshot_codec: Some(SnapshotCodecKind::Markdown),
        ..OpenOptions::default()
    }
}

fn open(dir: &std::path::Path) -> Database {
    Database::open_with(dir.to_str().unwrap(), opts()).unwrap()
}

#[test]
fn the_committed_document_is_the_store_state_and_set_refuses() {
    preview_ontology();
    let dir = tempfile::tempdir().unwrap();
    vault_with_ontology(dir.path());
    std::fs::write(dir.path().join("ontology.yaml"), ONTOLOGY).unwrap();

    let db = open(dir.path());
    // `db.ontology.get` serves the committed document: canonical YAML
    // by the no-argument default (ADR-0227 §3), the canonical JSON on
    // request — the file's document either way.
    let mut conn = db.connect().unwrap();
    for (call, needle) in [
        (
            "CALL db.ontology.get() YIELD ontology RETURN ontology",
            "id: vault-schema",
        ),
        (
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology",
            "\"id\":\"vault-schema\"",
        ),
    ] {
        let row = conn.query(call).unwrap();
        let document = row.rows[0][0].as_string().expect("the document");
        assert!(
            document.contains(needle),
            "{call}: the served document is the file's, got {document}"
        );
    }

    // Set and drop refuse: the file is the document's source. The set
    // carries a valid document — the refusal is the committed-source
    // gate, not a validation error.
    let valid = concat!(
        "{\"schema\":\"https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json\",",
        "\"id\":\"other\",\"version\":\"1\"}"
    );
    for call in [
        format!("CALL db.ontology.set('{valid}')"),
        "CALL db.ontology.drop()".to_string(),
    ] {
        let refused = conn
            .query(&call)
            .expect_err("a file-backed store refuses document swaps")
            .to_string();
        assert!(
            refused.contains("committed ontology file"),
            "{call}: the refusal names the source, got {refused}"
        );
    }

    // The document survives the clean close (the no-op checkpoint
    // skip writes nothing) and the reopen re-applies it.
    drop(conn);
    drop(db);
    let db = open(dir.path());
    let row = db
        .connect()
        .unwrap()
        .query("CALL db.ontology.get('json') YIELD ontology RETURN ontology")
        .unwrap();
    assert!(row.rows[0][0].as_string().unwrap().contains("vault-schema"));
}

#[test]
fn the_committed_indexes_install_with_kinds_and_options() {
    let dir = tempfile::tempdir().unwrap();
    vault_with_ontology(dir.path());
    std::fs::write(dir.path().join("ontology.yaml"), ONTOLOGY).unwrap();

    let db = open(dir.path());
    let mut conn = db.connect().unwrap();
    let rows = conn
        .query(
            "SHOW INDEXES YIELD name, type, options \
             WHERE name IN ['title_range', 'embedding_vec'] \
             RETURN name, type, options",
        )
        .unwrap();
    let by_name: BTreeMap<String, Vec<Value>> = rows
        .rows
        .into_iter()
        .map(|mut row| {
            let name = row.remove(0).as_string().unwrap().to_string();
            (name, row)
        })
        .collect();
    let range = by_name
        .get("title_range")
        .expect("the file's RANGE index is installed");
    assert_eq!(range[0].as_string(), Some("RANGE"));
    let vector = by_name
        .get("embedding_vec")
        .expect("the file's VECTOR index is installed");
    assert_eq!(vector[0].as_string(), Some("VECTOR"));
    let options = vector[1].to_string();
    assert!(
        options.contains("3"),
        "the declared vector dimensions ride the options: {options}"
    );

    // The vector index is live AND its declared config is enforced:
    // an exact-dimension insert indexes and a search finds it...
    let hit = conn
        .query(
            "CREATE (p:Person {title: 'Grace', embedding: [0.1, 0.2, 0.3]}) \
             WITH p CALL db.index.vector.queryNodes('embedding_vec', 1, [0.1, 0.2, 0.3]) \
             YIELD node RETURN node.title AS title",
        )
        .unwrap();
    assert_eq!(hit.rows[0][0].as_string(), Some("Grace"));
    // ...and a mismatched-dimension query is refused with the engine's
    // own dimensionality guard — the file's `vector.dimensions: 3`
    // rides the stored indexConfig, not just the display.
    let refused = conn
        .query(
            "CALL db.index.vector.queryNodes('embedding_vec', 1, [0.1, 0.2]) YIELD node \
             RETURN node",
        )
        .expect_err("a mismatched dimension must be refused")
        .to_string();
    assert!(
        refused.contains("VectorIndexDimensionalityMismatch"),
        "the declared dimensions are enforced, got {refused}"
    );

    // Idempotence and convergence: reopen (the tail re-applies over
    // whatever the sidecar now carries) and the index is still there,
    // exactly once.
    drop(conn);
    drop(db);
    let db = open(dir.path());
    let rows = db
        .connect()
        .unwrap()
        .query("SHOW INDEXES YIELD name WHERE name = 'embedding_vec' RETURN count(*) AS n")
        .unwrap();
    assert_eq!(rows.rows[0][0], Value::Int(1));
}

#[test]
fn the_committed_graph_type_is_current() {
    let dir = tempfile::tempdir().unwrap();
    vault_with_ontology(dir.path());
    std::fs::write(dir.path().join("ontology.yaml"), ONTOLOGY).unwrap();

    let db = open(dir.path());
    let rows = db
        .connect()
        .unwrap()
        .query("SHOW CURRENT GRAPH TYPE YIELD specification RETURN specification")
        .unwrap();
    assert!(
        rows.rows[0][0]
            .as_string()
            .is_some_and(|spec| spec.contains("KNOWS")),
        "the file's converted graph type is the store's current one, \
         carrying the declared relationship type"
    );
}

#[test]
fn a_file_edit_never_reads_as_mirror_drift() {
    // The regression the open tail exists for: the schema text
    // mirror's open-time verification compares the checkpointed
    // state, so a committed change applied inside the codec's load
    // would refuse the next open. Edit the file — add an index —
    // between a checkpointed session and the next open, and prove
    // the vault still opens and the new index is live beside the old.
    let dir = tempfile::tempdir().unwrap();
    vault_with_ontology(dir.path());
    std::fs::write(dir.path().join("ontology.yaml"), ONTOLOGY).unwrap();
    let db = open(dir.path());
    // A write makes the exit checkpoint publish: the sidecar and the
    // schema mirror now carry the committed state as of this file.
    db.connect()
        .unwrap()
        .query("CREATE (:Person {title: 'Grace'})")
        .unwrap();
    drop(db);

    // The edit: a TEXT index on the `summary` property the document
    // declares (a different property from every existing index — the
    // engine's equivalent-schema rule refuses two single-property
    // indexes over the same pair).
    let edited = ONTOLOGY.replace(
        "        embedding_vec:",
        "        summary_text:\n          type: TEXT\n          labels: [Person]\n          properties: [summary]\n        embedding_vec:",
    );
    std::fs::write(dir.path().join("ontology.yaml"), edited).unwrap();

    let db = open(dir.path());
    let rows = db
        .connect()
        .unwrap()
        .query(
            "SHOW INDEXES YIELD name WHERE name IN ['summary_text', 'title_range'] \
             RETURN count(*) AS n",
        )
        .unwrap();
    assert_eq!(
        rows.rows[0][0],
        Value::Int(2),
        "the edited file's index is live beside the old one, and the open did not refuse"
    );
}

#[test]
fn a_file_edit_retargets_and_renames_held_indexes() {
    // The rust-review High: the committed application must override,
    // not collide — a machine whose sidecar holds the file's old index
    // (every machine that checkpointed before the edit) reopens over a
    // retargeted definition (same name, new property) and a renamed
    // one (new name, same pair) without the catalog's duplicate rules
    // failing the open. The file is the source and wins.
    let dir = tempfile::tempdir().unwrap();
    vault_with_ontology(dir.path());
    std::fs::write(dir.path().join("ontology.yaml"), ONTOLOGY).unwrap();
    let db = open(dir.path());
    db.connect()
        .unwrap()
        .query("CREATE (:Person {title: 'Grace'})")
        .unwrap();
    drop(db); // the exit checkpoint publishes: the sidecar now holds the old rows

    // Retarget title_range onto `summary`; rename embedding_vec (the
    // old name's row stays in the sidecar — the add-and-override
    // reach — while the new name takes the same indexed pair).
    let edited = ONTOLOGY
        .replace("properties: [title]", "properties: [summary]")
        .replace("embedding_vec:", "embedding_search:");
    std::fs::write(dir.path().join("ontology.yaml"), edited).unwrap();

    let db = open(dir.path());
    let rows = db
        .connect()
        .unwrap()
        .query(
            "SHOW INDEXES YIELD name, properties \
             WHERE name IN ['title_range', 'embedding_vec', 'embedding_search'] \
             RETURN name, properties ORDER BY name",
        )
        .unwrap();
    let rows = named_index_rows(rows.rows);
    assert_eq!(
        rows,
        vec![
            ("embedding_search".to_string(), "['embedding']".to_string()),
            ("title_range".to_string(), "['summary']".to_string()),
        ],
        "the retargeted definition is live, the stale name is gone, \
         and the open did not refuse: {rows:?}"
    );
}

#[test]
fn a_ddl_created_row_retires_through_its_encoded_pair() {
    // The round-4 pin: the catalog records `IndexDef.property` in the
    // legacy-encoded form every DDL create writes — the committed
    // override's pair comparison decodes the held row, and the
    // committed row records the same encoding, so a file index that
    // takes a DDL-created pair retires that row (a raw string
    // comparison would never match it) and the two producers install
    // byte-identical rows.
    let dir = tempfile::tempdir().unwrap();
    vault_with_ontology(dir.path());
    std::fs::write(dir.path().join("ontology.yaml"), ONTOLOGY).unwrap();
    let db = open(dir.path());
    db.connect()
        .unwrap()
        .query("CREATE (:Person {title: 'Grace'})")
        .unwrap();
    // A DDL-created row over (Person, summary) — the pair the file
    // edit below moves title_range onto.
    db.connect()
        .unwrap()
        .query("CREATE INDEX ddl_title FOR (p:Person) ON (p.summary)")
        .unwrap();
    drop(db); // the exit checkpoint publishes: the sidecar holds all rows

    // Retarget title_range onto `summary` — the DDL row's pair.
    let edited = ONTOLOGY.replace("properties: [title]", "properties: [summary]");
    std::fs::write(dir.path().join("ontology.yaml"), edited).unwrap();

    let db = open(dir.path());
    let rows = db
        .connect()
        .unwrap()
        .query(
            "SHOW INDEXES YIELD name, properties \
             WHERE name IN ['title_range', 'ddl_title'] RETURN name, properties ORDER BY name",
        )
        .unwrap();
    let rows = named_index_rows(rows.rows);
    assert_eq!(
        rows,
        vec![("title_range".to_string(), "['summary']".to_string())],
        "the DDL-created row retired through its encoded pair and the \
         retargeted definition is live: {rows:?}"
    );
}

#[test]
fn a_read_only_open_serves_the_committed_schema() {
    let dir = tempfile::tempdir().unwrap();
    vault_with_ontology(dir.path());
    std::fs::write(dir.path().join("ontology.yaml"), ONTOLOGY).unwrap();
    // A read-only open never creates a store: bootstrap once
    // (writable), then serve read-only.
    drop(open(dir.path()));
    let db = Database::open_read_only(dir.path().to_str().unwrap()).unwrap();
    let rows = db
        .connect()
        .unwrap()
        .query("SHOW INDEXES YIELD name WHERE name = 'title_range' RETURN count(*) AS n")
        .unwrap();
    assert_eq!(rows.rows[0][0], Value::Int(1));
}

/// The SHOW INDEXES rows as `(name, rendered properties)` pairs, in
/// the query's order — the shape every override test compares
/// against, mapped once.
fn named_index_rows(rows: Vec<Vec<Value>>) -> Vec<(String, String)> {
    rows.into_iter()
        .map(|mut row| {
            (
                row.remove(0).as_string().unwrap().to_string(),
                row.remove(0).to_string(),
            )
        })
        .collect()
}

#[test]
fn a_case_variant_root_label_round_trips_the_loads_identity() {
    // The round-6 blocker pin: a `:vault` (case-variant) node replacing
    // a root file's node plans through the root path — its vault key
    // must be the canonical reserved spelling the load derives
    // (`Vault/…`), never the node's observed case (`vault/…`), or the
    // next open's sidecar lookup misses, the node takes a fresh id,
    // and restored rel ids drift.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Ada.md"),
        "---\nname: Ada\n---\nroot note\n",
    )
    .unwrap();
    let db = open(dir.path());
    db.connect()
        .unwrap()
        .query("MATCH (n:Vault {name: 'Ada'}) DELETE n CREATE (:vault {name: 'Ada'})")
        .unwrap();
    // The id the replacement node carries at the end of the session —
    // fresh by construction (the original was deleted). The invariant
    // is that the reopen preserves it: the checkpoint plans the root
    // file under the load's canonical key (`Vault/Ada`), so the next
    // open's sidecar lookup restores this id — under the observed-case
    // spelling the lookup misses and the node takes another fresh id.
    let session_id: i64 = db
        .connect()
        .unwrap()
        .query("MATCH (n) WHERE n.name = 'Ada' RETURN id(n) AS id")
        .unwrap()
        .rows
        .remove(0)
        .remove(0)
        .as_int()
        .unwrap();
    drop(db); // the exit checkpoint publishes the round-trip
    let db = open(dir.path());
    let after: i64 = db
        .connect()
        .unwrap()
        .query("MATCH (n) WHERE n.name = 'Ada' RETURN id(n) AS id")
        .unwrap()
        .rows
        .remove(0)
        .remove(0)
        .as_int()
        .unwrap();
    assert_eq!(
        session_id, after,
        "the root node's id survived the case-variant round-trip"
    );
}

#[test]
fn a_label_that_sanitizes_into_the_reserved_root_directory_refuses() {
    // The round-10 pin: a non-root label whose *sanitized* directory
    // folds to the reserved root label (backticked `` `Vault ` `` —
    // sanitize trims the trailing space) would render into a `Vault/`
    // directory the next open refuses — a checkpoint-to-unopenable
    // loop. The plan refuses it instead, naming the label.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\ntitle: Ada\n---\nbody\n",
    )
    .unwrap();
    let db = open(dir.path());
    db.connect()
        .unwrap()
        .query("CREATE (:`Vault ` {name: 'Trapped'})")
        .unwrap();
    // The explicit checkpoint names the refusal; nothing was written
    // (the plan refuses before any render or publish).
    let refused = db.checkpoint().unwrap_err().to_string();
    assert!(
        refused.contains("sanitizes to the reserved root directory"),
        "the sanitizing label refuses the checkpoint: {refused}"
    );
    drop(db);
    // The vault is untouched and the open still works — the node is
    // still queryable, never silently rendered into the reserved root.
    let db = open(dir.path());
    let count = db
        .connect()
        .unwrap()
        .query("MATCH (n:`Vault `) RETURN count(*) AS n")
        .unwrap()
        .rows
        .remove(0)
        .remove(0)
        .as_int()
        .unwrap();
    assert_eq!(count, 1, "the node is still live after the refusal");
}
