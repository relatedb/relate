//! The vendored Odyssey vault e2e (ADR-0406 §7): 27 files transferred
//! from the lpgfs exploration as owned test data — `title:` keys
//! matching filenames 1:1 (the inference path), `name:` keys on
//! Character, full-form wikilinks under per-rel-type keys, bodies, a
//! root-level README (`:Vault` rule), and `index.md`/`log.md` at root
//! *and* inside label directories (reserved at any hierarchy level).
//! The test asserts load → checkpoint → reload stability and an
//! idempotent re-checkpoint.

mod vault_support;

use relate_storage::{
    memory_store_with_codec, parse_uri_opening_with_config, parse_uri_with_options,
    CodecOpenConfig, MarkdownBodyProperties, SnapshotCodecKind,
};
use vault_support::{copy_dir, copy_dir_into_vfs, fixture_dir, vault_signature};

#[test]
fn odyssey_vault_loads_checkpoints_and_reloads_stably() {
    // Copy the fixture into a temp dir so the checkpoint can write.
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    copy_dir(&fixture_dir(), &vault);

    let uri = vault.to_str().unwrap();
    let store = parse_uri_with_options(uri, None, Some(SnapshotCodecKind::Markdown)).unwrap();
    let (graph, _wal) = relate_storage::open_store::<relate_executor::Graph>(&store).unwrap();

    // 20 concept files + the root README, all loaded as nodes.
    let node_count = graph.all_node_ids_at(u64::MAX).len();
    assert_eq!(node_count, 21, "20 concepts + the :Vault README");

    // Every rel-type link key became relationships; spot-check the hub.
    let rel_count = graph.all_rel_ids_at(u64::MAX).len();
    assert_eq!(rel_count, 30, "Odyssey topology loaded");

    // Checkpoint, reopen: same graph shape, plus the engine sidecar.
    relate_storage::checkpoint(&store, &graph, None).unwrap();
    let manifest = SnapshotCodecKind::Markdown.checkpoint_image();
    assert!(store.vfs().exists(manifest).unwrap());
    let (reloaded, _wal) = relate_storage::open_store::<relate_executor::Graph>(&store).unwrap();
    assert_eq!(
        reloaded.all_node_ids_at(u64::MAX).len(),
        node_count,
        "the stub-free stable vault round-trips"
    );
    assert_eq!(
        reloaded.all_rel_ids_at(u64::MAX).len(),
        rel_count,
        "relationship topology survives the checkpoint"
    );

    // Idempotent re-checkpoint: no guarded file changes.
    let before = vault_signature(&store);
    relate_storage::checkpoint(&store, &reloaded, None).unwrap();
    assert_eq!(before, vault_signature(&store));
}

#[test]
fn odyssey_vault_roundtrip_on_memory_vfs_matches_native() {
    // The same fixture through a MemoryVfs store: the graph built from
    // the files must equal the native path's graph.
    let native = native_odyssey_counts();
    let memory = memory_store_with_codec(SnapshotCodecKind::Markdown).unwrap();
    let vfs = memory.vfs();
    copy_dir_into_vfs(&fixture_dir(), &*vfs);
    let (memory_graph, _) = relate_storage::open_store::<relate_executor::Graph>(&memory).unwrap();
    assert_eq!(
        memory_graph.all_node_ids_at(u64::MAX).len(),
        native.nodes,
        "the MemoryVfs graph matches the native store's node count"
    );
    assert_eq!(
        memory_graph.all_rel_ids_at(u64::MAX).len(),
        native.rels,
        "the MemoryVfs graph matches the native store's relationship count"
    );
}

/// The native path's Odyssey graph shape, opened from a fresh fixture
/// copy: the reference the MemoryVfs roundtrip is compared against.
fn native_odyssey_counts() -> OdysseyCounts {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    copy_dir(&fixture_dir(), &vault);
    let store = parse_uri_with_options(
        vault.to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Markdown),
    )
    .unwrap();
    let (graph, _) = relate_storage::open_store::<relate_executor::Graph>(&store).unwrap();
    OdysseyCounts {
        nodes: graph.all_node_ids_at(u64::MAX).len(),
        rels: graph.all_rel_ids_at(u64::MAX).len(),
    }
}

struct OdysseyCounts {
    nodes: usize,
    rels: usize,
}

/// The Odyssey fixture with a body-property list applied (ADR-0460) —
/// the shape the default-config tests above never exercise: Odysseus's
/// note heads **both** sections explicitly (`## summary` then
/// `## content`, the lpgfs-era layout the fixture carries), the other
/// notes are bare bodies, Eumaeus has none. Read: every marker is
/// stripped from its value. Write: the checkpoint re-renders the
/// canonical form, byte-exact, and the sidecar records the config so a
/// config-free reopen parses the same values.
#[test]
fn odyssey_vault_with_body_properties_reads_and_writes_sections() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    copy_dir(&fixture_dir(), &vault);
    let uri = vault.to_str().unwrap();

    let config = MarkdownBodyProperties::parse("summary;Character=summary,content").unwrap();
    let store = parse_uri_opening_with_config(
        uri,
        None,
        Some(SnapshotCodecKind::Markdown),
        false,
        Some(CodecOpenConfig::MarkdownBodies(config)),
    )
    .unwrap();
    let (graph, _wal) = relate_storage::open_store::<relate_executor::Graph>(&store).unwrap();

    // Read: the dual-marker note — both markers stripped, each property
    // owning exactly its own section's text.
    let odysseus = node_by(&graph, "name", "Odysseus");
    assert_eq!(
        prop(&odysseus, "summary"),
        Some("King of Ithaca, renowned for his cunning and endurance.".to_string()),
        "the first entry's heading is a marker, never part of the value"
    );
    assert_eq!(
        prop(&odysseus, "content"),
        Some(
            "After the fall of Troy, Odysseus wandered for ten years before returning home \
             to reclaim his kingdom and his family."
                .to_string()
        ),
        "the second entry owns its own section's text"
    );
    // A bare body is the first entry's value; the later entry is absent.
    let circe = node_by(&graph, "name", "Circe");
    assert_eq!(
        prop(&circe, "summary"),
        Some("A goddess-sorceress who transforms sailors into swine on her island.".to_string())
    );
    assert_eq!(prop(&circe, "content"), None);
    // A case-variant first marker (`## Summary`) opens the same slot —
    // Penelope's note heads its only section with the capitalized
    // spelling, a plausible hand-authoring variant.
    let penelope = node_by(&graph, "name", "Penelope");
    assert_eq!(
        prop(&penelope, "summary"),
        Some("Queen of Ithaca, who outwitted her suitors while awaiting her husband.".to_string())
    );
    assert_eq!(prop(&penelope, "content"), None);
    // A note with no body has neither.
    let eumaeus = node_by(&graph, "name", "Eumaeus");
    assert_eq!(prop(&eumaeus, "summary"), None);
    assert_eq!(prop(&eumaeus, "content"), None);
    // The default list governs labels without an override (the Place
    // files key on `title`, not `name`).
    let ithaca = node_by(&graph, "title", "Ithaca");
    assert_eq!(
        prop(&ithaca, "summary"),
        Some("A rocky island kingdom in the Ionian Sea, home of Odysseus.".to_string())
    );

    // Write: the checkpoint renders the canonical form — the frontmatter
    // and wikilinks untouched, the first section bare, the second under
    // its marker.
    relate_storage::checkpoint(&store, &graph, None).unwrap();
    let odysseus_md = String::from_utf8(
        relate_storage::read_to_vec(&*store.vfs(), "Character/Odysseus.md").unwrap(),
    )
    .unwrap();
    let (frontmatter, body) = split_file(&odysseus_md);
    assert!(frontmatter.contains("type: Character"));
    assert!(frontmatter.contains("name: Odysseus"));
    assert!(frontmatter.contains("MARRIED_TO:\n  - \"[[Character/Penelope]]\""));
    assert_eq!(
        body,
        "King of Ithaca, renowned for his cunning and endurance.\n\n## content\n\nAfter the \
         fall of Troy, Odysseus wandered for ten years before returning home to reclaim his \
         kingdom and his family.\n",
        "canonical body: first section bare, second under its marker"
    );
    // The bare-body notes are unchanged in shape.
    let circe_md = String::from_utf8(
        relate_storage::read_to_vec(&*store.vfs(), "Character/Circe.md").unwrap(),
    )
    .unwrap();
    let (_, circe_body) = split_file(&circe_md);
    assert_eq!(
        circe_body,
        "A goddess-sorceress who transforms sailors into swine on her island.\n"
    );

    // The sidecar recorded the configuration: a config-free reopen
    // parses the same values from the canonical files. Both the store
    // target and the WAL handle hold the writer lease.
    drop(_wal);
    drop(store);
    let reopened = parse_uri_with_options(uri, None, Some(SnapshotCodecKind::Markdown)).unwrap();
    let (reloaded, _wal) = relate_storage::open_store::<relate_executor::Graph>(&reopened).unwrap();
    let odysseus = node_by(&reloaded, "name", "Odysseus");
    assert_eq!(
        prop(&odysseus, "summary"),
        Some("King of Ithaca, renowned for his cunning and endurance.".to_string())
    );
    assert_eq!(
        prop(&odysseus, "content"),
        Some(
            "After the fall of Troy, Odysseus wandered for ten years before returning home \
             to reclaim his kingdom and his family."
                .to_string()
        )
    );
}

/// The suite's one node scan, parameterized by the lookup key — the
/// fixture's Character files carry `name:`, its Place files `title:`.
fn node_by(graph: &relate_executor::Graph, key: &str, value: &str) -> relate_core::Node {
    graph
        .all_node_ids_at(u64::MAX)
        .into_iter()
        .map(|id| graph.node_at(id, u64::MAX).unwrap())
        .find(|node| prop(node, key).as_deref() == Some(value))
        .unwrap_or_else(|| panic!("the vault holds {key}={value}"))
}

fn prop(node: &relate_core::Node, key: &str) -> Option<String> {
    node.properties
        .get(key)
        .and_then(relate_core::Value::as_string)
        .map(str::to_string)
}

fn split_file(text: &str) -> (String, String) {
    let (frontmatter, body) = relate_markdown::split_frontmatter(text);
    let frontmatter = frontmatter.expect("the note carries frontmatter");
    // The fence/body separator blank line is layout, not content.
    let body = body.strip_prefix('\n').unwrap_or(body);
    (frontmatter.to_string(), body.to_string())
}
