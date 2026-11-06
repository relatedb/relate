//! The `.cypherlite/schema/` text mirror (ADR-0481): every checkpoint of
//! the native (Parquet) and JSON codecs writes the three-file contract —
//! the canonical graph type body, the verbatim ontology document, and
//! the SHOW-rendered catalog DDL — and clears the stale spellings a
//! previous checkpoint left behind. The open-time drift guard
//! (ADR-0481 §4) verifies the same files against the state the open
//! restored: an untouched checkpointed store opens clean, a drifted
//! file refuses the open naming itself, and an absent folder is a
//! pre-ADR store that opens unchanged.

use super::*;
use relate_executor::{Graph, IndexDetailSpec, IndexKindSpec, RedoOp};

/// The store's committed schema state one checkpoint persists: a graph
/// type on its own label, two named indexes with their typed details,
/// and one named existence constraint — installed through the redo ops a
/// WAL replay of the same DDL would apply, with a graph type the named
/// catalog does not touch (so the constraint stays independent of it).
fn schema_graph() -> Graph {
    let graph = Graph::default();
    let index_detail = |name: &str, kind, label: &str, property: &str| {
        vec![
            RedoOp::CreateIndex {
                name: name.into(),
                label: label.into(),
                property: property.into(),
            },
            RedoOp::CatalogIndexDetail {
                name: name.into(),
                detail: IndexDetailSpec {
                    kind,
                    relationship: false,
                    labels: vec![label.into()],
                    properties: vec![property.into()],
                    with_properties: vec![],
                },
                options: serde_json::json!({}),
            },
        ]
    };
    graph
        .apply_redo(
            index_detail("person_name", IndexKindSpec::Range, "Person", "name")
                .into_iter()
                .chain(index_detail(
                    "book_title",
                    IndexKindSpec::Text,
                    "Book",
                    "title",
                ))
                .chain(std::iter::once(RedoOp::create_constraint(ConstraintDef {
                    name: "person_email".into(),
                    relationship: false,
                    label: "Person".into(),
                    properties: vec!["email".into()],
                    kind: "exists".into(),
                    property_type: None,
                    enforced_label: None,
                })))
                .collect(),
            1,
        )
        .unwrap();
    graph
        .restore_graph_type_spec(Some("(:Page => {url :: STRING})"))
        .unwrap();
    graph
}

/// The pinned `schema.cypher` body for [`schema_graph`]: one statement
/// per catalog entry, in the catalog's name order (the order the
/// checkpoint manifest persists), each the SHOW `createStatement` render.
const PINNED_DDL: &str = concat!(
    "CREATE TEXT INDEX `book_title` FOR (n:`Book`) ON (n.`title`);\n",
    "CREATE RANGE INDEX `person_name` FOR (n:`Person`) ON (n.`name`);\n",
    "CREATE CONSTRAINT `person_email` FOR (n:`Person`) REQUIRE (n.`email`) IS NOT NULL;\n",
);

/// A strict-JSON ontology document (the `.json` mirror spelling).
const ONTOLOGY_JSON: &str = "{\"id\":\"example\",\"version\":\"1\"}";

use crate::schema_files::{
    GRAPH_TYPE_FILE, ONTOLOGY_JSON_FILE, ONTOLOGY_YAML_FILE, SCHEMA_DDL_FILE, SCHEMA_DIR,
};

/// The mirror spells its directory under the `.cypherlite` engine
/// directory name the markdown codec's sidecar family owns — one fact,
/// two constants, pinned together so they cannot drift — and every
/// contract file sits inside that directory.
#[test]
fn schema_dir_sits_under_the_engine_dir() {
    assert_eq!(
        crate::schema_files::SCHEMA_DIR.strip_suffix("/schema"),
        Some(crate::codec::markdown::ENGINE_DIR)
    );
    for file in [
        GRAPH_TYPE_FILE,
        ONTOLOGY_JSON_FILE,
        ONTOLOGY_YAML_FILE,
        SCHEMA_DDL_FILE,
    ] {
        assert!(
            file.starts_with(crate::schema_files::SCHEMA_DIR),
            "{file} sits outside the schema mirror directory"
        );
    }
}

/// ADR-0481 acceptance 1: a native (Parquet) store checkpointed with a
/// graph type, an ontology document, two indexes and one constraint
/// carries the three-file mirror — graph-type.cypher byte-identical to
/// the canonical form, ontology.json byte-identical to the published
/// `ontology/<hex>.json` object, schema.cypher with exactly one SHOW
/// createStatement per catalog entry.
#[cfg(parquet_codec)]
#[test]
fn native_checkpoint_writes_the_schema_text_mirror() {
    let dir = tempfile::tempdir().unwrap();
    let store = parse_uri(dir.path().join("db").to_str().unwrap()).unwrap();
    let graph = schema_graph();
    checkpoint_with_lsn(&store, &graph, 1, Some(ONTOLOGY_JSON)).unwrap();

    // graph-type.cypher: the canonical body, byte-identical.
    let canonical = graph.graph_type_spec().unwrap().unwrap();
    assert_eq!(
        snapshot::get_object(&store, GRAPH_TYPE_FILE).unwrap(),
        canonical.as_bytes()
    );

    // ontology.json: byte-identical to the object the checkpoint
    // published (and to the document itself — no re-serialization).
    let manifest = snapshot::native::read_manifest_for_test(&store)
        .unwrap()
        .unwrap();
    let published = snapshot::get_object(
        &store,
        manifest.ontology_path.as_deref().expect("v4 manifest"),
    )
    .unwrap();
    let mirrored = snapshot::get_object(&store, ONTOLOGY_JSON_FILE).unwrap();
    assert_eq!(mirrored, published);
    assert_eq!(mirrored, ONTOLOGY_JSON.as_bytes());
    assert!(!snapshot::object_exists(&store, ONTOLOGY_YAML_FILE).unwrap());

    // schema.cypher: exactly one statement per catalog entry, each the
    // SHOW createStatement render.
    let ddl = String::from_utf8(snapshot::get_object(&store, SCHEMA_DDL_FILE).unwrap()).unwrap();
    assert_eq!(ddl, PINNED_DDL);
    assert_eq!(
        ddl.lines().count(),
        graph.named_indexes().len() + graph.named_constraints().len()
    );
}

/// ADR-0481 acceptance 2: the JSON codec's dump writes the same
/// three-file mirror, its graph type byte-identical to the `graph.json`
/// body the codec carries.
#[test]
fn json_checkpoint_writes_the_same_schema_text_mirror() {
    let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    let graph = schema_graph();
    checkpoint_with_lsn(&store, &graph, 1, Some(ONTOLOGY_JSON)).unwrap();

    // graph-type.cypher: byte-identical to the `graph_type` field the
    // JSON snapshot persisted.
    let body: serde_json::Value = serde_json::from_slice(
        &snapshot::get_object(&store, crate::codec::json::GRAPH_JSON).unwrap(),
    )
    .unwrap();
    let carried = body["graph_type"].as_str().unwrap();
    let mirrored =
        String::from_utf8(snapshot::get_object(&store, GRAPH_TYPE_FILE).unwrap()).unwrap();
    assert_eq!(mirrored, carried);
    assert_eq!(mirrored, graph.graph_type_spec().unwrap().unwrap());

    // ontology.json: the document verbatim.
    assert_eq!(
        snapshot::get_object(&store, ONTOLOGY_JSON_FILE).unwrap(),
        ONTOLOGY_JSON.as_bytes()
    );
    assert!(!snapshot::object_exists(&store, ONTOLOGY_YAML_FILE).unwrap());

    // schema.cypher: the same one-statement-per-entry DDL.
    let ddl = String::from_utf8(snapshot::get_object(&store, SCHEMA_DDL_FILE).unwrap()).unwrap();
    assert_eq!(ddl, PINNED_DDL);
}

/// ADR-0481 acceptance 5, markdown leg: the vault's checkpoint writes the
/// same three-file mirror beside its `.cypherlite/manifest.json` sidecar
/// — `graph-type.cypher` byte-equal to the flattened `CatalogWire` text
/// the sidecar carries, `ontology.json` byte-verbatim when the sidecar
/// records an ontology document, `schema.cypher` always present — and a
/// follow-up checkpoint without the ontology clears its mirror the same
/// way the sidecar drops the field, without touching the other two.
#[test]
fn markdown_checkpoint_writes_the_schema_text_mirror_beside_the_sidecar() {
    let store = memory_store_with_codec(SnapshotCodecKind::Markdown).unwrap();
    let graph = schema_graph();
    checkpoint_with_lsn(&store, &graph, 1, Some(ONTOLOGY_JSON)).unwrap();

    // The sidecar the checkpoint just published, parsed as the JSON it
    // is: the flattened wire carrier spells `graph_type` and `ontology`
    // inline, so the mirror is compared against exactly what the sidecar
    // carries — never a second rendering of the graph.
    let sidecar: serde_json::Value = serde_json::from_slice(
        &snapshot::get_object(&store, crate::codec::markdown::MANIFEST).unwrap(),
    )
    .unwrap();

    // graph-type.cypher: byte-equal to the sidecar's graph_type text.
    let carried = sidecar["graph_type"].as_str().unwrap();
    let mirrored =
        String::from_utf8(snapshot::get_object(&store, GRAPH_TYPE_FILE).unwrap()).unwrap();
    assert_eq!(mirrored, carried);
    assert_eq!(mirrored, graph.graph_type_spec().unwrap().unwrap());
    // ADR-0481 §1's re-parse contract, on the exact bytes the vault's
    // mirror carries: the body parses through the parser's own
    // graph-type-body entry point.
    relate_parser::parse_graph_type_body(&mirrored).unwrap();

    // ontology.json: the document byte-verbatim, exactly the sidecar's
    // `ontology` string, and never the other spelling.
    assert_eq!(
        snapshot::get_object(&store, ONTOLOGY_JSON_FILE).unwrap(),
        ONTOLOGY_JSON.as_bytes()
    );
    assert_eq!(sidecar["ontology"].as_str().unwrap(), ONTOLOGY_JSON);
    assert!(!snapshot::object_exists(&store, ONTOLOGY_YAML_FILE).unwrap());

    // schema.cypher: present, the same one-statement-per-entry DDL.
    let ddl = String::from_utf8(snapshot::get_object(&store, SCHEMA_DDL_FILE).unwrap()).unwrap();
    assert_eq!(ddl, PINNED_DDL);

    // An unchanged re-checkpoint rewrites the mirror byte-identically
    // (the idempotent re-checkpoint contract, over the mirror itself).
    let mirror = |store: &StoreTarget| {
        (
            snapshot::get_object(store, GRAPH_TYPE_FILE).unwrap(),
            snapshot::get_object(store, ONTOLOGY_JSON_FILE).unwrap(),
            snapshot::get_object(store, SCHEMA_DDL_FILE).unwrap(),
        )
    };
    let before = mirror(&store);
    checkpoint_with_lsn(&store, &graph, 2, Some(ONTOLOGY_JSON)).unwrap();
    assert_eq!(before, mirror(&store));

    // A checkpoint without the ontology clears its mirror the way the
    // sidecar drops the field; the graph type and the DDL stay.
    checkpoint_with_lsn(&store, &graph, 3, None).unwrap();
    assert!(!snapshot::object_exists(&store, ONTOLOGY_JSON_FILE).unwrap());
    assert!(!snapshot::object_exists(&store, ONTOLOGY_YAML_FILE).unwrap());
    let sidecar: serde_json::Value = serde_json::from_slice(
        &snapshot::get_object(&store, crate::codec::markdown::MANIFEST).unwrap(),
    )
    .unwrap();
    assert!(sidecar.get("ontology").is_none());
    assert_eq!(
        String::from_utf8(snapshot::get_object(&store, GRAPH_TYPE_FILE).unwrap()).unwrap(),
        carried
    );
    assert!(snapshot::object_exists(&store, SCHEMA_DDL_FILE).unwrap());
}

/// The mirror is rewritten, not appended: a YAML document switches the
/// ontology spelling (and clears the `.json` one), a removed ontology
/// clears both, a dropped graph type clears its file, and the DDL file
/// stays present — empty — for a schema-less store (ADR-0481 §1–§3).
#[test]
fn schema_mirror_clears_stale_files_and_follows_the_document_format() {
    let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    let graph = schema_graph();
    let yaml = "id: example\nversion: \"1\"\n";

    checkpoint_with_lsn(&store, &graph, 1, Some(ONTOLOGY_JSON)).unwrap();
    assert!(snapshot::object_exists(&store, GRAPH_TYPE_FILE).unwrap());

    // A YAML document takes the `.yaml` spelling and clears `.json`.
    checkpoint_with_lsn(&store, &graph, 2, Some(yaml)).unwrap();
    assert_eq!(
        snapshot::get_object(&store, ONTOLOGY_YAML_FILE).unwrap(),
        yaml.as_bytes()
    );
    assert!(!snapshot::object_exists(&store, ONTOLOGY_JSON_FILE).unwrap());

    // No ontology clears both spellings.
    checkpoint_with_lsn(&store, &graph, 3, None).unwrap();
    assert!(!snapshot::object_exists(&store, ONTOLOGY_JSON_FILE).unwrap());
    assert!(!snapshot::object_exists(&store, ONTOLOGY_YAML_FILE).unwrap());
    assert!(snapshot::object_exists(&store, SCHEMA_DDL_FILE).unwrap());

    // A dropped graph type clears its file; a schema-less store keeps an
    // empty — present — schema.cypher.
    graph
        .apply_redo(
            vec![RedoOp::DropIndex {
                name: "person_name".into(),
            }],
            4,
        )
        .unwrap();
    graph
        .apply_redo(
            vec![
                RedoOp::DropIndex {
                    name: "book_title".into(),
                },
                RedoOp::DropConstraint {
                    name: "person_email".into(),
                },
            ],
            5,
        )
        .unwrap();
    graph
        .apply_redo(
            vec![RedoOp::SetCurrentGraphType {
                body: Default::default(),
            }],
            6,
        )
        .unwrap();
    checkpoint_with_lsn(&store, &graph, 7, None).unwrap();
    assert!(!snapshot::object_exists(&store, GRAPH_TYPE_FILE).unwrap());
    assert_eq!(
        snapshot::get_object(&store, SCHEMA_DDL_FILE).unwrap(),
        Vec::<u8>::new()
    );
}

/// Checkpoint [`schema_graph`] with its ontology at LSN 1 over one
/// committed data record — the way a real store reaches a checkpointed
/// state — and return the graph the checkpoint persisted. The WAL
/// handle is closed, so the open paths under test open their own.
fn checkpointed_store(store: &StoreTarget) -> Graph {
    let (vfs, prefix) = store.wal_vfs();
    let mut wal = Wal::open(vfs, prefix, 0).unwrap();
    wal.append_commit(
        1,
        &[RedoOp::CreateNode {
            id: 0,
            labels: vec!["Seed".into()].into(),
            properties: Default::default(),
        }],
    )
    .unwrap();
    let graph = schema_graph();
    checkpoint_with_lsn(store, &graph, 1, Some(ONTOLOGY_JSON)).unwrap();
    graph
}

/// The one refusal assertion every drift test makes: the open fails
/// with the authored error naming the drifted file and the repair.
fn refuses_open_naming(store: &StoreTarget, file: &str) {
    let Err(error) = open_store::<relate_executor::Graph>(store) else {
        panic!("a drifted {file} opened the store")
    };
    let text = error.to_string();
    assert!(text.contains(file), "expected {file} in: {text}");
    assert!(text.contains(".cypherlite/schema"), "repair in: {text}");
}

/// The store targets every cross-codec open test runs: the JSON-codec
/// memory VFS store, plus the native (Parquet) store rooted at `dir`'s
/// `db` when that codec is compiled in. The caller owns `dir` and keeps
/// it alive for the whole test — the native store's files live inside
/// it, so a dropped `TempDir` deletes the store under test.
#[cfg_attr(not(parquet_codec), allow(unused_variables))]
fn stores_under_test(dir: &std::path::Path) -> Vec<StoreTarget> {
    [
        memory_store_with_codec(SnapshotCodecKind::Json).unwrap(),
        #[cfg(parquet_codec)]
        parse_uri(dir.join("db").to_str().unwrap()).unwrap(),
    ]
    .into_iter()
    .collect()
}

/// ADR-0481 §4 acceptance 3: an untouched checkpointed store opens
/// clean — writable and read-only — under both the native (Parquet) and
/// the JSON codec, the loaded state matching what the checkpoint
/// persisted.
#[test]
fn untouched_checkpointed_store_opens_clean() {
    let dir = tempfile::tempdir().unwrap();
    for store in stores_under_test(dir.path()) {
        let graph = checkpointed_store(&store);
        let (opened, wal) = open_store::<relate_executor::Graph>(&store).unwrap();
        assert_eq!(
            opened.graph_type_spec().unwrap(),
            graph.graph_type_spec().unwrap(),
            "graph type survived the guarded open"
        );
        assert_eq!(opened.named_indexes().len(), 2);
        assert_eq!(opened.named_constraints().len(), 1);
        drop((opened, wal));
        let (readonly, wal) = open_store_readonly::<relate_executor::Graph>(&store).unwrap();
        assert_eq!(readonly.named_constraints().len(), 1);
        drop((readonly, wal));
    }
}

/// ADR-0481 §4 acceptance 1: a hand-edited
/// `.cypherlite/schema/graph-type.cypher` fails the open closed with
/// the authored error naming that file — the writable and the read-only
/// open alike — and the documented repair (delete the folder, reopen,
/// let the next checkpoint regenerate the mirror) restores the store.
#[cfg(parquet_codec)]
#[test]
fn hand_edited_graph_type_mirror_refuses_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = parse_uri(dir.path().join("db").to_str().unwrap()).unwrap();
    let graph = checkpointed_store(&store);
    let root = store.filesystem_dir().unwrap().to_path_buf();
    std::fs::write(root.join(GRAPH_TYPE_FILE), b"(:Page => {url :: INTEGER})").unwrap();

    refuses_open_naming(&store, "graph-type.cypher");
    let Err(error) = open_store_readonly::<relate_executor::Graph>(&store) else {
        panic!("a drifted graph-type.cypher opened the store read-only")
    };
    assert!(error.to_string().contains("graph-type.cypher"), "{error}");

    // The repair: the folder's absence is a pre-ADR-0481 store, the open
    // proceeds, and the next checkpoint regenerates the mirror.
    std::fs::remove_dir_all(root.join(crate::schema_files::SCHEMA_DIR)).unwrap();
    let (opened, wal) = open_store::<relate_executor::Graph>(&store).unwrap();
    assert_eq!(
        opened.graph_type_spec().unwrap(),
        graph.graph_type_spec().unwrap()
    );
    checkpoint_with_lsn(&store, &opened, wal.lsn, Some(ONTOLOGY_JSON)).unwrap();
    drop((opened, wal));
    assert_eq!(
        snapshot::get_object(&store, GRAPH_TYPE_FILE).unwrap(),
        graph.graph_type_spec().unwrap().unwrap().as_bytes()
    );
    open_store::<relate_executor::Graph>(&store).unwrap();
}

/// ADR-0481 §4, the markdown-vault leg: the guard the classic codecs'
/// tests exercise holds over a real vault directory too — a hand-edited
/// `graph-type.cypher` or `schema.cypher` under `.cypherlite/schema/`
/// refuses the open (writable and read-only alike) naming that file, and
/// the documented repair — deleting the folder, the pre-ADR-0481 shape —
/// restores the open, the next checkpoint regenerating the mirror beside
/// the vault's sidecar.
#[test]
fn markdown_vault_guard_refuses_a_hand_edited_mirror_and_repairs_by_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let store = parse_uri_with_options(
        dir.path().to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Markdown),
    )
    .unwrap();
    let graph = checkpointed_store(&store);
    let root = store.filesystem_dir().unwrap().to_path_buf();

    // A hand-edited graph-type.cypher refuses the open naming itself,
    // restored it opens clean again.
    let canonical = graph.graph_type_spec().unwrap().unwrap();
    std::fs::write(root.join(GRAPH_TYPE_FILE), b"(:Page => {url :: INTEGER})").unwrap();
    refuses_open_naming(&store, "graph-type.cypher");
    std::fs::write(root.join(GRAPH_TYPE_FILE), canonical.as_bytes()).unwrap();
    drop(open_store::<relate_executor::Graph>(&store).unwrap());

    // A hand-edited schema.cypher refuses both opens naming the file's
    // path, with every durable byte the checkpoint wrote untouched.
    std::fs::write(
        root.join(SCHEMA_DDL_FILE),
        PINNED_DDL.replace("person_name", "renamed").as_bytes(),
    )
    .unwrap();
    refuses_open_naming(&store, SCHEMA_DDL_FILE);
    let Err(error) = open_store_readonly::<relate_executor::Graph>(&store) else {
        panic!("a drifted schema.cypher opened the vault read-only")
    };
    assert!(error.to_string().contains(SCHEMA_DDL_FILE), "{error}");

    // The repair: the folder's absence is the pre-ADR-0481 vault shape,
    // the open proceeds, and the next checkpoint regenerates the mirror
    // beside the sidecar.
    std::fs::remove_dir_all(root.join(crate::schema_files::SCHEMA_DIR)).unwrap();
    let (opened, wal) = open_store::<relate_executor::Graph>(&store).unwrap();
    assert_eq!(
        opened.graph_type_spec().unwrap(),
        graph.graph_type_spec().unwrap(),
        "graph type survived the guarded vault open"
    );
    assert_eq!(opened.named_indexes().len(), 2);
    checkpoint_with_lsn(&store, &opened, wal.lsn, Some(ONTOLOGY_JSON)).unwrap();
    drop((opened, wal));
    assert_eq!(
        snapshot::get_object(&store, SCHEMA_DDL_FILE).unwrap(),
        PINNED_DDL.as_bytes()
    );
    open_store::<relate_executor::Graph>(&store).unwrap();
}

/// ADR-0481 §4 acceptance 2: a store whose whole `.cypherlite/schema/`
/// folder is gone — the pre-ADR-0481 shape, or the documented repair —
/// opens with no refusal and no regeneration until the next checkpoint.
#[test]
fn absent_schema_folder_opens_an_older_store() {
    let dir = tempfile::tempdir().unwrap();
    for store in stores_under_test(dir.path()) {
        let graph = checkpointed_store(&store);
        // The documented repair, through the VFS's own operation — one
        // call yields the same absence on every backend.
        store.vfs.remove_dir_all(SCHEMA_DIR).unwrap();
        let (opened, wal) = open_store::<relate_executor::Graph>(&store).unwrap();
        assert_eq!(
            opened.graph_type_spec().unwrap(),
            graph.graph_type_spec().unwrap()
        );
        assert_eq!(opened.named_constraints().len(), 1);
        drop((opened, wal));
        // Nothing regenerates before a checkpoint.
        assert!(!snapshot::object_exists(&store, SCHEMA_DDL_FILE).unwrap());
    }
}

/// ADR-0481 §4 acceptance 3, the drift matrix: a changed, missing, or
/// extra mirror file — the catalog DDL, the ontology document, a stale
/// other-extension ontology spelling, a stray file — each refuses the
/// open naming itself, and removing the drift restores the open.
#[test]
fn drifted_mirror_files_refuse_the_open_naming_the_file() {
    let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    checkpointed_store(&store);
    let ddl = snapshot::get_object(&store, SCHEMA_DDL_FILE).unwrap();
    let restore = || snapshot::put_object(&store, SCHEMA_DDL_FILE, &ddl).unwrap();

    // Changed catalog DDL.
    snapshot::put_object(
        &store,
        SCHEMA_DDL_FILE,
        PINNED_DDL.replace("book", "renamed").as_bytes(),
    )
    .unwrap();
    refuses_open_naming(&store, SCHEMA_DDL_FILE);
    restore();

    // Missing catalog DDL.
    store.vfs.remove_file(SCHEMA_DDL_FILE).unwrap();
    refuses_open_naming(&store, SCHEMA_DDL_FILE);
    restore();

    // Changed ontology document.
    snapshot::put_object(&store, ONTOLOGY_JSON_FILE, b"{\"id\":\"other\"}").unwrap();
    refuses_open_naming(&store, ONTOLOGY_JSON_FILE);
    snapshot::put_object(&store, ONTOLOGY_JSON_FILE, ONTOLOGY_JSON.as_bytes()).unwrap();

    // A stale other-extension ontology spelling beside the live one.
    snapshot::put_object(&store, ONTOLOGY_YAML_FILE, b"id: stale\n").unwrap();
    refuses_open_naming(&store, ONTOLOGY_YAML_FILE);
    store.vfs.remove_file(ONTOLOGY_YAML_FILE).unwrap();

    // An extra unexpected file.
    let stray = format!("{SCHEMA_DIR}/notes.txt");
    snapshot::put_object(&store, &stray, b"stray").unwrap();
    refuses_open_naming(&store, &stray);
    store.vfs.remove_file(&stray).unwrap();

    // Every drift removed, the untouched mirror opens clean again.
    let (opened, wal) = open_store::<relate_executor::Graph>(&store).unwrap();
    drop((opened, wal));
}

/// The guard's mirror reads are bounded like every other untrusted-VFS
/// read in the crate: a planted `.cypherlite/schema/graph-type.cypher`
/// past the family's 16 MiB class — written straight through the VFS,
/// past the checkpoint object path's own bound check — refuses the open
/// with the bounded-read class's limit error naming the file, in the
/// writable and the read-only open alike, never an unbounded allocation
/// that OOMs every `Database::open` (a shared serve process included).
#[test]
fn oversized_mirror_file_refuses_the_open_through_the_bounded_read() {
    let dir = tempfile::tempdir().unwrap();
    let oversized = vec![b'x'; snapshot::MAX_JSON_OBJECT as usize + 1];
    for store in stores_under_test(dir.path()) {
        checkpointed_store(&store);
        crate::vfs::atomic_write(&*store.vfs, GRAPH_TYPE_FILE, &oversized).unwrap();

        for open in [
            open_store::<relate_executor::Graph>(&store),
            open_store_readonly::<relate_executor::Graph>(&store),
        ] {
            let Err(error) = open else {
                panic!("an oversized graph-type.cypher opened the store")
            };
            let text = error.to_string();
            assert!(text.contains(GRAPH_TYPE_FILE), "names the file in: {text}");
            assert!(text.contains("16 MiB"), "names the bound in: {text}");
        }
    }
}

/// A mirror entry whose name carries control characters — the
/// newline/ANSI-escape spoofing vector — refuses the open with the name
/// escaped: no raw control character reaches the error text that CLI
/// stderr, serve logs, and client bodies carry.
#[test]
fn control_character_mirror_entry_name_is_escaped_in_the_drift_error() {
    let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    checkpointed_store(&store);
    let planted = format!("{SCHEMA_DIR}/esc\u{1b}[31mfake\nerror.txt");
    snapshot::put_object(&store, &planted, b"stray").unwrap();

    let Err(error) = open_store::<relate_executor::Graph>(&store) else {
        panic!("a control-character mirror entry opened the store")
    };
    let text = error.to_string();
    assert!(
        text.contains("is not part of this store's schema mirror"),
        "drift verdict in: {text}"
    );
    assert_planted_entry_name_escaped(&text);
}

/// The escape contract's one verdict over a drift error that names the
/// planted `esc\u{1b}[31mfake\nerror.txt` entry: no raw control
/// character survives, and the name reads in its escaped spelling.
fn assert_planted_entry_name_escaped(text: &str) {
    assert!(!text.contains('\u{1b}'), "raw ESC reached: {text:?}");
    assert!(!text.contains('\n'), "raw newline reached: {text:?}");
    assert!(
        text.contains(r"esc\u{1b}[31mfake\nerror.txt"),
        "escaped name in: {text}"
    );
}

/// The real-filesystem leg of the escape contract: a control-character
/// entry planted on disk — creatable on every Unix filesystem — refuses
/// the open with its name escaped in the drift error the same way.
#[cfg(all(parquet_codec, unix))]
#[test]
fn control_character_mirror_entry_on_disk_is_escaped_in_the_drift_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = parse_uri(dir.path().join("db").to_str().unwrap()).unwrap();
    checkpointed_store(&store);
    let root = store.filesystem_dir().unwrap().to_path_buf();
    std::fs::write(
        root.join(SCHEMA_DIR).join("esc\u{1b}[31mfake\nerror.txt"),
        b"stray",
    )
    .unwrap();

    let Err(error) = open_store::<relate_executor::Graph>(&store) else {
        panic!("an on-disk control-character mirror entry opened the store")
    };
    assert_planted_entry_name_escaped(&error.to_string());
}

/// The guard's comparison basis is the checkpoint the manifest names,
/// never the post-replay state (ADR-0481 §4's authority model): schema
/// commits past the checkpoint LSN legitimately run ahead of the
/// mirror, so they open clean — and the next checkpoint brings the
/// mirror up to the state they produced.
#[test]
fn wal_pending_schema_changes_are_not_mirror_drift() {
    let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    checkpointed_store(&store);
    let (vfs, prefix) = store.wal_vfs();
    let mut wal = Wal::open(vfs, prefix, 1).unwrap();
    wal.append_commit(
        2,
        &[RedoOp::create_constraint(ConstraintDef {
            name: "late_rule".into(),
            relationship: false,
            label: "Book".into(),
            properties: vec!["isbn".into()],
            kind: "exists".into(),
            property_type: None,
            enforced_label: None,
        })],
    )
    .unwrap();
    wal.sync().unwrap();
    drop(wal);

    let (loaded, wal) = open_store::<relate_executor::Graph>(&store).unwrap();
    assert_eq!(loaded.named_constraints().len(), 2);
    checkpoint_with_lsn(&store, &loaded, wal.lsn, Some(ONTOLOGY_JSON)).unwrap();
    drop((loaded, wal));
    let ddl = String::from_utf8(snapshot::get_object(&store, SCHEMA_DDL_FILE).unwrap()).unwrap();
    assert!(ddl.contains("late_rule"), "{ddl}");
    let (reopened, wal) = open_store::<relate_executor::Graph>(&store).unwrap();
    assert_eq!(reopened.named_constraints().len(), 2);
    drop((reopened, wal));
}
