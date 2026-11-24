//! ADR-0610's closed side — the store without the `value-provenance`
//! opt-in: the two compat procedures behave exactly as before the
//! feature (`tx.setMetaData` the void no-op, `tx.getMetaData` the fixed
//! `source: 'cip230'` row), the embedded seed fails the authored preview
//! error, a WAL and a checkpoint written under load contain no
//! transaction-record bytes (the zero-cost guarantee), the collapse
//! — an open without the opt-in drops retained history at its
//! checkpoint — holds against provenance-carrying bytes planted by hand
//! (the byte-level store crafting the `recovery_fails_closed` suite
//! uses), and the `db.transactions` procedure stays gated with the whole
//! machinery: a call fails the authored preview error and `SHOW
//! PROCEDURES` lists no row.
//!
//! This file is its own test binary and pins the unset-env precondition
//! first: `RELATE_PREVIEW` is process-global (ADR-0371), and a leaked
//! opt-in would silently change what these fixtures test. The enabled
//! side lives in `tests/value_provenance.rs`.

mod common;

use std::collections::BTreeMap;

use relate::{Database, Value};
use relate_storage::SnapshotCodecKind;

/// The closed-gate precondition (ADR-0371): no leaked opt-in.
#[test]
fn tx_get_metadata_is_the_fixed_cip230_row() {
    relate::test_opts::assert_preview_unset();
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    // Auto-commit read…
    let result = c
        .query("CALL tx.getMetaData() YIELD metadata RETURN metadata")
        .unwrap();
    assert_eq!(
        result.rows[0][0],
        Value::Map(BTreeMap::from([("source".to_string(), Value::String("cip230".into()))]).into())
    );
    // …and inside an explicit transaction — the compat form stands.
    c.begin().unwrap();
    c.query("CALL tx.setMetaData({by: 'x'})").unwrap();
    let result = c
        .query("CALL tx.getMetaData() YIELD metadata RETURN metadata")
        .unwrap();
    assert_eq!(
        result.rows[0][0],
        Value::Map(BTreeMap::from([("source".to_string(), Value::String("cip230".into()))]).into())
    );
    c.commit().unwrap();
    // The call-shape rejection the corpus pins is unchanged.
    let error = c
        .query("CALL tx.setMetaData(1)")
        .expect_err("the type check stands without the opt-in");
    assert!(error.to_string().contains("InvalidArgumentType"), "{error}");
    // And nothing was retained: the no-op stored no map, derived no keys.
    assert!(db.transaction_records().unwrap().is_empty());
}

#[test]
fn the_embedded_seed_fails_the_authored_preview_error() {
    relate::test_opts::assert_preview_unset();
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    let error = c
        .set_tx_metadata(BTreeMap::from([(
            "by".to_string(),
            Value::String("app".into()),
        )]))
        .expect_err("the embedded channel is gated");
    assert_eq!(
        error.to_string(),
        "semantic error: SemanticError: preview feature 'value-provenance' is not enabled: \
         set RELATE_PREVIEW=value-provenance"
    );
    // The failed seed stored nothing: a later write commits no metadata.
    c.query("CREATE (:Person {name: 'Ada'})").unwrap();
    assert!(db.transaction_records().unwrap().is_empty());
}

/// The typed twin gates with its whole machinery (ADR-0610): exactly the
/// untyped channel's authored preview error on a store without the
/// opt-in — the typed name buys no ungated seeding.
#[test]
fn the_typed_seed_fails_the_authored_preview_error() {
    relate::test_opts::assert_preview_unset();
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    let error = c
        .set_tx_metadata_of_type("audit-trail", BTreeMap::new())
        .expect_err("the typed channel is gated like the untyped one");
    assert_eq!(
        error.to_string(),
        "semantic error: SemanticError: preview feature 'value-provenance' is not enabled: \
         set RELATE_PREVIEW=value-provenance"
    );
    // The failed seed stored nothing, map and type both.
    c.query("CREATE (:Person {name: 'Ada'})").unwrap();
    assert!(db.transaction_records().unwrap().is_empty());
}

/// The zero-cost guarantee (ADR-0610): a store that never opts in is
/// indistinguishable from one on a build without the feature — a WAL
/// written under load carries no transaction-record bytes, and neither
/// does the checkpoint.
#[test]
fn wal_and_checkpoint_carry_no_transaction_record_bytes() {
    relate::test_opts::assert_preview_unset();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db =
            Database::open_with(path, common::store::opts(SnapshotCodecKind::default())).unwrap();
        let mut c = db.connect().unwrap();
        // Load: one batched explicit transaction plus auto-commit writes,
        // with metadata-channel traffic that must store nothing.
        c.begin().unwrap();
        for i in 0..30 {
            c.query(&format!("CREATE (:Load {{n: {i}}})")).unwrap();
        }
        c.query("CALL tx.setMetaData({by: 'ignored'})").unwrap();
        c.commit().unwrap();
        c.query("CREATE (:Tail {n: 0})").unwrap();
        // A crash after the WAL fsync: the whole load lives in the log.
        db.leak_without_checkpoint();
    }
    // The WAL's records carry no provenance field bytes.
    {
        let vfs = common::store::store_vfs(dir.path());
        let segment = common::store::active_segment(&*vfs);
        let bytes = relate_storage::read_to_vec(&*vfs, &segment).unwrap();
        assert!(
            !bytes
                .windows(b"\"metadata\"".len())
                .any(|w| w == b"\"metadata\""),
            "the WAL carries a metadata field"
        );
        assert!(
            !bytes
                .windows(b"\"changed_keys\"".len())
                .any(|w| w == b"\"changed_keys\""),
            "the WAL carries a changed_keys field"
        );
    }
    // And neither does the checkpoint: the store holds no family objects
    // beside its snapshot image.
    let db = Database::open_with(path, common::store::opts(SnapshotCodecKind::default())).unwrap();
    db.checkpoint().unwrap();
    assert!(db.transaction_records().unwrap().is_empty());
    let vfs = common::store::store_vfs(dir.path());
    for name in vfs.list_dir("").unwrap() {
        assert!(
            name != "transactions" && name != "chains",
            "the checkpoint published a retained-history family: {name}"
        );
    }
}

/// The collapse (ADR-0610): provenance is bounded by the last
/// provenance-enabled open — an open without the opt-in loads the
/// retained history its store carries (the recorded transition cost),
/// then drops it, in memory and on disk, at its checkpoint. The
/// provenance-carrying bytes are planted by hand, the way a store
/// written by an opted-in binary looks on disk: a retained-transaction
/// part, and a retained-chain part holding a deleted element's
/// tombstoned chain (the shape the checkpoint's GC would have dropped
/// on a store that never opted in).
#[test]
fn an_opt_out_checkpoint_collapses_retained_history() {
    relate::test_opts::assert_preview_unset();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    common::store::seed_store(&path, SnapshotCodecKind::Json);
    // One retained record: the committed shape an opted-in store writes
    // — a metadata map (tagged values) and derived changed keys.
    let part = concat!(
        "{\"records\":[{\"lsn\":4,\"commit_ts\":4,",
        "\"metadata\":{\"by\":{\"t\":\"string\",\"v\":\"app\"}},",
        "\"changed_keys\":[{\"kind\":\"node\",\"id\":2,\"key\":\"name\"}]}]}",
    );
    // One retained chain: node 9 born at commit 3, deleted at commit 4 —
    // a tombstoned chain the image never held, the family's part shape
    // (ADR-0610's `chains/` grammar).
    let chains_part = concat!(
        "{\"nodes\":[{\"id\":9,\"versions\":[{\"xmin\":3,\"xmax\":4,",
        "\"labels\":[],\"properties\":{\"name\":{\"t\":\"string\",\"v\":\"gone\"}}}]}],",
        "\"rels\":[]}",
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
        vfs.create_dir_all("chains").unwrap();
        let chain_name = "chains/00000000000000000000-00000000000000000004.json";
        let mut file = vfs
            .open(chain_name, relate_storage::VfsOpen::create_truncate())
            .unwrap();
        file.write_all(chains_part.as_bytes()).unwrap();
        file.sync_all().unwrap();
        vfs.sync_dir("chains").unwrap();
    }
    // The opt-out open still loads the retained history — the table is
    // populated from the store bytes before the preview set resolves.
    let db = Database::open_with(
        path.to_str().unwrap(),
        common::store::opts(SnapshotCodecKind::Json),
    )
    .unwrap();
    let records = db.transaction_records().unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        records[0].metadata.as_ref().and_then(|map| map.get("by")),
        Some(&Value::String("app".into()))
    );
    // Its checkpoint collapses: the table clears and the durable families
    // go with it (the empty family directories may remain — they carry
    // no bytes and the next provenance era reuses them).
    db.checkpoint().unwrap();
    assert!(
        db.transaction_records().unwrap().is_empty(),
        "the collapse clears the retained table"
    );
    let vfs = common::store::store_vfs(&path);
    let parts: Vec<String> = vfs.list_dir("transactions").unwrap_or_default();
    assert!(
        parts.is_empty(),
        "the collapse drops the durable family: {parts:?}"
    );
    let chains: Vec<String> = vfs.list_dir("chains").unwrap_or_default();
    assert!(
        chains.is_empty(),
        "the collapse drops the retained-chain family: {chains:?}"
    );
}

// ---- history() (ADR-0610): the closed function gate ----

/// The function names gate with the whole machinery (ADR-0610):
/// `history()`, like `provenance()`, is the ordinary unknown function
/// while the opt-in is off — 42N48, never a preview advertisement. The
/// `SHOW FUNCTIONS` side is covered table-driven by
/// `function_preview_closed.rs`'s derived gated-name walk.
#[test]
fn history_is_the_ordinary_unknown_function_without_the_opt_in() {
    relate::test_opts::assert_preview_unset();
    let mut c = Database::open(":memory:").unwrap().connect().unwrap();
    c.query("CREATE (n:P {k: 1})").unwrap();
    for query in [
        "RETURN history(1, 'k') AS x",
        "MATCH (n:P) RETURN history(n, 'k') AS x",
        "CALL { RETURN history(1, 'k') AS x } RETURN x",
    ] {
        let error = c
            .query(query)
            .expect_err(&format!("`{query}` must be rejected without the opt-in"))
            .to_string();
        assert!(
            error.contains("UnknownFunction"),
            "`{query}`: the rejection must be the ordinary unknown function, got {error}"
        );
        assert!(
            !error.contains("RELATE_PREVIEW"),
            "`{query}`: the rejection must not advertise the gated name, got {error}"
        );
    }
}

// ---- db.transactions (ADR-0610): the closed gate ----

/// The procedure gates with its whole machinery (ADR-0610): a call fails
/// the authored preview error — ahead of arity and `YIELD` validation,
/// from every nested position — while a store that never opted in has no
/// rows to list anyway.
#[test]
fn db_transactions_fails_the_authored_preview_error() {
    relate::test_opts::assert_preview_unset();
    for query in [
        "CALL db.transactions()",
        "CALL db.transactions(4)",
        "CALL db.transactions(1, 2)",
        "CALL db.transactions() YIELD lsn RETURN lsn",
        "EXPLAIN CALL db.transactions()",
        "CALL { CALL db.transactions() YIELD lsn RETURN lsn } RETURN lsn",
    ] {
        common::assert_preview_closed(relate_core::PreviewFeature::ValueProvenance, query);
    }
}

/// Discovery agrees with callability (ADR-0610): `SHOW PROCEDURES` lists
/// no `db.transactions` row in the composed forms, while the rest of the
/// catalog is untouched.
#[test]
fn show_procedures_omits_db_transactions() {
    relate::test_opts::assert_preview_unset();
    let mut c = Database::open(":memory:").unwrap().connect().unwrap();
    let named = c
        .query("SHOW PROCEDURES YIELD name WHERE name = 'db.transactions' RETURN name")
        .unwrap();
    assert!(named.rows.is_empty(), "{:?}", named.rows);
    let counted = c
        .query("SHOW PROCEDURES YIELD name WHERE name = 'db.transactions' RETURN count(*) AS c")
        .unwrap();
    assert_eq!(counted.rows, vec![vec![Value::Int(0)]]);
    let result = c
        .query("SHOW PROCEDURES YIELD name WHERE name = 'db.labels' RETURN count(*) AS c")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}
