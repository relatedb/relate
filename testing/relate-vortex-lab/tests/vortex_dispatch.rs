//! The Vortex store's executor dispatch (ADR-0665, measured ordering): a
//! declared index always outranks the attached store; the label scan stays
//! on the canonical label route (measured at 1.5-2.2x on the pure
//! label-scan pattern queries, so the seam has no label arm at all); and
//! the sorted side-file preempts a COLD lazy pair only — a warm pair's
//! probe beats the seek.

use std::sync::Arc;

use relate::Database;
use relate_core::Value;
use relate_executor::Graph;
use relate_vortex_lab::engine::VortexStore;

/// Ten `N` nodes, `x` 1..=10: an equality or a two-value `IN` selects a
/// tenth of the label, so the seek funnel serves it rather than scanning.
fn seeded() -> Database {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    for x in 1..=10i64 {
        conn.query(&format!("CREATE (:N {{x: {x}}})")).unwrap();
    }
    drop(conn);
    db
}

fn attach(db: &Database) -> Arc<VortexStore> {
    let graph: &Graph = db.sol_graph();
    let store = Arc::new(VortexStore::from_graph(graph, graph.commit_ts()).unwrap());
    assert!(
        graph.attach_prop_store(store.clone()),
        "a store is already attached"
    );
    store
}

#[test]
fn the_store_serves_cold_equality_arms_and_never_the_label_scan() {
    let db = seeded();
    let store = attach(&db);
    let mut conn = db.connect().unwrap();
    let before_scan = store.served();

    // The label scan's hot shape stays on the canonical label scan: the
    // CI interleave measured a store bitset arm at 1.5-2.2x on the pure
    // label-scan pattern queries (q8/q30), so the seam carries no label
    // arm — the query answers correctly and no store arm moves.
    let scan = conn.query("MATCH (n:N) RETURN n.x AS x").unwrap();
    assert_eq!(scan.rows.len(), 10);
    assert_eq!(
        store.served(),
        before_scan,
        "no store arm serves the canonical label scan"
    );
    let (seek0, member0, _, _) = store.served();

    // A label-less equality is the scan route: the store's sorted
    // side-file seek answers it ahead of the full scan.
    let equality = conn
        .query("MATCH (n) WHERE n.x = 3 RETURN n.x AS x")
        .unwrap();
    assert_eq!(equality.rows, vec![vec![Value::Int(3)]]);
    let (seek, _, _, _) = store.served();
    assert!(seek > seek0, "the side-file seek served the equality");

    // The label-less `IN` member is the same arm.
    let member = conn
        .query("MATCH (n) WHERE n.x IN [2, 3] RETURN n.x AS x")
        .unwrap();
    assert_eq!(member.rows.len(), 2);
    let (_, members, _, _) = store.served();
    assert!(members > member0, "the side-file member served the IN-list");

    // A declared RANGE index outranks the store: once it covers the
    // seek, every store arm stands down for that lookup. The labeled
    // form is the one the declared pair covers (the index is on (N, x);
    // a label-less seek keeps the scan route by ADR-0614's own rule).
    conn.query("CREATE INDEX FOR (n:N) ON (n.x)").unwrap();
    conn.query("MATCH (n:N) WHERE n.x = 7 RETURN n.x AS x")
        .unwrap();
    let (seek2, _, _, _) = store.served();
    conn.query("MATCH (n:N) WHERE n.x = 8 RETURN n.x AS x")
        .unwrap();
    let (seek3, _, _, _) = store.served();
    assert_eq!(
        seek3, seek2,
        "the declared range pair outranks the side-file"
    );
}

#[test]
fn the_store_serves_the_aggregate_ladder_behind_the_postings() {
    let db = seeded();
    let store = attach(&db);
    let mut conn = db.connect().unwrap();
    let (_, _, folds0, grouped0) = store.served();

    // The folds shape over an undeclared key: one columnar pass on the
    // folds rung, not the grouped one.
    let folds = conn
        .query("MATCH (n:N) RETURN count(n.x) AS c, min(n.x) AS lo, max(n.x) AS hi")
        .unwrap();
    assert_eq!(
        folds.rows,
        vec![vec![Value::Int(10), Value::Int(1), Value::Int(10)]]
    );
    let (_, _, folds_served, grouped_after_folds) = store.served();
    assert!(
        folds_served > folds0,
        "the columnar folds served the aggregate shape"
    );
    assert_eq!(
        grouped_after_folds, grouped0,
        "the folds shape is not the grouped rung"
    );

    // The grouped shape: one (value, count) row per distinct value, on the
    // grouped rung, not the folds one.
    let grouped = conn
        .query("MATCH (n:N) RETURN n.x, count(*) ORDER BY n.x")
        .unwrap();
    let expected: Vec<Vec<Value>> = (1..=10i64)
        .map(|x| vec![Value::Int(x), Value::Int(1)])
        .collect();
    assert_eq!(grouped.rows, expected);
    let (_, _, folds_after_grouped, grouped_served) = store.served();
    assert!(
        grouped_served > grouped_after_folds,
        "the columnar distinct served the grouped shape"
    );
    assert_eq!(
        folds_after_grouped, folds_served,
        "the grouped shape is not the folds rung"
    );
}

#[test]
fn writes_after_the_attach_stay_answer_correct() {
    // Step 1 of the versioning design: commits after the attach feed the
    // store's delta overlay, so the attached store keeps answering (point
    // reads resolve the overlay; the set arms decline to the engine while
    // dirty). The same script runs against an unattached twin — every
    // answer must match.
    let script = [
        "MATCH (n:N {x: 3}) SET n.x = 99",
        "MATCH (n) WHERE n.x = 99 RETURN n.x AS x",
        "MATCH (n) WHERE n.x = 3 RETURN n.x AS x",
        "CREATE (:N {x: 42})",
        "MATCH (n) WHERE n.x = 42 RETURN n.x AS x",
        "MATCH (n:N {x: 5}) DELETE n",
        "MATCH (n) WHERE n.x = 5 RETURN n.x AS x",
        "MATCH (n) WHERE n.x > 90 RETURN n.x AS x",
        "MATCH (n:N) RETURN count(n.x) AS c",
    ];
    let run = |db: &Database| -> Vec<Vec<Value>> {
        let mut conn = db.connect().unwrap();
        script
            .iter()
            .flat_map(|q| conn.query(q).unwrap().rows)
            .collect()
    };
    let attached_db = seeded();
    let store = attach(&attached_db);
    let mut conn = attached_db.connect().unwrap();
    // Statement by statement, so the store has a counter sample from after
    // the first commit and before the rest of the script's reads.
    let mut attached = conn.query(script[0]).unwrap().rows;
    let counters = store.served();
    for q in &script[1..] {
        attached.extend(conn.query(q).unwrap().rows);
    }
    drop(conn);
    let plain = run(&seeded());
    assert_eq!(
        attached, plain,
        "the attached store answers as the row store"
    );
    // The conservative rule this test pins: once a commit has landed, the
    // set arms decline to the engine (their base tier alone is stale), so
    // no post-write query advances a store counter.
    assert_eq!(
        store.served(),
        counters,
        "the set arms decline while the delta is dirty"
    );
}
