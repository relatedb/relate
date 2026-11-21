//! The range-index integration tests: numeric ordering plus the eq-only
//! lookup, the aggregate shortcut's trust-mark protocol, and the
//! out-of-window temporal that falls back to the scan — the concern
//! module the 1k-line boundary moved out of `mod.rs` (its
//! `point`/`chain` siblings' precedent).

use super::*;
use crate::graph::test_support::out_of_window_local_datetime;

#[test]
fn range_index_orders_numbers() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let node = |v: i64| {
        g.create_node(
            vec!["N".into()],
            BTreeMap::from([("x".into(), Value::Int(v))]),
        )
        .unwrap()
    };
    let low = node(1);
    let high = node(5);
    g.commit_write();
    let labels = vec!["N".into()];
    let seek = RangeSeek::Cmp {
        op: relate_ir::BinaryOp::Gt,
        bound: Value::Int(1),
    };
    let seek_ids = || g.lookup_range_seek(&labels, "x", &seek, g.commit_ts());
    assert!(seek_ids().is_none());
    g.materialize_range_index("N", "x");
    assert_eq!(seek_ids().unwrap(), vec![high]);
    let ids = g
        .lookup_nodes(&labels, &BTreeMap::from([("x".into(), Value::Int(1))]))
        .unwrap();
    assert_eq!(ids, vec![low]);
}

#[test]
fn the_pair_answers_again_once_an_unpostable_value_is_superseded() {
    // rust-review (PR #611): the range index's `unposted` trust mark must
    // follow the LIVE value — a NaN property that a later write replaces
    // must stop declining the pair, or the aggregate shortcut would fall
    // back to the scan forever. The aggregate reads are the mark's one
    // consumer (a comparison seek answers from `unbounded`, a different
    // gate); the drain runs through the next read.
    let g = Graph::default();
    g.begin_write().unwrap();
    let node = g
        .create_node(
            vec!["N".into()],
            BTreeMap::from([("x".into(), Value::Float(f64::NAN))]),
        )
        .unwrap();
    g.commit_write();
    g.materialize_range_index("N", "x");
    assert!(
        g.range_extremum_at("N", "x", Extremum::Min, g.commit_ts())
            .is_none(),
        "the NaN value declines the aggregate"
    );
    // Supersede it: the next read drains the dirty mark and the live value
    // alone decides the trust mark — the aggregate answers again.
    g.begin_write().unwrap();
    g.set_property_node(node, "x", Value::Int(5));
    g.commit_write();
    assert_eq!(
        g.range_extremum_at("N", "x", Extremum::Min, g.commit_ts()),
        Some(Value::Int(5)),
        "the live value restores the pair"
    );
}

#[test]
fn range_seek_scans_when_a_datetime_does_not_fit_the_order_key() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(
        vec!["N".into()],
        BTreeMap::from([("t".into(), out_of_window_local_datetime())]),
    )
    .unwrap();
    g.commit_write();
    g.materialize_range_index("N", "t");
    assert!(g
        .lookup_range_seek(
            &["N".into()],
            "t",
            &RangeSeek::Cmp {
                op: relate_ir::BinaryOp::Gt,
                bound: Value::Int(0),
            },
            g.commit_ts(),
        )
        .is_none());
}
