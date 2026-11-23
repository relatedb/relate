//! The vault-committed ontology suite (ADR-0649): the reserved
//! non-`.md` document at the vault root declares the relationship
//! vocabulary and the per-label key properties, and its declarations
//! travel with the vault through version control the way the
//! per-machine sidecar cannot.

use super::open_markdown;
use relate_executor::Graph;

/// Open a markdown store at `dir`.
fn open(dir: &std::path::Path) -> Result<Graph, relate_core::Error> {
    open_markdown(dir).map(|(_, graph)| graph)
}

/// REL8-2039's shape: an ontology-declared rel type whose frontmatter
/// link lists carry **bare** wikilinks — the container-rewire shape of
/// the agent-memory vault conversion. `name` is the pinned key on every
/// label (the shape REL8-2022 verified).
const LEDGER_ONTOLOGY: &str = r#"schema: https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json
id: ledger-vault
version: "1"
nodes:
  todo:
    label: Todo
    properties:
      name: { type: STRING, key: true }
  ledger_record:
    label: LedgerRecord
    properties:
      name: { type: STRING, key: true }
  source:
    label: Source
    properties:
      name: { type: STRING, key: true }
relationships:
  part_of:
    type: PART_OF
    from: { node: todo }
    to: { node: source }
  sourced_from:
    type: SOURCED_FROM
    from: { node: ledger_record }
    to: { node: source }
"#;

/// The container-and-todo file set REL8-2039 reports: a Source
/// container, a Todo whose frontmatter carries a bare-wikilink PART_OF
/// list, and a LedgerRecord with a SOURCED_FROM list.
fn write_ledger_vault(dir: &std::path::Path) {
    let source = dir.join("Source");
    let todo = dir.join("Todo");
    let record = dir.join("LedgerRecord");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&todo).unwrap();
    std::fs::create_dir_all(&record).unwrap();
    std::fs::write(
        source.join("ledger_todos.md"),
        "---\ntype: Source\nname: ledger_todos\n---\n\n# ledger_todos\n",
    )
    .unwrap();
    std::fs::write(
        todo.join("task_1.md"),
        "---\ntype: Todo\nname: task_1\nPART_OF:\n  - \"[[Source/ledger_todos]]\"\n---\n\nfix the thing\n",
    )
    .unwrap();
    std::fs::write(
        record.join("rec_1.md"),
        "---\ntype: LedgerRecord\nname: rec_1\nSOURCED_FROM:\n  - \"[[Source/ledger_todos]]\"\n---\n\nsome entry\n",
    )
    .unwrap();
    std::fs::write(dir.join("ontology.yaml"), LEDGER_ONTOLOGY).unwrap();
}

#[test]
fn ontology_declared_bare_link_lists_survive_a_load_checkpoint_cycle() {
    let dir = tempfile::tempdir().unwrap();
    write_ledger_vault(dir.path());
    let (store, graph) = open_markdown(dir.path()).unwrap();
    let signature = super::signature(&graph);
    assert_eq!(
        signature.1,
        vec![
            (
                "LedgerRecord/rec_1".to_string(),
                "SOURCED_FROM".to_string(),
                "Source/ledger_todos".to_string()
            ),
            (
                "Todo/task_1".to_string(),
                "PART_OF".to_string(),
                "Source/ledger_todos".to_string()
            ),
        ],
        "the declared bare-wikilink lists load as relationships"
    );
    crate::checkpoint(&store, &graph, None).unwrap();
    let task = std::fs::read_to_string(dir.path().join("Todo/task_1.md")).unwrap();
    assert!(
        task.contains("PART_OF:"),
        "the todo file keeps its PART_OF list: {task}"
    );
    let (reloaded, _wal) = crate::open_store::<Graph>(&store).unwrap();
    let signature = super::signature(&reloaded);
    assert_eq!(
        signature.1,
        vec![
            (
                "LedgerRecord/rec_1".to_string(),
                "SOURCED_FROM".to_string(),
                "Source/ledger_todos".to_string()
            ),
            (
                "Todo/task_1".to_string(),
                "PART_OF".to_string(),
                "Source/ledger_todos".to_string()
            ),
        ],
        "the relationships survive the checkpoint round trip"
    );
}

/// The minimal valid 1.1.0 ontology declaring `KNOWS` and Person's key
/// property (`title`).
const PERSON_ONTOLOGY: &str = r#"schema: https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json
id: test-vault
version: "1"
nodes:
  person:
    label: Person
    properties:
      title: { type: STRING, key: true }
relationships:
  knows:
    type: KNOWS
    from: { node: person }
    to: { node: person }
"#;

#[test]
fn a_file_declared_relationship_type_declares_map_form_links() {
    // ADR-0406 §2's bootstrap ambiguity: a map-form link entry under a
    // key no bare wikilink anywhere declares refuses the open. The
    // committed ontology declares the type — the refusal is gone, and
    // the declaration travels with the vault.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    // Map-form KNOWS entries, no bare KNOWS declaration anywhere.
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\ntitle: Ada\nKNOWS:\n  - target: \"[[Person/Grace]]\"\n    how: ship\n---\n",
    )
    .unwrap();
    std::fs::write(
        person.join("Grace.md"),
        "---\ntype: Person\ntitle: Grace\n---\n",
    )
    .unwrap();

    // Without the ontology file: the undeclared map-form refusal.
    let refused = open(dir.path()).unwrap_err().to_string();
    assert!(
        refused.contains("no file declares the relationship type"),
        "the undeclared map-form entry refuses: {refused}"
    );

    // With the committed ontology declaring KNOWS: the vault opens and
    // the map-form entry is a relationship.
    std::fs::write(dir.path().join("ontology.yaml"), PERSON_ONTOLOGY).unwrap();
    let graph = open(dir.path()).unwrap();
    assert_eq!(graph.all_node_ids().len(), 2);
}

#[test]
fn a_file_declared_key_property_binds_where_inference_would_pick_another() {
    // The declaration is binding, not advisory: with the file naming
    // `title` as Person's key, a file whose `title` disagrees with its
    // filename refuses — where without the file the value-matching
    // inference would have picked the agreeing `name` and opened.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\nname: Ada\ntitle: Zed\n---\n",
    )
    .unwrap();

    // Without the ontology file: the inference picks `name` (its value
    // is the filename) and the vault opens.
    open(dir.path()).expect("inference picks the matching property");

    // With the ontology declaring `title` as the key: the declaration
    // binds — `title` is the filename-mirroring property, its value
    // disagrees, and the open refuses naming it.
    std::fs::write(dir.path().join("ontology.yaml"), PERSON_ONTOLOGY).unwrap();
    let refused = open(dir.path()).unwrap_err().to_string();
    assert!(
        refused.contains("key property 'title' value 'Zed' does not match its filename 'Ada'"),
        "the declared key binds: {refused}"
    );
}

#[test]
fn both_ontology_spellings_refuse() {
    // One committed ontology, one spelling: both present is an
    // ambiguous source, refused before either is read.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\ntitle: Ada\n---\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("ontology.yaml"), PERSON_ONTOLOGY).unwrap();
    std::fs::write(dir.path().join("ontology.json"), "{\"schema\": \"https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json\", \"id\": \"x\", \"version\": \"1\"}").unwrap();
    let refused = open(dir.path()).unwrap_err().to_string();
    assert!(
        refused.contains("both ontology.yaml and ontology.json"),
        "the ambiguous pair refuses: {refused}"
    );
}

#[test]
fn an_invalid_ontology_document_refuses_naming_the_file() {
    // The file is untrusted input: a document that does not validate
    // against the pinned spec (here: the wrong schema URI) fails the
    // open naming the file and the violation.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\ntitle: Ada\n---\n",
    )
    .unwrap();
    let invalid = PERSON_ONTOLOGY.replace(
        "https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json",
        "https://example.com/not-the-spec.json",
    );
    std::fs::write(dir.path().join("ontology.yaml"), invalid).unwrap();
    let refused = open(dir.path()).unwrap_err().to_string();
    assert!(
        refused.contains("ontology file ontology.yaml"),
        "the refusal names the file: {refused}"
    );
}

#[test]
fn an_unconvertible_ontology_document_refuses() {
    // The expressibility gate: a spec-valid document whose graph type
    // ADR-0226's converter cannot express (a node entry with no
    // identifying label) refuses the open — a file that claims to be
    // the store's schema must be a schema the engine can hold.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\ntitle: Ada\n---\n",
    )
    .unwrap();
    // `label: null` is main-valid; the converter refuses it.
    let unconvertible = PERSON_ONTOLOGY.replace("label: Person", "label: null");
    std::fs::write(dir.path().join("ontology.yaml"), unconvertible).unwrap();
    let refused = open(dir.path()).unwrap_err().to_string();
    assert!(
        refused.contains("ontology file ontology.yaml"),
        "the unconvertible document refuses naming the file: {refused}"
    );
}

#[test]
fn a_label_declaration_binds_through_the_identity_fold() {
    // The declaration's label is resolved against the walked labels
    // through the vault's one identity relation (the case fold,
    // ADR-0647): `label: person` binds the `Person/` directory's
    // files, it does not silently miss them.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    // `name` matches the filename; a `title` that does not — with the
    // fold binding, the declared `title` key refuses the open (the
    // binding test's shape, through the case fold).
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\nname: Ada\ntitle: Zed\n---\n",
    )
    .unwrap();
    let ontology = PERSON_ONTOLOGY.replace("label: Person", "label: person");
    std::fs::write(dir.path().join("ontology.yaml"), ontology).unwrap();
    let refused = open(dir.path()).unwrap_err().to_string();
    assert!(
        refused.contains("key property 'title' value 'Zed' does not match its filename 'Ada'"),
        "the case-differing declaration binds, got {refused}"
    );
}

#[test]
fn a_multi_label_index_declaration_refuses() {
    // The catalog row holds one label (and one property): a
    // multi-label entry is refused at the extraction, naming the file
    // and the index — the same refusal shape the multi-property entry
    // takes — never left for the open tail's boundary check to
    // refuse without the file's name.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\ntitle: Ada\n---\n",
    )
    .unwrap();
    let multi = PERSON_ONTOLOGY.replace(
        "relationships:",
        "    extensions:\n      indexes:\n        title_range:\n          type: RANGE\n          labels: [Person, Company]\n          properties: [title]\nrelationships:",
    );
    std::fs::write(dir.path().join("ontology.yaml"), multi).unwrap();
    let refused = open(dir.path()).unwrap_err().to_string();
    assert!(
        refused.contains("ontology file ontology.yaml")
            && refused.contains("must declare exactly one label"),
        "the multi-label entry refuses naming the file: {refused}"
    );
}
