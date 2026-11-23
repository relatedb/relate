//! PROFILE's expression-memo counters (ADR-0654): a memoised expression
//! reports its probes, hits, inserts, refused inserts, table bytes and
//! the count of memos that gave up, on the operator that ran it — or, for
//! the temporal constructors' statement-level memos, on the plan root with
//! the memo types named. A statement that used no memo carries no group.
//!
//! Run with `cargo run -p relate --example profile_memo`.

use relate::{Database, Result, Value};

/// A string-fed `datetime(...)` over two distinct texts: the constructor
/// memo answers every repeat after its warm-up.
const REPEATED: &str = "UNWIND range(0, 1999) AS i \
     WITH ['2024-05-05T10:00:00Z', '2023-01-01T09:00:00Z'][i % 2] AS s \
     RETURN count(datetime(s)) AS n";

fn main() -> Result<()> {
    let db = Database::open(":memory:")?;
    let mut conn = db.connect()?;

    // The root carries the statement's constructor-memo sums.
    let profiled = conn.query(&format!("PROFILE {REPEATED}"))?;
    assert_eq!(profiled.rows[0][0], Value::Int(2000));
    let root = profiled.plan_root().expect("PROFILE stamps the plan");
    let memo = root.metrics.expect("a PROFILE root has metrics").memo;
    assert!(memo.probes > 0 && memo.hits > 0 && memo.hits <= memo.probes);
    assert_eq!(memo.inserts, 2, "two distinct texts were stored");
    assert!(
        memo.bytes > 0,
        "the table's allocation, never `memory_bytes`"
    );
    println!("hits/probes = {}/{}", memo.hits, memo.probes);

    // On the wire the group is presence-only: the `Memo*` integers and the
    // root's `MemoKinds` names exist because a memo was probed.
    let Value::Map(wire) = root.to_value() else {
        panic!("the machine-native plan value is a map");
    };
    let Some(Value::Map(args)) = wire.get("args") else {
        panic!("a node's args is a map");
    };
    assert_eq!(args.get("MemoHits"), Some(&Value::Int(memo.hits as i64)));
    assert!(args.contains_key("MemoKinds"));

    // A statement that used no memo carries no group at all.
    let plain = conn.query("PROFILE RETURN 1 AS one")?;
    let root = plain.plan_root().expect("PROFILE stamps the plan");
    assert_eq!(root.metrics.expect("metrics").memo.probes, 0);
    let Value::Map(wire) = root.to_value() else {
        panic!("the machine-native plan value is a map");
    };
    let Some(Value::Map(args)) = wire.get("args") else {
        panic!("a node's args is a map");
    };
    assert!(!args.contains_key("MemoProbes"));
    Ok(())
}
