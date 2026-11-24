//! ADR-0610's transaction-record machinery under the `value-provenance`
//! opt-in: the three metadata channels (`tx.setMetaData`, the embedded
//! `Connection::set_tx_metadata`, a Bolt BEGIN's `tx_metadata` extra —
//! the last in the relate-bolt suite), the changed-keys derivation, the
//! records' survival — through WAL replay (reopen without checkpoint)
//! and checkpoint restore, on native directories, a shared `MemoryVfs`
//! and `.rl` bundles, for every compiled snapshot codec (the roundtrip
//! patterns of `src/checkpoint/tests.rs` and the codec matrix of
//! `tests/ontology_store.rs`) — and the `db.transactions` procedure: the
//! audit query surface over those records (the `dbms_procedures.rs`
//! pattern — a matching `since` filter, a no-match one, and the filter
//! semantics ADR-0610 records). The `history()` function and the
//! retention that keeps its chains whole across checkpoint/GC/reopen
//! live in `value_provenance/history_retention.rs`, and the typed
//! transaction records (the `type` field, the typed channel, the
//! provenance-only join, the mixed-type store) in
//! `value_provenance/typed_records.rs` (the
//! `constraint_validation.rs` one-file-per-concern pattern), in this
//! same binary.
//!
//! This file is its own test binary: `RELATE_PREVIEW` is process-global
//! and read once per `Database::open` (ADR-0371), so the opt-in is set
//! before the first open and every test in this crate runs behind the
//! gate. The closed side — the fixed compat rows, the authored preview
//! error, and the zero-byte guarantee — lives in
//! `tests/value_provenance_closed.rs`, its own binary.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use relate::{ChangedKey, ChangedKeyKind, Database, TransactionRecord, Value};
use relate_storage::{MemoryVfs, SnapshotCodecKind, Vfs};

/// Opt this process into `value-provenance` before the first open —
/// the Once-shared form, so sibling threads opening stores race no
/// in-flight `set_var`.
fn gated() -> Database {
    common::set_preview_once("value-provenance");
    Database::open(":memory:").unwrap()
}

/// The `by: app` metadata map every channel test seeds.
fn by_app() -> BTreeMap<String, Value> {
    BTreeMap::from([("by".to_string(), Value::String("app".into()))])
}

/// One `ChangedKey`, spelled once for the expectations below.
fn node_key(id: i64, key: &str) -> ChangedKey {
    ChangedKey {
        kind: ChangedKeyKind::Node,
        id,
        key: key.to_string(),
    }
}

/// The first column of a one-row result, as the map the metadata
/// procedures yield.
fn metadata_map(result: &relate::QueryResult) -> Value {
    result.rows[0][0].clone()
}

#[test]
fn set_metadata_stores_the_map_and_get_metadata_returns_it() {
    let mut c = gated().connect().unwrap();
    // Off the transaction the call is a no-op; here, behind the opt-in,
    // the map lands on the transaction's state.
    c.begin().unwrap();
    c.query("CALL tx.setMetaData({by: 'x', count: 2})").unwrap();
    let result = c
        .query("CALL tx.getMetaData() YIELD metadata RETURN metadata")
        .unwrap();
    assert_eq!(
        metadata_map(&result),
        Value::Map(
            BTreeMap::from([
                ("by".to_string(), Value::String("x".into())),
                ("count".to_string(), Value::Int(2)),
            ])
            .into()
        )
    );
    // A second call merges, later keys winning — ADR-0610's call order.
    c.query("CALL tx.setMetaData({by: 'y'})").unwrap();
    let result = c
        .query("CALL tx.getMetaData() YIELD metadata RETURN metadata")
        .unwrap();
    assert_eq!(
        metadata_map(&result),
        Value::Map(
            BTreeMap::from([
                ("by".to_string(), Value::String("y".into())),
                ("count".to_string(), Value::Int(2)),
            ])
            .into()
        )
    );
    // A wrong-typed argument keeps the call-shape rejection the corpus
    // pins, gate or no gate.
    let error = c
        .query("CALL tx.setMetaData(1)")
        .expect_err("the MAP type check holds behind the gate");
    assert!(error.to_string().contains("InvalidArgumentType"), "{error}");
}

#[test]
fn an_explicit_transaction_serializes_its_metadata_into_its_record() {
    let db = gated();
    let mut c = db.connect().unwrap();
    c.set_tx_metadata(by_app()).unwrap();
    c.begin().unwrap();
    c.query("CREATE (n:Person {name: 'Ada'})").unwrap();
    let bookmark = c.commit().unwrap().expect("the transaction wrote");
    // The record of the commit the bookmark names carries the map.
    let sequence: u64 = bookmark.strip_prefix("relate:").unwrap().parse().unwrap();
    let record = db
        .transaction_records()
        .unwrap()
        .into_iter()
        .find(|record| record.commit_ts == sequence)
        .expect("the commit retained a record");
    assert_eq!(record.metadata.as_ref(), Some(&by_app()));
    assert_eq!(
        record.changed_keys.as_deref(),
        Some(&[node_key(0, "name")][..])
    );
    // The map is cleared after: the next transaction's record carries none.
    c.begin().unwrap();
    c.query("CREATE (:Person {name: 'Bob'})").unwrap();
    let second = c.commit().unwrap().expect("the second transaction wrote");
    let sequence: u64 = second.strip_prefix("relate:").unwrap().parse().unwrap();
    let record = db
        .transaction_records()
        .unwrap()
        .into_iter()
        .find(|record| record.commit_ts == sequence)
        .expect("the second commit retained a record");
    assert_eq!(record.metadata, None);
}

#[test]
fn the_embedded_channel_seeds_the_next_auto_commit_and_clears_after() {
    let db = gated();
    let mut c = db.connect().unwrap();
    c.set_tx_metadata(by_app()).unwrap();
    let result = c.query("CREATE (:Person {name: 'Ada'})").unwrap();
    let sequence: u64 = result
        .bookmark
        .as_deref()
        .and_then(|b| b.strip_prefix("relate:"))
        .and_then(|n| n.parse().ok())
        .expect("the auto-commit write carries a bookmark");
    let record = db
        .transaction_records()
        .unwrap()
        .into_iter()
        .find(|record| record.commit_ts == sequence)
        .expect("the auto-commit retained a record");
    assert_eq!(record.metadata.as_ref(), Some(&by_app()));
    // Cleared after (ADR-0610): a later write carries nothing.
    c.query("CREATE (:Person {name: 'Bob'})").unwrap();
    assert_eq!(
        db.transaction_records()
            .unwrap()
            .into_iter()
            .filter(|record| record.metadata.is_some())
            .count(),
        1
    );
}

#[test]
fn rollback_discards_the_transaction_metadata() {
    let db = gated();
    let mut c = db.connect().unwrap();
    c.set_tx_metadata(by_app()).unwrap();
    c.begin().unwrap();
    c.query("CREATE (:Discarded {name: 'no'})").unwrap();
    c.rollback().unwrap();
    // The rolled-back transaction's metadata never rides a later one.
    c.query("CREATE (:Kept {name: 'yes'})").unwrap();
    for record in db.transaction_records().unwrap() {
        assert_eq!(record.metadata, None, "{record:?}");
    }
}

#[test]
fn changed_keys_carry_creates_sets_removes_and_deletes() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // A pre-existing element, for the delete arm's pre-state read.
    c.query("CREATE (:Old {a: 1, b: 2})").unwrap();
    // One statement covering the create, set and remove arms — and a
    // same-statement create-then-delete, whose keys the create already
    // contributed (the element did not exist at the snapshot, so the
    // delete arm reads no pre-state for it).
    c.query(
        "CREATE (n:P {name: 'Ada'}) \
         CREATE (m:Q {gone: 1, stays: 2}) \
         SET m.gone2 = 3 \
         REMOVE m.gone \
         DELETE m",
    )
    .unwrap();
    // The delete arm over a pre-existing element: every key it carried.
    c.query("MATCH (n:Old) DELETE n").unwrap();
    let records = db.transaction_records().unwrap();
    assert_eq!(records.len(), 3, "{records:?}");
    assert_eq!(
        records[0].changed_keys.as_deref(),
        Some(&[node_key(0, "a"), node_key(0, "b")][..])
    );
    assert_eq!(
        records[1].changed_keys.as_deref(),
        Some(
            &[
                node_key(1, "name"),
                node_key(2, "gone"),
                node_key(2, "gone2"),
                node_key(2, "stays"),
            ][..]
        )
    );
    assert_eq!(
        records[2].changed_keys.as_deref(),
        Some(&[node_key(0, "a"), node_key(0, "b")][..])
    );
    // Labels and catalog DDL are not property keys, and a property-less
    // create changes none: those commits retain no record at all.
    c.query("CREATE (:Labeled)").unwrap();
    c.query("CREATE INDEX labeled_x FOR (n:Labeled) ON (n.x)")
        .unwrap();
    assert_eq!(db.transaction_records().unwrap().len(), 3);
}

// --- ADR-0610's provenance() function (the bar_function.rs pattern,
// in this binary because the name is callable only behind the opt-in). ---

/// One statement in its own committed transaction: the bookmark's
/// sequence is the commitTs its versions carry — the fixture every
/// provenance() test below drives its writes through.
fn committed(c: &mut relate::Connection, statement: &str) -> u64 {
    c.begin().unwrap();
    c.query(statement).unwrap();
    let bookmark = c.commit().unwrap().expect("the transaction wrote");
    bookmark
        .strip_prefix("relate:")
        .unwrap()
        .parse()
        .expect("the bookmark names the commit sequence")
}

/// The ADR-0610 event map of the retained record at `commit_ts` with
/// `value` — the expectation shape every provenance() assertion renders:
/// the lsn and metadata come from the record itself, so a passing
/// assertion proves the function joined them.
fn event(db: &Database, commit_ts: u64, value: Value) -> Value {
    let record = db
        .transaction_records()
        .unwrap()
        .into_iter()
        .find(|record| record.commit_ts == commit_ts)
        .unwrap_or_else(|| panic!("the commit at {commit_ts} retained a record"));
    Value::Map(
        BTreeMap::from([
            ("commitTs".to_string(), Value::Int(commit_ts as i64)),
            ("lsn".to_string(), Value::Int(record.lsn as i64)),
            (
                "metadata".to_string(),
                Value::Map(record.metadata.clone().unwrap_or_default().into()),
            ),
            ("value".to_string(), value),
        ])
        .into(),
    )
}

/// The single `provenance()` cell of a one-row query over `pattern`.
fn provenance_of(c: &mut relate::Connection, pattern: &str, key: &str) -> Value {
    let result = c
        .query(&format!(
            "MATCH {pattern} RETURN provenance(element, '{key}') AS p"
        ))
        .unwrap();
    assert_eq!(result.rows.len(), 1, "one {pattern} row");
    result.rows[0][0].clone()
}

#[test]
fn a_set_leaves_the_sibling_property_at_its_own_event() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let birth = committed(&mut c, "CREATE (n:P {a: 1, b: 2})");
    let set_a = committed(&mut c, "MATCH (n:P) SET n.a = 3");
    // The SET commit is `a`'s event; the sibling it did not touch keeps
    // the commit that wrote it (ADR-0610's whole-row diffing: one version
    // changed only the key it changed).
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "a"),
        event(&db, set_a, Value::Int(3))
    );
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "b"),
        event(&db, birth, Value::Int(2))
    );
}

#[test]
fn a_removed_property_answers_the_removal_event() {
    let db = gated();
    let mut c = db.connect().unwrap();
    committed(&mut c, "CREATE (n:P {k: 1})");
    let removal = committed(&mut c, "MATCH (n:P) REMOVE n.k");
    // The property is gone, and its newest retained event is the one that
    // removed it: `value` null at the removing commit.
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "k"),
        event(&db, removal, Value::Null)
    );
}

#[test]
fn remove_then_reset_makes_the_reset_the_newest_event() {
    let db = gated();
    let mut c = db.connect().unwrap();
    committed(&mut c, "CREATE (n:P {k: 1})");
    committed(&mut c, "MATCH (n:P) REMOVE n.k");
    let reset = committed(&mut c, "MATCH (n:P) SET n.k = 2");
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "k"),
        event(&db, reset, Value::Int(2))
    );
}

#[test]
fn merge_on_match_set_is_the_propertys_event() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let birth = committed(&mut c, "CREATE (n:P {name: 'Ada'})");
    let merge = committed(&mut c, "MERGE (n:P {name: 'Ada'}) ON MATCH SET n.age = 39");
    // The matched node's `age` was installed by the MERGE's commit; the
    // merge key keeps its birth.
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "age"),
        event(&db, merge, Value::Int(39))
    );
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "name"),
        event(&db, birth, Value::String("Ada".into()))
    );
}

#[test]
fn two_properties_written_in_one_transaction_carry_the_same_commit() {
    let db = gated();
    let mut c = db.connect().unwrap();
    committed(&mut c, "CREATE (n:P {a: 0, b: 0})");
    // One statement, one whole-row version: both keys' events name the
    // same commit.
    c.begin().unwrap();
    c.query("MATCH (n:P) SET n.a = 1, n.b = 2").unwrap();
    let bookmark = c.commit().unwrap().expect("the transaction wrote");
    let both: u64 = bookmark.strip_prefix("relate:").unwrap().parse().unwrap();
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "a"),
        event(&db, both, Value::Int(1))
    );
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "b"),
        event(&db, both, Value::Int(2))
    );
}

#[test]
fn provenance_answers_relationships_with_the_same_contract() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let birth = committed(&mut c, "CREATE (:A)-[r:KNOWS {since: 2020}]->(:B)");
    let note = committed(&mut c, "MATCH ()-[r:KNOWS]->() SET r.note = 'why'");
    assert_eq!(
        provenance_of(&mut c, "()-[element:KNOWS]->()", "since"),
        event(&db, birth, Value::Int(2020))
    );
    assert_eq!(
        provenance_of(&mut c, "()-[element:KNOWS]->()", "note"),
        event(&db, note, Value::String("why".into()))
    );
}

#[test]
fn the_event_map_joins_the_committing_transactions_metadata() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // The metadata channel rides the commit the property was written in,
    // so the event's `metadata` cell is that transaction's own map.
    c.begin().unwrap();
    c.query("CALL tx.setMetaData({by: 'job', run: 7})").unwrap();
    c.query("CREATE (n:P {k: 1})").unwrap();
    let bookmark = c.commit().unwrap().expect("the transaction wrote");
    let seeded: u64 = bookmark.strip_prefix("relate:").unwrap().parse().unwrap();
    assert_eq!(
        provenance_of(&mut c, "(element:P)", "k"),
        event(&db, seeded, Value::Int(1))
    );
    // `event` read the record's metadata: assert it is the seeded map,
    // not an empty one, so the join above is proven non-vacuous.
    let record = db
        .transaction_records()
        .unwrap()
        .into_iter()
        .find(|record| record.commit_ts == seeded)
        .unwrap();
    assert_eq!(
        record.metadata.as_ref().and_then(|m| m.get("by")),
        Some(&Value::String("job".into()))
    );
}

#[test]
fn provenance_is_null_without_a_retained_event_and_propagates_nulls() {
    let mut c = gated().connect().unwrap();
    c.query("CREATE (n:P {a: 1})").unwrap();
    // A key no version of the element ever carried has no event.
    assert_eq!(provenance_of(&mut c, "(element:P)", "missing"), Value::Null);
    // Cypher's null propagation: a null element or key answers null.
    let result = c
        .query("MATCH (n:P) RETURN provenance(null, 'k') AS a, provenance(n, null) AS b")
        .unwrap();
    assert_eq!(result.rows[0][0], Value::Null);
    assert_eq!(result.rows[0][1], Value::Null);
}

#[test]
fn provenance_type_and_arity_errors_are_the_ordinary_function_errors() {
    let mut c = gated().connect().unwrap();
    c.query("CREATE (n:P {a: 1})").unwrap();
    for query in [
        // The row's arity contract fires before any argument evaluates.
        "RETURN provenance(1)",
        "RETURN provenance(1, 'k', 3)",
        // Wrong-typed arguments.
        "RETURN provenance(1, 'k')",
        "MATCH (n:P) RETURN provenance(n, 5)",
    ] {
        let error = c
            .query(query)
            .expect_err(&format!("`{query}` must be rejected"))
            .to_string();
        assert!(
            error.contains("InvalidNumberOfArguments") || error.contains("InvalidArgumentType"),
            "`{query}`: the rejection must be an ordinary function error, got {error}"
        );
        assert!(
            !error.contains("RELATE_PREVIEW"),
            "`{query}`: the rejection must not advertise the gated name, got {error}"
        );
    }
}

#[test]
fn show_functions_lists_the_provenance_row_under_the_opt_in() {
    let mut c = gated().connect().unwrap();
    let result = c
        .query(
            "SHOW FUNCTIONS YIELD name, category, signature \
             WHERE name = 'provenance' RETURN name, category, signature",
        )
        .unwrap();
    let [row] = &result.rows[..] else {
        panic!("expected one provenance row, got {:?}", result.rows)
    };
    assert_eq!(row[0], Value::String("provenance".into()));
    assert_eq!(row[1], Value::String("Scalar".into()));
    assert_eq!(
        row[2],
        Value::String("provenance(element :: NODE | RELATIONSHIP, key :: STRING) :: MAP".into())
    );
}

// One file per concern (the `constraint_validation.rs` pattern): the
// history()/retention half lives in its own module file, and so does the
// typed-records half.
#[path = "value_provenance/history_retention.rs"]
mod history_retention;
#[path = "value_provenance/typed_records.rs"]
mod typed_records;

/// The roundtrip body every store backend runs: seed a transaction with
/// metadata, checkpoint (or not — `checkpoint` decides), close, reopen,
/// and assert the retained records survived. The reopen keeps the
/// feature's opt-in, so the reopened store keeps retaining.
fn records_survive_reopen(db: Database, reopen: impl Fn() -> Database, checkpoint: bool) {
    let mut c = db.connect().unwrap();
    c.set_tx_metadata(by_app()).unwrap();
    c.begin().unwrap();
    // The created node's id is the codec's to assign (the markdown vault
    // numbers its first node differently than a fresh in-memory store),
    // so the expectation reads it back instead of assuming zero.
    let created = c
        .query("CREATE (n:Person {name: 'Ada'}) RETURN id(n) AS id")
        .unwrap();
    let id = match &created.rows[0][0] {
        Value::Int(id) => id,
        other => panic!("id(n) was {other:?}"),
    };
    c.commit().unwrap().expect("the transaction wrote");
    drop(c);
    if checkpoint {
        db.checkpoint().unwrap();
        drop(db);
    } else {
        // A crash after the WAL fsync: the commit lives only in the log.
        db.leak_without_checkpoint();
    }
    let db = reopen();
    let records = db.transaction_records().unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].metadata.as_ref(), Some(&by_app()));
    assert_eq!(
        records[0].changed_keys.as_deref(),
        Some(&[node_key(*id, "name")][..])
    );
    // The retained store keeps retaining: a commit after the reopen
    // joins the table beside the recovered one.
    let mut c = db.connect().unwrap();
    c.set_tx_metadata(BTreeMap::from([(
        "by".to_string(),
        Value::String("later".into()),
    )]))
    .unwrap();
    c.query("CREATE (:Person {name: 'Bob'})").unwrap();
    assert_eq!(db.transaction_records().unwrap().len(), 2);
}

/// The checkpoint-restore matrix across every compiled codec — the
/// `tests/ontology_store.rs` codec-matrix pattern: each backend hosts
/// whichever codec set it can, `open` contributing the reopen closure.
fn roundtrips_compiled_codecs<R>(open: impl Fn(SnapshotCodecKind) -> R, checkpoint: bool)
where
    R: Fn() -> Database,
{
    common::set_preview_once("value-provenance");
    for codec in SnapshotCodecKind::compiled().iter().copied() {
        let reopen = open(codec);
        records_survive_reopen(reopen(), &reopen, checkpoint);
    }
}

#[test]
fn native_dir_checkpoint_restore_roundtrips_every_compiled_codec() {
    roundtrips_compiled_codecs(
        |codec| {
            let dir = tempfile::tempdir().unwrap();
            move || {
                Database::open_with(dir.path().to_str().unwrap(), common::store::opts(codec))
                    .unwrap()
            }
        },
        true,
    );
}

#[test]
fn native_dir_wal_replay_roundtrips_every_compiled_codec() {
    roundtrips_compiled_codecs(
        |codec| {
            let dir = tempfile::tempdir().unwrap();
            move || {
                Database::open_with(dir.path().to_str().unwrap(), common::store::opts(codec))
                    .unwrap()
            }
        },
        false,
    );
}

#[test]
fn memory_vfs_checkpoint_restore_roundtrips_every_compiled_codec() {
    roundtrips_compiled_codecs(
        |codec| {
            let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
            move || Database::open_vfs(Arc::clone(&vfs), common::store::opts(codec)).unwrap()
        },
        true,
    );
}

#[test]
fn bundle_checkpoint_restore_roundtrips_every_compiled_codec() {
    roundtrips_compiled_codecs(
        |codec| {
            let dir = tempfile::tempdir().unwrap();
            let uri = dir.path().join("graph.rl");
            move || Database::open_with(uri.to_str().unwrap(), common::store::opts(codec)).unwrap()
        },
        true,
    );
}

#[test]
fn records_dedup_across_two_family_parts() {
    common::set_preview_once("value-provenance");
    // The mid-checkpoint-crash shape: the same committed record readable
    // from two family parts (the crash window between a part publish and
    // the manifest that would have superseded it) — the table dedups by
    // `commit_ts`, so the reopen retains each commit once. The planted
    // second part is the byte-level crash simulation the
    // `recovery_fails_closed` suite uses for its stores.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db =
            Database::open_with(path, common::store::opts(SnapshotCodecKind::default())).unwrap();
        let mut c = db.connect().unwrap();
        c.set_tx_metadata(by_app()).unwrap();
        c.query("CREATE (:Person {name: 'Ada'})").unwrap();
        db.checkpoint().unwrap();
    }
    {
        // Plant the published part's bytes a second time under another
        // valid window name — the two-parts-hold-one-record state.
        let vfs = common::store::store_vfs(dir.path());
        let names: Vec<String> = vfs.list_dir("transactions").unwrap();
        let file_name = names.first().expect("the checkpoint published a part");
        let part = format!("transactions/{file_name}");
        let bytes = relate_storage::read_to_vec(&*vfs, &part).unwrap();
        let planted = format!("transactions/{:020}-{:020}.json", 2, 9);
        let mut file = vfs
            .open(&planted, relate_storage::VfsOpen::create_truncate())
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
    }
    let db = Database::open_with(path, common::store::opts(SnapshotCodecKind::default())).unwrap();
    let records: Vec<TransactionRecord> = db.transaction_records().unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].metadata.as_ref(), Some(&by_app()));
}

// ---- db.transactions (ADR-0610) ----

/// The bookmark sequence of one committed auto-commit write — the commit
/// the record's `commitTs` names (`relate:<n>`'s `n` is the commit
/// timestamp).
fn sequence_of(bookmark: Option<&str>) -> u64 {
    bookmark
        .and_then(|b| b.strip_prefix("relate:"))
        .and_then(|n| n.parse().ok())
        .expect("the bookmark names the commit sequence")
}

/// The `{agent: <agent>}` metadata map the procedure fixtures seed.
fn agent(agent: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([("agent".to_string(), Value::String(agent.into()))])
}

/// One changed-key cell, in the wire spelling the procedure renders.
fn changed_key(element_id: &str, key: &str) -> Value {
    Value::Map(
        BTreeMap::from([
            ("elementId".to_string(), Value::String(element_id.into())),
            ("key".to_string(), Value::String(key.into())),
        ])
        .into(),
    )
}

/// Three committed writes — `a` creates, `b` creates, `a` deletes its own
/// node — each tagged with its agent through the embedded channel. The
/// tuple is (the load node's id, the serve node's id, the three bookmark
/// sequences).
fn three_agent_commits(c: &mut relate::Connection) -> (i64, i64, Vec<u64>) {
    let mut sequences = Vec::new();
    let mut ids = Vec::new();
    for (who, cypher) in [
        ("a", "CREATE (t:Task {title: 'load'}) RETURN id(t) AS id"),
        ("b", "CREATE (t:Task {title: 'serve'}) RETURN id(t) AS id"),
    ] {
        c.set_tx_metadata(agent(who)).unwrap();
        let created = c.query(cypher).unwrap();
        ids.push(match created.rows[0][0] {
            Value::Int(id) => id,
            ref other => panic!("id(t) was {other:?}"),
        });
        sequences.push(sequence_of(created.bookmark.as_deref()));
    }
    // `a` retracts its own write: the delete's record still names the
    // deleted element's changed keys.
    c.set_tx_metadata(agent("a")).unwrap();
    let deleted = c.query("MATCH (t:Task {title: 'load'}) DELETE t").unwrap();
    sequences.push(sequence_of(deleted.bookmark.as_deref()));
    (ids[0], ids[1], sequences)
}

/// The matching form (ADR-0610): every retained record with the five
/// yield columns, ascending by `lsn`, deleted elements included in
/// `changedKeys` — the `dbms_procedures.rs` pattern of asserting the
/// header and the rows.
#[test]
fn db_transactions_returns_each_retained_record_with_the_adrs_yields() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let (load_id, serve_id, sequences) = three_agent_commits(&mut c);

    // Without YIELD the call answers the signature's five columns.
    let result = c.query("CALL db.transactions()").unwrap();
    assert_eq!(
        result.header.columns,
        ["lsn", "commitTs", "type", "metadata", "changedKeys"]
    );
    assert_eq!(result.rows.len(), 3);

    // Ascending by lsn — the row order the contract records.
    let lsns: Vec<i64> = result
        .rows
        .iter()
        .map(|row| match row[0] {
            Value::Int(lsn) => lsn,
            ref other => panic!("lsn was {other:?}"),
        })
        .collect();
    assert!(lsns.windows(2).all(|pair| pair[0] < pair[1]), "{lsns:?}");

    // The other four columns, row by row: the commitTs the bookmarks
    // name, the provenance type the embedded channel hard-sets, the
    // metadata map each channel seeded, and the changed keys with
    // element ids in the `4:rl-id:n<id>` wire spelling — the deleted
    // node's record included.
    let agent_map = |who: &str| {
        Value::Map(BTreeMap::from([("agent".to_string(), Value::String(who.into()))]).into())
    };
    assert_eq!(
        result.rows,
        vec![
            vec![
                Value::Int(lsns[0]),
                Value::Int(sequences[0] as i64),
                Value::String("provenance".into()),
                agent_map("a"),
                Value::List(vec![changed_key(&format!("4:rl-id:n{load_id}"), "title")].into()),
            ],
            vec![
                Value::Int(lsns[1]),
                Value::Int(sequences[1] as i64),
                Value::String("provenance".into()),
                agent_map("b"),
                Value::List(vec![changed_key(&format!("4:rl-id:n{serve_id}"), "title")].into()),
            ],
            vec![
                Value::Int(lsns[2]),
                Value::Int(sequences[2] as i64),
                Value::String("provenance".into()),
                agent_map("a"),
                Value::List(vec![changed_key(&format!("4:rl-id:n{load_id}"), "title")].into()),
            ],
        ]
    );

    // The call contract stands like any builtin's: an unknown YIELD
    // column fails validation.
    common::assert_unknown_output_fails("CALL db.transactions() YIELD nope RETURN nope", "nope");
}

/// The no-match filter: a `since` above every retained commit's
/// timestamp answers zero rows.
#[test]
fn a_since_above_every_record_answers_zero_rows() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let (_, _, sequences) = three_agent_commits(&mut c);
    let beyond = sequences.iter().copied().max().unwrap() + 10;
    let result = c
        .query(&format!(
            "CALL db.transactions({beyond}) YIELD lsn RETURN lsn"
        ))
        .unwrap();
    assert_eq!(result.header.columns, ["lsn"]);
    assert!(result.rows.is_empty(), "{:?}", result.rows);
}

/// The filter semantics ADR-0610 records: `since` is the inclusive
/// `commitTs` lower bound (the record at the bound stays), 0 (the
/// default) answers every retained record, further filtering goes
/// through YIELD/WHERE, and a wrong-typed bound keeps the call-shape
/// rejection.
#[test]
fn since_is_the_inclusive_bound_and_where_filters_further() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let (_, _, sequences) = three_agent_commits(&mut c);

    // The inclusive bound: the record at the bound itself and everything
    // after it.
    let from_b = c
        .query(&format!(
            "CALL db.transactions({}) YIELD commitTs RETURN commitTs",
            sequences[1]
        ))
        .unwrap();
    assert_eq!(
        from_b.rows,
        vec![
            vec![Value::Int(sequences[1] as i64)],
            vec![Value::Int(sequences[2] as i64)],
        ]
    );
    // The default bound answers every retained record.
    let all = c
        .query("CALL db.transactions() YIELD commitTs RETURN commitTs")
        .unwrap();
    assert_eq!(all.rows.len(), 3);

    // Further filtering through WHERE — the audit question itself: one
    // agent's commits only.
    let of_b = c
        .query(
            "CALL db.transactions() YIELD commitTs, metadata \
             WHERE metadata.agent = 'b' RETURN commitTs",
        )
        .unwrap();
    assert_eq!(of_b.rows, vec![vec![Value::Int(sequences[1] as i64)]]);

    // A wrong-typed bound keeps the call-shape rejection.
    let error = c
        .query("CALL db.transactions('not-an-int')")
        .expect_err("a STRING since is rejected");
    assert!(error.to_string().contains("InvalidArgumentType"), "{error}");
}

/// The catalogue side of the gate (ADR-0610): behind the opt-in the row
/// is listed with the contract's signature — `since` defaulted, the five
/// yield columns in order.
#[test]
fn show_procedures_lists_db_transactions_behind_the_opt_in() {
    let mut c = gated().connect().unwrap();
    let result = c
        .query(
            "SHOW PROCEDURES YIELD name, signature, mode \
             WHERE name = 'db.transactions' RETURN signature, mode",
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Value::String(
                "db.transactions(since = 0 :: INTEGER) :: (lsn :: INTEGER, commitTs :: INTEGER, \
                 type :: STRING, metadata :: MAP, changedKeys :: LIST<MAP>)"
                    .into()
            ),
            Value::String("READ".into()),
        ]]
    );
}
