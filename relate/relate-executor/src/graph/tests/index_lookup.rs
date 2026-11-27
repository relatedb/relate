//! The equality seek's route contract: the store arm's regression home
//! (a Scan route declines to the caller's one scan, never a scan per
//! list member) and its siblings as they land.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{PropertyMap, Value};

use super::super::redo::RedoOp;
use super::super::Graph;

#[test]
fn a_scan_route_in_list_seek_declines_to_the_callers_one_scan() {
    // The store arm's regression, pinned: a label-less `IN`-list seek
    // over a key no index covers (the Scan route) must decline (`None`)
    // so the caller scans ONCE — the per-value fallback loop would turn
    // it into one full scan per list member, and the pre-store pre-check
    // declined exactly here. No store is attached (the default build's
    // every path); the store arm's consult returns `None` first.
    let g = Graph::default();
    g.begin_write().unwrap();
    let ops: Vec<_> = (0..3)
        .map(|i| RedoOp::CreateNode {
            id: i,
            labels: Arc::new(vec!["P".into()]),
            properties: PropertyMap::share(BTreeMap::from([("k".to_string(), Value::Int(i))])),
        })
        .collect();
    g.apply_redo(ops, 1).unwrap();
    let ts = g.commit_ts();
    let values = vec![Value::Int(0), Value::Int(1), Value::Int(2)];
    assert!(g.lookup_equality_upto(&[], "k", &values, ts, 10).is_none());
}
