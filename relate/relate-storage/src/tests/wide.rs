//! The wide-column opt-in (ADR-0534): an option, not a replacement — a
//! legacy store stays legacy without it, re-encodes once with it, and a
//! wide manifest keeps the store wide whatever the next open's flags.

use super::*;

/// A v3 store for the wide opt-in test (ADR-0534): six Person nodes —
/// `name` and `age` on all, `rare` on the first only (one row in six:
/// below the promotion share, residual) — under one legacy-shaped node
/// part, the pre-wide encoder's bytes published as the manifest's only
/// part.
#[cfg(parquet_codec)]
fn legacy_v3_shard_store(uri: &std::path::Path) {
    let target = parse_uri(uri.to_str().unwrap()).unwrap();
    let (wv, prefix) = target.wal_vfs();
    let mut wal = Wal::open(wv, prefix, 0).unwrap();
    let graph = Graph::default();
    let nodes = (0..6i64).map(|i| {
        let mut props = std::collections::BTreeMap::from([
            (
                "name".to_owned(),
                relate_core::Value::String(format!("n{i}").into()),
            ),
            ("age".to_owned(), relate_core::Value::Int(20 + i)),
        ]);
        if i == 0 {
            props.insert("rare".to_owned(), relate_core::Value::Bool(true));
        }
        (vec!["Person".to_owned()], props)
    });
    let ts = commit_nodes(&graph, &mut wal, nodes);
    let ids = graph.all_node_ids_at(ts);
    let bytes = crate::codec::parquet::nodes_parquet_ids(&graph, ts, &ids).unwrap();
    let path = "nodes/0-6-0123456789abcdef0123456789abcdef.parquet".to_owned();
    let mut manifest = bare_manifest(
        crate::version::manifest::CURRENT_VERSION,
        graph.next_node_id(),
        graph.next_rel_id(),
        wal.lsn,
        ts,
    );
    manifest.node_parts = vec![crate::snapshot::ShardPart {
        path: path.clone(),
        id_lo: 0,
        id_hi: 6,
        rows: ids.len() as u64,
        bytes: bytes.len() as u64,
    }];
    snapshot::put_object(
        &target,
        crate::snapshot::MANIFEST_JSON,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    snapshot::put_object(&target, &path, &bytes).unwrap();
}

/// Open `uri` with the Parquet-wide opt-in (ADR-0534) and load it: the
/// one spelling of the wide open this suite's tests share.
#[cfg(parquet_codec)]
fn open_wide(uri: &std::path::Path) -> (StoreTarget, Graph, Wal, crate::version::MigrationReport) {
    let target = parse_uri_opening_with_config(
        uri.to_str().unwrap(),
        None,
        Some(crate::SnapshotCodecKind::Parquet),
        false,
        Some(crate::CodecOpenConfig::ParquetWide),
    )
    .unwrap();
    let (graph, wal, _, report) = open_store_with_report::<Graph>(&target).unwrap();
    (target, graph, wal, report)
}

/// ADR-0534: the wide-column encoding is an option, not a replacement. A
/// v3 store opened without the opt-in stays legacy — no migration, its
/// next checkpoint keeping the part as is; opened with the opt-in, its
/// first checkpoint re-encodes every part wide and publishes a v5
/// manifest, which then keeps the store wide whatever the next open's
/// flags.
#[cfg(parquet_codec)]
#[test]
fn wide_opt_in_re_encodes_a_legacy_store_once_and_sticks() {
    let dir = tempfile::tempdir().unwrap();

    // Without the opt-in: the store stays legacy, its re-checkpoint keeps
    // the part as is.
    let plain = dir.path().join("plain");
    legacy_v3_shard_store(&plain);
    {
        let target = parse_uri(plain.to_str().unwrap()).unwrap();
        let (opened, wal, _, report) = open_store_with_report::<Graph>(&target).unwrap();
        assert!(
            report.is_empty(),
            "a current v3 store does not migrate: {report}"
        );
        let before = on_disk_manifest(&target).node_parts[0].path.clone();
        checkpoint_with_lsn(&target, &opened, wal.lsn, None).unwrap();
        let manifest = on_disk_manifest(&target);
        assert_eq!(
            manifest.format_version,
            crate::version::manifest::CURRENT_VERSION
        );
        assert_eq!(
            manifest.node_parts[0].path, before,
            "the legacy part is kept as is"
        );
        assert_eq!(opened.live_node_count(), 6);
        drop((opened, wal));
    }

    // With the opt-in: the first checkpoint re-encodes every part wide,
    // the promoted keys becoming typed columns and the manifest publishing
    // at the wide version.
    let optin = dir.path().join("optin");
    legacy_v3_shard_store(&optin);
    {
        let (target, opened, wal, report) = open_wide(&optin);
        assert!(
            report.is_empty(),
            "v3 is current: the re-encode is the opt-in checkpoint, not a migration"
        );
        checkpoint_with_lsn(&target, &opened, wal.lsn, None).unwrap();
        let manifest = on_disk_manifest(&target);
        assert_eq!(
            manifest.format_version,
            crate::version::manifest::WIDE_VERSION
        );
        let [part] = &manifest.node_parts[..] else {
            panic!("one wide part: {manifest:?}");
        };
        let columns = crate::codec::parquet::previous_shard_columns(
            target.vfs(),
            &part.path,
            crate::snapshot::Kind::Node,
            part.id_lo..part.id_hi,
        )
        .unwrap();
        let keys: Vec<&str> = columns.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["age", "name"], "the promoted keys in key order");
        assert_eq!(
            opened.node(0).unwrap().properties.get("rare"),
            Some(&relate_core::Value::Bool(true)),
            "the unpromoted key survives through the residual column"
        );
        drop((opened, wal));
    }

    // The wide store stays wide without the opt-in.
    {
        let target = parse_uri(optin.to_str().unwrap()).unwrap();
        let (opened, wal, _, report) = open_store_with_report::<Graph>(&target).unwrap();
        assert!(report.is_empty(), "{report}");
        let before = on_disk_manifest(&target).node_parts[0].path.clone();
        checkpoint_with_lsn(&target, &opened, wal.lsn, None).unwrap();
        let manifest = on_disk_manifest(&target);
        assert_eq!(
            manifest.format_version,
            crate::version::manifest::WIDE_VERSION
        );
        assert_eq!(manifest.node_parts[0].path, before, "kept as is");
        assert_eq!(opened.live_node_count(), 6);
    }
}

/// The full property map of node `i`, sorted pairs, for row-survival
/// asserts.
#[cfg(parquet_codec)]
fn node_properties(opened: &Graph, i: i64) -> Vec<(String, relate_core::Value)> {
    opened
        .node(i)
        .unwrap()
        .properties
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// ADR-0534: a wide checkpoint that copies clean rows out of a previous
/// wide part — the carried columns, the per-row reroute of a
/// type-conflicting value into the residual column, the merge of the
/// newly encoded rows' keys — preserves every property of every row:
/// the copied rows through their columns, the dirty rows through their
/// re-encode.
#[cfg(parquet_codec)]
#[test]
fn a_wide_checkpoint_copies_clean_rows_and_keeps_every_property() {
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().join("wide");
    legacy_v3_shard_store(&uri);
    let before: Vec<Vec<(String, relate_core::Value)>>;
    {
        let (target, opened, mut wal, _) = open_wide(&uri);
        checkpoint_with_lsn(&target, &opened, wal.lsn, None).unwrap();
        // The next checkpoint copies the clean rows: only nodes 1 and 2
        // are dirty — node 1's `age` turns String (the carried Int
        // column reroutes it to the residual per row), node 2 gains a
        // key.
        opened.begin_write().unwrap();
        opened.set_property_node(1, "age", relate_core::Value::String("old".into()));
        opened.set_property_node(2, "joined", relate_core::Value::Int(2026));
        let ops = opened.take_redo();
        let ts = opened.pending_commit_ts();
        opened.abort_write();
        wal.append_commit(ts, &ops).unwrap();
        opened.apply_redo(ops, ts).unwrap();
        // The live state every row must still carry after the next
        // checkpoint — copied and encoded alike.
        before = (0..6).map(|i| node_properties(&opened, i)).collect();
        checkpoint_with_lsn(&target, &opened, wal.lsn, None).unwrap();
    }

    // Reopened, every row carries exactly the properties it had before
    // the copying checkpoint.
    let target = parse_uri(uri.to_str().unwrap()).unwrap();
    let (opened, _wal, _, _) = open_store_with_report::<Graph>(&target).unwrap();
    let after: Vec<Vec<(String, relate_core::Value)>> =
        (0..6).map(|i| node_properties(&opened, i)).collect();
    assert_eq!(after, before, "every row keeps every property");
}
