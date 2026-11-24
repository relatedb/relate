//! ADR-0610's typed transaction records: the record's family `type`
//! string. Provenance records spell it; the retained record and its WAL
//! tail carry it; a tail-less legacy record and an old family part
//! without the field read as the provenance family's own; a record of
//! another type — seeded through the embedded Rust connection's typed
//! channel, `Connection::set_tx_metadata_of_type` — roundtrips WAL
//! replay and checkpoint restore with its type intact. The untyped
//! channels hard-set the provenance type (the Bolt BEGIN channel's side
//! lives in the relate-bolt suite), changed keys derive only for
//! provenance commits, and `provenance()`/`history()` join only
//! provenance records — a mixed-type store's provenance timeline holds
//! only its own family's commits. A submodule of
//! `tests/value_provenance.rs`: its own binary is the parent's, so the
//! process-global `RELATE_PREVIEW` opt-in is the parent's too.

use super::*;

/// The audit-family metadata map the typed-channel fixtures seed.
fn why_run() -> BTreeMap<String, Value> {
    BTreeMap::from([("why".to_string(), Value::String("nightly-run".into()))])
}

/// The bookmark sequence of one committed auto-commit write (the parent
/// module's `sequence_of`, restated for the reader of this file's
/// typed-channel fixtures).
fn typed_write(
    c: &mut relate::Connection,
    record_type: Option<&str>,
    metadata: BTreeMap<String, Value>,
    statement: &str,
) -> u64 {
    match record_type {
        Some(name) => c.set_tx_metadata_of_type(name, metadata).unwrap(),
        None => c.set_tx_metadata(metadata).unwrap(),
    }
    sequence_of(c.query(statement).unwrap().bookmark.as_deref())
}

/// The one retained record of the commit at `commit_ts` — the guarded
/// accessor every fixture below reads its type through.
fn record_at(db: &Database, commit_ts: u64) -> TransactionRecord {
    db.transaction_records()
        .unwrap()
        .into_iter()
        .find(|record| record.commit_ts == commit_ts)
        .unwrap_or_else(|| panic!("the commit at {commit_ts} retained a record"))
}

#[test]
fn provenance_records_spell_their_type() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // The embedded channel's record: the metadata map beside the
    // family's own type, spelled.
    let seeded = typed_write(
        &mut c,
        None,
        by_app(),
        "CREATE (n:P {k: 1}) RETURN id(n) AS id",
    );
    let record = record_at(&db, seeded);
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(record.metadata.as_ref(), Some(&by_app()));
    // A plain property write (no channel at all): the changed keys
    // alone retain, and the record still spells the family's type.
    let plain = sequence_of(
        c.query("MATCH (n:P) SET n.k = 2")
            .unwrap()
            .bookmark
            .as_deref(),
    );
    let record = record_at(&db, plain);
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(record.metadata, None);
    assert!(record.changed_keys.is_some());
    // A labels-only create changes no property key: no record at all —
    // the nothing-to-say commit writes no tail, type included.
    c.query("CREATE (:Labeled)").unwrap();
    assert_eq!(db.transaction_records().unwrap().len(), 2);
}

#[test]
fn the_typed_channel_seeds_a_record_of_its_own_type() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let audit = typed_write(
        &mut c,
        Some("audit-trail"),
        why_run(),
        "CREATE (n:P {k: 1}) RETURN id(n) AS id",
    );
    // The record belongs to the typed channel's family: its type and
    // metadata, and no derived keys — changed keys are the provenance
    // family's derivation, never another family's.
    let record = record_at(&db, audit);
    assert_eq!(record.record_type.as_str(), "audit-trail");
    assert_eq!(record.metadata.as_ref(), Some(&why_run()));
    assert_eq!(record.changed_keys, None);
    // The boundary clears the type with the map: the next plain write
    // is a provenance record again.
    let plain = sequence_of(
        c.query("MATCH (n:P) SET n.k = 2")
            .unwrap()
            .bookmark
            .as_deref(),
    );
    let record = record_at(&db, plain);
    assert_eq!(record.record_type.as_str(), "provenance");
    assert!(record.changed_keys.is_some());
}

#[test]
fn the_typed_channel_validates_the_name_and_reserves_provenance() {
    let db = gated();
    let mut c = db.connect().unwrap();
    for name in ["", "Au dit", "audit_trail", "a--b", "a-", "-a", "Audit"] {
        let error = c
            .set_tx_metadata_of_type(name, by_app())
            .err()
            .unwrap_or_else(|| panic!("`{name}` must be rejected"));
        assert!(
            error.to_string().contains(&format!(
                "InvalidArgumentValue: record type '{name}' must be a non-empty kebab-case name"
            )),
            "`{name}`: {error}"
        );
    }
    // The family's own name is reserved: the untyped channels seed it,
    // so a typed seed naming it is the typed rejection.
    let reserved = c
        .set_tx_metadata_of_type("provenance", by_app())
        .expect_err("the provenance family's own name is reserved");
    assert!(
        reserved
            .to_string()
            .contains("InvalidArgumentValue: record type 'provenance' is reserved"),
        "{reserved}"
    );
    // A refused seed sets nothing: the next write is a plain
    // provenance record.
    let plain = sequence_of(c.query("CREATE (n:P {k: 1})").unwrap().bookmark.as_deref());
    let record = record_at(&db, plain);
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(record.metadata, None);
    // An empty map seeds nothing (the untyped channel's own rule): the
    // audit-typed boundary then writes no record at all.
    let before = db.transaction_records().unwrap().len();
    c.set_tx_metadata_of_type("audit-trail", BTreeMap::new())
        .unwrap();
    c.query("CREATE (n:Q {k: 2})").unwrap();
    assert_eq!(db.transaction_records().unwrap().len(), before);
}

#[test]
fn the_untyped_channels_hard_set_the_provenance_type() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // The embedded untyped channel after a typed seed: the later
    // channel call owns the transaction's type, and the maps merge in
    // call order (later keys win).
    c.set_tx_metadata_of_type("audit-trail", why_run()).unwrap();
    let embedded = typed_write(
        &mut c,
        None,
        by_app(),
        "CREATE (n:P {k: 1}) RETURN id(n) AS id",
    );
    let record = record_at(&db, embedded);
    assert_eq!(record.record_type.as_str(), "provenance");
    let mut merged = why_run();
    merged.insert("by".to_string(), Value::String("app".into()));
    assert_eq!(record.metadata.as_ref(), Some(&merged));
    // The in-statement procedure channel: same rule — a
    // `tx.setMetaData` inside the transaction returns the type to the
    // family default.
    c.set_tx_metadata_of_type("audit-trail", why_run()).unwrap();
    c.begin().unwrap();
    c.query("CALL tx.setMetaData({by: 'job'})").unwrap();
    c.query("MATCH (n:P) SET n.k = 2").unwrap();
    let bookmark = c.commit().unwrap().expect("the transaction wrote");
    let commit: u64 = bookmark.strip_prefix("relate:").unwrap().parse().unwrap();
    let record = record_at(&db, commit);
    assert_eq!(record.record_type.as_str(), "provenance");
    let mut expected = why_run();
    expected.insert("by".to_string(), Value::String("job".into()));
    assert_eq!(record.metadata.as_ref(), Some(&expected));
}

/// The filter (ADR-0610): a mixed-type store writes the same property
/// through a provenance commit and a second-type commit, and the
/// provenance timeline holds only its own family's commits — the
/// audit-typed write joins no `provenance()`/`history()` event, and its
/// record derives no changed keys.
#[test]
fn provenance_and_history_join_only_provenance_records() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let birth = committed(&mut c, "CREATE (n:P {k: 0})");
    // The second-type commit: the same property through the typed
    // channel's transaction.
    let audit = typed_write(
        &mut c,
        Some("audit-trail"),
        why_run(),
        "MATCH (n:P) SET n.k = 1",
    );
    let reset = committed(&mut c, "MATCH (n:P) SET n.k = 2");
    // The audit record retains with its type and no derived keys.
    let record = record_at(&db, audit);
    assert_eq!(record.record_type.as_str(), "audit-trail");
    assert_eq!(record.changed_keys, None);
    // The provenance timeline: birth and the provenance reset only —
    // the audit commit's k=1 exists in the version chain but joins no
    // provenance event.
    let history = c
        .query("MATCH (element:P) RETURN history(element, 'k') AS h")
        .unwrap();
    assert_eq!(
        history.rows[0][0],
        Value::List(
            vec![
                event(&db, birth, Value::Int(0)),
                event(&db, reset, Value::Int(2)),
            ]
            .into()
        )
    );
    let newest = c
        .query("MATCH (element:P) RETURN provenance(element, 'k') AS p")
        .unwrap();
    assert_eq!(newest.rows[0][0], event(&db, reset, Value::Int(2)));
}

/// The type column (ADR-0610): `db.transactions` lists every retained
/// record with its type — a provenance row answering its changed keys,
/// another family's row answering null (its keys were never derived) —
/// and `YIELD`/`WHERE` filters on it.
#[test]
fn db_transactions_yields_the_type_column_and_filters_by_it() {
    let db = gated();
    let mut c = db.connect().unwrap();
    three_agent_commits(&mut c);
    typed_write(
        &mut c,
        Some("audit-trail"),
        why_run(),
        "CREATE (n:Task {title: 'audit'})",
    );

    // The mixed table: every retained record with its type, the audit
    // row's changedKeys null.
    let rows = c
        .query("CALL db.transactions() YIELD type, changedKeys RETURN type, changedKeys")
        .unwrap();
    let audit_row = rows
        .rows
        .iter()
        .find(|row| row[0] == Value::String("audit-trail".into()))
        .expect("the audit-typed record is listed");
    assert_eq!(audit_row[1], Value::Null);
    let provenance_rows: Vec<_> = rows
        .rows
        .iter()
        .filter(|row| row[0] == Value::String("provenance".into()))
        .collect();
    assert_eq!(provenance_rows.len(), 3);
    for row in &provenance_rows {
        assert!(
            matches!(row[1], Value::List(_)),
            "changedKeys: {:?}",
            row[1]
        );
    }

    // The bounding filter the audit question itself asks: one family's
    // commits only.
    let of_type = c
        .query(
            "CALL db.transactions() YIELD type, changedKeys, metadata \
             WHERE type = 'audit-trail' RETURN changedKeys, metadata",
        )
        .unwrap();
    assert_eq!(
        of_type.rows,
        vec![vec![Value::Null, Value::Map(why_run().into()),]]
    );
    // And the element the provenance family's rows still name: the
    // deleted load node's record keeps its changed keys.
    let provenance_keys = c
        .query(
            "CALL db.transactions() YIELD type, changedKeys \
             WHERE type = 'provenance' UNWIND changedKeys AS ck RETURN ck.key AS key",
        )
        .unwrap();
    assert!(
        provenance_keys
            .rows
            .iter()
            .any(|row| row[0] == Value::String("title".into())),
        "{:?}",
        provenance_keys.rows
    );
}

/// A record of another type roundtrips both restore forms with its type
/// intact (ADR-0610): the WAL replay of a crash-after-fsync reopen, and
/// the checkpoint's `transactions/` family on the reopen after it.
#[test]
fn a_second_type_roundtrips_wal_replay_and_checkpoint_restore() {
    common::set_preview_once("value-provenance");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let open =
        || Database::open_with(path, common::store::opts(SnapshotCodecKind::default())).unwrap();
    let (audit, provenance, birth) = {
        let db = open();
        let mut c = db.connect().unwrap();
        let birth = committed(&mut c, "CREATE (n:P {k: 0})");
        let audit = typed_write(
            &mut c,
            Some("audit-trail"),
            why_run(),
            "MATCH (n:P) SET n.k = 1",
        );
        let provenance = sequence_of(
            c.query("MATCH (n:P) SET n.k = 2")
                .unwrap()
                .bookmark
                .as_deref(),
        );
        // A crash after the WAL fsync: both records live in the log.
        drop(c);
        db.leak_without_checkpoint();
        (audit, provenance, birth)
    };
    // The WAL replay: both records, their types with them.
    {
        let db = open();
        let audit_record = record_at(&db, audit);
        assert_eq!(audit_record.record_type.as_str(), "audit-trail");
        assert_eq!(audit_record.metadata.as_ref(), Some(&why_run()));
        assert_eq!(
            record_at(&db, provenance).record_type.as_str(),
            "provenance"
        );
    }
    // The checkpoint restore: the family part carries the types, and
    // the provenance timeline still answers only its own commits.
    {
        let db = open();
        db.checkpoint().unwrap();
        drop(db);
        let db = open();
        let audit_record = record_at(&db, audit);
        assert_eq!(audit_record.record_type.as_str(), "audit-trail");
        assert_eq!(
            record_at(&db, provenance).record_type.as_str(),
            "provenance"
        );
        let mut c = db.connect().unwrap();
        let history = c
            .query("MATCH (element:P) RETURN history(element, 'k') AS h")
            .unwrap();
        assert_eq!(
            history.rows[0][0],
            Value::List(
                vec![
                    event(&db, birth, Value::Int(0)),
                    event(&db, provenance, Value::Int(2)),
                ]
                .into()
            )
        );
    }
}

/// An old family part without the `type` field reads as the provenance
/// family's (ADR-0610): the part a pre-typed binary published — records
/// with metadata and changed keys but no type — loads as provenance
/// records, so the typed era changes nothing for yesterday's stores.
#[test]
fn an_old_family_part_without_the_type_field_reads_as_provenance() {
    common::set_preview_once("value-provenance");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    common::store::seed_store(&path, SnapshotCodecKind::Json);
    // One pre-typed record: the committed shape an earlier feature build
    // wrote — a metadata map (tagged values) and derived changed keys,
    // and no `type` field at all.
    let part = concat!(
        "{\"records\":[{\"lsn\":4,\"commit_ts\":4,",
        "\"metadata\":{\"by\":{\"t\":\"string\",\"v\":\"app\"}},",
        "\"changed_keys\":[{\"kind\":\"node\",\"id\":2,\"key\":\"name\"}]}]}",
    );
    {
        let vfs = common::store::store_vfs(&path);
        vfs.create_dir_all("transactions").unwrap();
        let name = "transactions/00000000000000000004-00000000000000000004.json";
        let mut file = vfs
            .open(name, relate_storage::VfsOpen::create_truncate())
            .unwrap();
        file.write_all(part.as_bytes()).unwrap();
        file.sync_all().unwrap();
        vfs.sync_dir("transactions").unwrap();
    }
    let db = Database::open_with(
        path.to_str().unwrap(),
        common::store::opts(SnapshotCodecKind::Json),
    )
    .unwrap();
    let record = record_at(&db, 4);
    assert_eq!(
        record.metadata.as_ref().and_then(|map| map.get("by")),
        Some(&Value::String("app".into()))
    );
    assert!(record.changed_keys.is_some());
    // The part carried no `type` field: the typed default it decodes as
    // is the provenance family's own (the absent-field rule's one home,
    // `RecordType::default`).
    assert_eq!(record.record_type(), &relate::RecordType::Provenance);
    assert!(record.is_provenance());
    // The audit surface agrees: the old record's row answers the
    // provenance family's type.
    let mut c = db.connect().unwrap();
    let old = c
        .query(
            "CALL db.transactions(4) YIELD type, metadata \
             WHERE metadata.by = 'app' RETURN type",
        )
        .unwrap();
    assert_eq!(old.rows, vec![vec![Value::String("provenance".into())]]);
}
