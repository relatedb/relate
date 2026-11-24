//! The retract recipe (ADR-0610): two simulated agents write with their
//! names in the transaction metadata, then one statement retracts
//! everything one of them wrote — `db.transactions()` lists the retained
//! records, the `WHERE` picks the agent's commits, their changed keys
//! name the touched elements, and the retraction deletes those elements.
//! The retracted state is asserted, and the retraction is itself
//! auditable: its own retained record carries the supervisor's metadata.
//!
//! Run with `cargo run -p relate --example retract_recipe`.

use relate::{Database, Value};

fn main() -> relate::Result<()> {
    // The whole machinery is the `value-provenance` preview feature's
    // (ADR-0371's registry, ADR-0610): without the opt-in
    // `db.transactions` fails with the authored preview error and the
    // commits record no metadata, so there is nothing to query. This
    // in-process mutation is example-only: a library must not set the
    // process environment — the process owner sets
    // `RELATE_PREVIEW=value-provenance` before open, the documented
    // deployment path.
    std::env::set_var("RELATE_PREVIEW", "value-provenance");
    let db = Database::open(":memory:")?;
    let mut conn = db.connect()?;

    // Two simulated agents. Each writes in one transaction with its name
    // in the metadata — the "why" of the write, merged from any of the
    // three channels; `tx.setMetaData` is the procedure channel here.
    conn.begin()?;
    conn.query("CALL tx.setMetaData({agent: 'ingest'})")?;
    conn.query("CREATE (:Task {title: 'load', queue: 'nightly'})")?;
    conn.query("CREATE (:Task {title: 'transform', queue: 'nightly'})")?;
    conn.commit()?.expect("the ingest transaction wrote");

    conn.begin()?;
    conn.query("CALL tx.setMetaData({agent: 'serving'})")?;
    conn.query("CREATE (:Task {title: 'serve', queue: 'always'})")?;
    conn.commit()?.expect("the serving transaction wrote");

    // The retract: one statement. `db.transactions()` yields the retained
    // records ascending by lsn; the WHERE keeps one agent's commits; the
    // UNWIND flattens their changed keys; the collect dedupes the element
    // ids; the MATCH finds those elements — by their element ids, the
    // wire spelling the records render — and DETACH DELETE removes them.
    conn.begin()?;
    conn.query("CALL tx.setMetaData({agent: 'supervisor'})")?;
    conn.query(
        "CALL db.transactions() YIELD commitTs, metadata, changedKeys \
         WHERE metadata.agent = 'ingest' \
         UNWIND changedKeys AS ck \
         WITH collect(DISTINCT ck.elementId) AS touched \
         MATCH (t) WHERE elementId(t) IN touched \
         DETACH DELETE t",
    )?;
    conn.commit()?.expect("the retract transaction wrote");

    // The retracted state: everything the ingest agent wrote is gone,
    // the serving agent's work stands.
    let titles = conn.query("MATCH (t:Task) RETURN t.title AS title ORDER BY title")?;
    assert_eq!(
        titles.rows,
        vec![vec![Value::String("serve".into())]],
        "the retract removed exactly the ingest agent's writes: {:?}",
        titles.rows
    );

    // The retraction is itself part of the audit surface: its own
    // retained record carries the supervisor's metadata, and its changed
    // keys name the same elements — their deletion.
    let records = conn.query(
        "CALL db.transactions() YIELD commitTs, metadata, changedKeys \
         WHERE metadata.agent = 'supervisor' \
         RETURN changedKeys AS keys",
    )?;
    assert_eq!(records.rows.len(), 1);
    let Value::List(keys) = &records.rows[0][0] else {
        panic!("changedKeys is a list, got {:?}", records.rows[0][0])
    };
    // Two deleted nodes, each contributing its two property keys: the
    // distinct changed keys of the retract, sorted by (kind, id, key).
    assert_eq!(keys.len(), 4, "{keys:?}");
    Ok(())
}
