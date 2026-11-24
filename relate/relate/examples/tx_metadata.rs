//! Transaction metadata through the ADR-0610 channels: the embedded
//! `Connection::set_tx_metadata` and its typed twin
//! `Connection::set_tx_metadata_of_type`, the `CALL tx.setMetaData` /
//! `CALL tx.getMetaData` procedure pair, and the derived changed keys —
//! all riding one per-transaction cell into the commit's WAL record,
//! retained for the audit surface (the `value-provenance` preview
//! feature's machinery).
//!
//! Run with `cargo run -p relate --example tx_metadata`.

use std::collections::BTreeMap;

use relate::{bookmark_sequence, ChangedKey, ChangedKeyKind, Database, Value};

fn main() -> relate::Result<()> {
    // The whole machinery is the `value-provenance` preview feature's
    // (ADR-0371's registry, ADR-0610): without the opt-in
    // `set_tx_metadata` fails with the authored preview error,
    // `tx.setMetaData` is the void compat no-op, and no record carries
    // metadata. This in-process mutation is example-only: a library must
    // not set the process environment — the process owner sets
    // `RELATE_PREVIEW=value-provenance` before open, the documented
    // deployment path.
    std::env::set_var("RELATE_PREVIEW", "value-provenance");
    let db = Database::open(":memory:")?;
    let mut conn = db.connect()?;

    // The embedded channel seeds the map; a transaction's commit carries
    // it and clears it after.
    let mut metadata = BTreeMap::new();
    metadata.insert("by".to_string(), Value::String("app".into()));
    conn.set_tx_metadata(metadata)?;
    conn.begin()?;
    let created = conn.query("CREATE (n:Person {name: 'Ada'}) RETURN id(n) AS id")?;
    let id = match &created.rows[0][0] {
        Value::Int(id) => *id,
        other => panic!("id(n) was {other:?}"),
    };
    // The bookmark's sequence through the crate's one decoder
    // (doc-hidden beside the record types): the commitTs its record
    // carries.
    let bookmark = conn.commit()?.expect("the transaction wrote");
    let sequence: u64 =
        bookmark_sequence(&bookmark).expect("the bookmark names the commit sequence");

    // The retained record of that commit: the merged map beside the
    // commit's derived changed property keys — the audit surface's input.
    let records = db.transaction_records()?;
    let record = records
        .iter()
        .find(|record| record.commit_ts == sequence)
        .expect("the commit retained a record");
    assert_eq!(
        record.metadata.as_ref().and_then(|m| m.get("by")),
        Some(&Value::String("app".into()))
    );
    assert_eq!(
        record.changed_keys.as_deref(),
        Some(
            &[ChangedKey {
                kind: ChangedKeyKind::Node,
                id,
                key: "name".to_string()
            }][..]
        )
    );

    // The procedure channel: `setMetaData` merges (later keys win) and
    // `getMetaData` returns the live map, inside one transaction.
    conn.begin()?;
    conn.query("CALL tx.setMetaData({by: 'job', run: 7})")?;
    conn.query("CALL tx.setMetaData({by: 'retry'})")?;
    let live = conn.query("CALL tx.getMetaData() YIELD metadata RETURN metadata")?;
    assert_eq!(
        live.rows[0][0],
        Value::Map(
            BTreeMap::from([
                ("by".to_string(), Value::String("retry".into())),
                ("run".to_string(), Value::Int(7)),
            ])
            .into()
        )
    );
    // A rolled-back transaction's metadata never rides a later one.
    conn.rollback()?;

    // Cleared after: the next transaction's record carries no metadata.
    conn.begin()?;
    conn.query("CREATE (:Person {name: 'Bob'})")?;
    let second = conn.commit()?.expect("the second transaction wrote");
    let sequence: u64 = bookmark_sequence(&second).expect("the bookmark names the commit sequence");
    let record = db
        .transaction_records()?
        .into_iter()
        .find(|record| record.commit_ts == sequence)
        .expect("the second commit retained a record");
    assert_eq!(record.metadata, None);

    // The typed channel: the same merge, another family's record. The
    // transaction's record carries `audit-trail` instead of the
    // provenance family's own type, so it derives no changed keys and
    // joins no provenance()/history() event — `db.transactions` lists it
    // with its `type` and null `changedKeys`.
    let mut run_metadata = BTreeMap::new();
    run_metadata.insert("why".to_string(), Value::String("nightly-run".into()));
    conn.set_tx_metadata_of_type("audit-trail", run_metadata)?;
    conn.begin()?;
    conn.query("CREATE (:Person {name: 'Cyd'})")?;
    let third = conn.commit()?.expect("the third transaction wrote");
    let sequence: u64 = bookmark_sequence(&third).expect("the bookmark names the commit sequence");
    let record = db
        .transaction_records()?
        .into_iter()
        .find(|record| record.commit_ts == sequence)
        .expect("the third commit retained a record");
    assert_eq!(record.record_type().as_str(), "audit-trail");
    assert_eq!(record.changed_keys, None);
    // The typed name is validated at the channel: kebab-case, and the
    // provenance family's own name reserved for the untyped channels.
    assert!(conn
        .set_tx_metadata_of_type("Not Kebab", BTreeMap::new())
        .is_err());
    assert!(conn
        .set_tx_metadata_of_type("provenance", BTreeMap::new())
        .is_err());
    Ok(())
}
