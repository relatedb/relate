//! `provenance(element, key)` and `history(element, key [, maxNum])` end
//! to end (ADR-0610): the per-property provenance functions over the
//! retained version chains — a property's birth, its re-SET, its removal —
//! each event joining the committing transaction's retained record for
//! `commitTs`, `lsn` and `metadata`, the whole history and its
//! `maxNum`-bounded tail answering through a checkpoint and reopen with
//! real `xmin` preserved (the `value-provenance` preview feature's
//! machinery and retention).
//!
//! Run with `cargo run -p relate --example provenance`.

use std::collections::BTreeMap;

use relate::{bookmark_sequence, Database, Value};

fn main() -> relate::Result<()> {
    // `provenance()` and `history()` are gated by the `value-provenance`
    // preview feature (ADR-0610 through ADR-0371's registry): without
    // the opt-in the names are the ordinary unknown-function error
    // (42N48) and `SHOW FUNCTIONS` lists no row. This in-process
    // mutation is example-only: a library must not set the process
    // environment — the process owner sets
    // `RELATE_PREVIEW=value-provenance` before open, the documented
    // deployment path.
    std::env::set_var("RELATE_PREVIEW", "value-provenance");
    // A durable store on a temp directory, so the retention half below
    // can close and reopen it.
    let dir = std::env::temp_dir().join("relate-provenance-example");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).ok();
    let db = Database::open_with(
        dir.to_str().expect("a UTF-8 temp path"),
        relate::OpenOptions::default(),
    )?;
    let mut conn = db.connect()?;

    // The property is born in a transaction that carries metadata: its
    // first event joins that commit.
    conn.begin()?;
    conn.query("CALL tx.setMetaData({by: 'import'})")?;
    conn.query("CREATE (n:Product {name: 'beans', price: 3})")?;
    let birth = bookmark_sequence(&conn.commit()?.expect("the transaction wrote"))
        .expect("the bookmark names the commit sequence");

    // A later transaction sets a new price: the newest event of `price`
    // is this commit, and the untouched sibling `name` keeps its birth.
    conn.begin()?;
    conn.query("CALL tx.setMetaData({by: 'pricing'})")?;
    conn.query("MATCH (n:Product) SET n.price = 4")?;
    let priced = bookmark_sequence(&conn.commit()?.expect("the transaction wrote"))
        .expect("the bookmark names the commit sequence");

    // A third transaction removes the property: the removal is the newest
    // event — its `value` is null at the removing commit.
    conn.begin()?;
    conn.query("MATCH (n:Product) REMOVE n.price")?;
    let removed = bookmark_sequence(&conn.commit()?.expect("the transaction wrote"))
        .expect("the bookmark names the commit sequence");

    // The function answers the newest retained event of one key.
    let price = conn.query("MATCH (n:Product) RETURN provenance(n, 'price') AS p")?;
    let Value::Map(price) = &price.rows[0][0] else {
        panic!("provenance() returns a MAP");
    };
    assert_eq!(price["commitTs"], Value::Int(removed as i64));
    assert_eq!(price["value"], Value::Null);
    assert_eq!(price["metadata"], Value::Map(BTreeMap::new().into()));

    // Walk the history the example built: re-SET then removal are past
    // events, so re-creating the property and asking again shows the
    // newest; the sibling never moved off its birth commit.
    conn.query("MATCH (n:Product) SET n.price = 5")?;
    let again = conn.query("MATCH (n:Product) RETURN provenance(n, 'price') AS p")?;
    let Value::Map(again) = &again.rows[0][0] else {
        panic!("provenance() returns a MAP");
    };
    // The auto-commit write carried no metadata; its lsn is its own.
    let last = db
        .transaction_records()?
        .into_iter()
        .last()
        .expect("the auto-commit retained a record");
    assert_eq!(again["commitTs"], Value::Int(last.commit_ts as i64));
    assert_eq!(again["lsn"], Value::Int(last.lsn as i64));
    assert_eq!(again["value"], Value::Int(5));

    // The sibling property keeps the commit that wrote it, metadata and
    // all — a later write to another key never re-dates it.
    let name = conn.query("MATCH (n:Product) RETURN provenance(n, 'name') AS p")?;
    let Value::Map(name) = &name.rows[0][0] else {
        panic!("provenance() returns a MAP");
    };
    assert_eq!(name["commitTs"], Value::Int(birth as i64));
    assert_eq!(name["value"], Value::String("beans".into()));
    assert_eq!(
        name["metadata"],
        Value::Map(BTreeMap::from([("by".to_string(), Value::String("import".into()))]).into())
    );

    // The intermediate event is still reachable through the retained
    // records (the `db.transactions` audit surface's input): the pricing
    // commit sits between the birth and the removal.
    let records = db.transaction_records()?;
    assert!(records
        .iter()
        .any(|record| record.commit_ts == priced && record.metadata.is_some()));

    // A null element or key answers null (Cypher's null propagation), and
    // a key no version carried has no event.
    let nulls = conn.query(
        "MATCH (n:Product) \
         RETURN provenance(null, 'price') AS a, provenance(n, null) AS b, \
         provenance(n, 'nope') AS c",
    )?;
    assert_eq!(nulls.rows[0], vec![Value::Null, Value::Null, Value::Null]);

    // Retention (ADR-0610): while the opt-in is set, the GC horizon drop
    // exempts every version and the checkpoint persists the retained
    // version chains — so history() answers the whole ascending list,
    // and the same events survive the checkpoint/reopen below with the
    // real xmin of every version.
    let history = conn.query("MATCH (n:Product) RETURN history(n, 'price') AS h")?;
    let Value::List(history) = &history.rows[0][0] else {
        panic!("history() returns a LIST<MAP>");
    };
    // The four events the example built — birth, re-SET, removal and the
    // final re-set — each joining its own commit's record, so the
    // commitTs column is that version's real xmin.
    let commits = [birth, priced, removed, last.commit_ts];
    let values = [Value::Int(3), Value::Int(4), Value::Null, Value::Int(5)];
    assert_eq!(history.len(), commits.len());
    for (event, (&commit_ts, value)) in history.iter().zip(commits.iter().zip(values)) {
        let Value::Map(event) = event else {
            panic!("history() returns a LIST<MAP>");
        };
        assert_eq!(event["commitTs"], Value::Int(commit_ts as i64));
        assert_eq!(event["value"], value);
    }

    // The bounded form (ADR-0610's maxNum): the last two events of the
    // same walk, ascending — the bound rides the walk as a sliding
    // window over the collected events, so the tail answers without
    // materializing the whole list. 0 answers the empty list, and an
    // omitted or null third argument is the whole-history form above.
    let tail = conn.query("MATCH (n:Product) RETURN history(n, 'price', 2) AS h")?;
    let Value::List(tail) = &tail.rows[0][0] else {
        panic!("history() returns a LIST<MAP>");
    };
    assert_eq!(tail.as_slice(), &history[history.len() - 2..]);
    let none = conn.query("MATCH (n:Product) RETURN history(n, 'price', 0) AS h")?;
    assert_eq!(none.rows[0][0], Value::List(Vec::new().into()));

    // The checkpoint collapses nothing while retention is on; the reopen
    // restores the real chains beside the collapsed image rows.
    db.checkpoint()?;
    drop(conn);
    drop(db);
    let db = Database::open_with(
        dir.to_str().expect("a UTF-8 temp path"),
        relate::OpenOptions::default(),
    )?;
    let mut conn = db.connect()?;
    let reopened = conn.query("MATCH (n:Product) RETURN history(n, 'price') AS h")?;
    let Value::List(reopened) = &reopened.rows[0][0] else {
        panic!("history() returns a LIST<MAP>");
    };
    assert_eq!(reopened, history);
    Ok(())
}
