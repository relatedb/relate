//! The RANGE order-key unit tests — posting maintenance, the numeric
//! cross-type scans, the temporal key-tie groups, and the eq_only
//! identity buckets. A child module of `order` (the `constraint_keys`
//! precedent): the pins construct the private key and posting types
//! directly, so the tests live where those are visible and stay private
//! to their module.

use super::DrainState;
use super::*;
use crate::graph::test_support::out_of_window_local_datetime;

#[test]
fn prefix_without_successor_scans_to_string_variant_end() {
    let mut st = RangeIndex::default();
    insert_order(&mut st, &Value::String("\u{10FFFF}".into()), 10);
    insert_order(&mut st, &Value::String("\u{10FFFF}z".into()), 11);
    insert_order(&mut st, &Value::String("abc".into()), 12);
    let date = Value::Temporal(Temporal::Date(
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
    ));
    insert_order(&mut st, &date, 13);
    // Every char is at the max code point, so no successor prefix
    // exists: the scan runs to the end of the String variant.
    assert!(prefix_successor("\u{10FFFF}").is_none());
    let ids = prefix_ids(&st.order, "\u{10FFFF}");
    assert_eq!(ids, HashSet::from([10, 11]));
}

#[test]
fn numeric_cross_type_scan_stays_within_numeric_variants() {
    let mut st = RangeIndex::default();
    insert_order(&mut st, &Value::Int(1), 1);
    insert_order(&mut st, &Value::Int(3), 2);
    insert_order(&mut st, &Value::Float(0.5), 3);
    insert_order(&mut st, &Value::Float(2.5), 4);
    insert_order(&mut st, &Value::Float(3.5), 5);
    insert_order(&mut st, &Value::String("s".into()), 6);
    insert_order(&mut st, &Value::Int(-100), 7);

    // Float bound: the int side of the cross scan runs up to the Float
    // variant lower bound and never reaches the String variant.
    let gt = range_ids(&st.order, BinaryOp::Gt, &Value::Float(2.5));
    assert!(gt.is_superset(&HashSet::from([2, 5])));
    assert!(!gt.contains(&6));
    assert!(!gt.contains(&7));

    // Int bound, other direction: the float side runs from the Float
    // variant lower bound and never reaches the String variant.
    let lt = range_ids(&st.order, BinaryOp::Lt, &Value::Int(3));
    assert_eq!(lt, HashSet::from([1, 3, 4, 7]));
}

#[test]
fn strict_temporal_seek_includes_the_key_tie_group() {
    // `12:00+01:00` and `11:00Z` share the order key (the same instant)
    // but not the predicate's ordering: `Temporal::compare` breaks the
    // tie on the offset, so the wall clock 12:00 is strictly greater
    // than 11:00. A strict `>` seek must still collect it — the
    // boundary is inclusive and `cmp_holds` rejects what it should.
    let mut st = RangeIndex::default();
    let plus_one = Value::Temporal(Temporal::Time {
        time: chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        offset: 3600,
    });
    let zulu = Value::Temporal(Temporal::Time {
        time: chrono::NaiveTime::from_hms_opt(11, 0, 0).unwrap(),
        offset: 0,
    });
    insert_order(&mut st, &plus_one, 10);
    insert_order(&mut st, &zulu, 11);
    assert_eq!(
        order_key(&plus_one),
        order_key(&zulu),
        "one key, two orders"
    );
    // The key scan is a superset (it carries the tie group, bound
    // included); the production pipeline filters it through the
    // evaluator predicate, and that is what must answer exactly.
    let scanned = range_ids(&st.order, BinaryOp::Gt, &zulu);
    assert!(
        scanned.contains(&10),
        "the tie-broken greater value is scanned"
    );
    assert!(
        scanned.contains(&11),
        "the bound's own key group is scanned too"
    );
    let kept: HashSet<NodeId> = [(10, plus_one.clone()), (11, zulu.clone())]
        .into_iter()
        .filter(|(_, v)| cmp_holds(v, BinaryOp::Gt, &zulu))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(kept, HashSet::from([10]));
}

#[test]
fn unique_key_stays_one_posting_until_a_second_node() {
    let mut st = RangeIndex::default();
    insert_order(&mut st, &Value::Int(1), 10);
    assert!(matches!(
        st.order.get(&OrderKey::Int(1)),
        Some(Postings::One(10))
    ));
    assert!(matches!(st.by_node.get(&10), Some(RevKeys::One(_))));
    insert_order(&mut st, &Value::Int(1), 11);
    assert!(matches!(
        st.order.get(&OrderKey::Int(1)),
        Some(Postings::Many(_))
    ));
    assert!(st.order.get(&OrderKey::Int(1)).unwrap().contains(10));
    assert!(st.order.get(&OrderKey::Int(1)).unwrap().contains(11));
}

#[test]
fn second_value_on_one_node_promotes_the_reverse_key() {
    let mut st = RangeIndex::default();
    insert_order(&mut st, &Value::Int(1), 10);
    insert_order(&mut st, &Value::Int(2), 10);
    assert!(matches!(st.by_node.get(&10), Some(RevKeys::Many(keys)) if keys.len() == 2));
    order_remove_node(&mut st, 10);
    assert!(st.order.is_empty());
    assert!(st.by_node.is_empty());
}

#[test]
fn exact_integral_float_shares_the_int_bucket() {
    let mut st = RangeIndex::default();
    insert_order(&mut st, &Value::Int(1), 10);
    insert_order(&mut st, &Value::Float(1.0), 11);
    insert_order(&mut st, &Value::Float(-0.0), 12);
    assert_eq!(
        equality_ids(&st, &Value::Float(1.0)),
        HashSet::from([10, 11])
    );
    assert_eq!(equality_ids(&st, &Value::Int(0)), HashSet::from([12]));
    insert_order(&mut st, &Value::Float(1.5), 13);
    assert!(matches!(
        order_key(&Value::Float(1.5)),
        Some(OrderKey::Float(_))
    ));
    assert_eq!(equality_ids(&st, &Value::Float(1.5)), HashSet::from([13]));
}

#[test]
fn unordered_value_is_found_by_equality() {
    let list = Value::List(vec![Value::Int(1)].into());
    let mut st = RangeIndex::default();
    insert_order(&mut st, &list, 20);
    assert!(st.order.is_empty());
    assert_eq!(equality_ids(&st, &list), HashSet::from([20]));
    order_remove_node(&mut st, 20);
    assert!(equality_ids(&st, &list).is_empty());
}

#[test]
fn range_count_sums_posting_lengths() {
    let mut st = RangeIndex::default();
    insert_order(&mut st, &Value::Int(50), 1);
    for id in [2, 3, 4] {
        insert_order(&mut st, &Value::Int(150), id);
    }
    for id in [5, 6] {
        insert_order(&mut st, &Value::Int(250), id);
    }
    // Node 2 holds two in-range keys, so the count sums its posting twice
    // while the id set holds it once — the distinction the docs claim.
    insert_order(&mut st, &Value::Int(250), 2);
    let bound = Value::Int(100);
    let count = range_count(&st.order, BinaryOp::Gt, &bound);
    // The reference walk takes the seek's own boundary, so a change to the
    // rule cannot leave the two spellings disagreeing.
    let seek = scan::seek_ranges(BinaryOp::Gt, &bound).expect("a comparison");
    let mut summed = 0usize;
    for (start, end) in seek.ranges() {
        scan::visit_range(&st.order, start, end, |postings| summed += postings.len());
    }
    assert_eq!(count, 6, "node 2's two in-range postings both count");
    assert_eq!(count, summed);
    assert_eq!(range_ids(&st.order, BinaryOp::Gt, &bound).len(), 5);
    assert_eq!(equality_count(&st, &Value::Int(150)), 3);
}

#[test]
fn out_of_window_datetime_is_not_an_order_key() {
    let value = out_of_window_local_datetime();
    let mut st = RangeIndex::default();
    insert_order(&mut st, &value, 7);
    assert!(st.order.is_empty());
    assert!(st.unbounded.contains(&7));
    order_remove_node(&mut st, 7);
    assert!(st.unbounded.is_empty());
}

/// The range index posts a zoned datetime under its *order* key (instant)
/// and under its *identity* key (instant + zone); the equality probe
/// reads the latter, so the DST fall-back overlap's other half is a miss
/// — Neo4j's index behaviour (LABS-785).
#[test]
fn zoned_datetime_equality_bucket_keeps_the_instant() {
    let local = chrono::NaiveDate::from_ymd_opt(2025, 10, 26)
        .unwrap()
        .and_hms_opt(2, 30, 0)
        .unwrap();
    // hours is the offset east of UTC; the UTC instant and the stored
    // offset both derive from it, so no incoherent half is expressible.
    let half = |hours: i64| {
        Value::Temporal(Temporal::DateTime {
            utc: local - chrono::Duration::hours(hours),
            offset: (hours * 3600) as i32,
            zone: Some("Europe/Stockholm".into()),
        })
    };
    let first = half(2);
    let second = half(1);
    let mut st = RangeIndex::default();
    insert_order(&mut st, &first, 1);
    assert_eq!(equality_ids(&st, &first), HashSet::from([1]));
    // The `=`-equal other half is a different instant, hence a miss.
    assert!(equality_ids(&st, &second).is_empty());
    // A different zone identity never joins the bucket.
    let other_zone = Value::Temporal(Temporal::DateTime {
        utc: local - chrono::Duration::hours(1),
        offset: 3600,
        zone: Some("Europe/Berlin".into()),
    });
    assert!(equality_ids(&st, &other_zone).is_empty());
}

/// The aggregate trust mark follows the LIVE value: the per-version
/// inserts post the superset and never decide it, so a superseded NaN
/// cannot leave its mark and the live value alone decides.
#[test]
fn a_superseded_nan_does_not_mark_the_pair() {
    let nan = Value::Float(f64::NAN);
    let mut st = RangeIndex::default();
    // Two versions of node 7: the stale NaN, the live 5. The per-version
    // inserts decided no trust mark; the live restatement keeps the node
    // clean and the pair complete.
    let live = Value::Int(5);
    insert_order(&mut st, &nan, 7);
    insert_order(&mut st, &live, 7);
    assert!(!st.unposted.contains(&7));
    st.reconcile_live(7, Some(&live));
    assert!(!st.unposted.contains(&7));
    assert!(st.order_is_complete(), "the pair can serve aggregates");
}

/// The other direction: the LIVE value is the NaN — the mark is set and
/// the aggregates decline.
#[test]
fn a_live_nan_marks_the_pair() {
    let nan = Value::Float(f64::NAN);
    let mut st = RangeIndex::default();
    insert_order(&mut st, &nan, 8);
    st.reconcile_live(8, Some(&nan));
    assert!(st.unposted.contains(&8));
    assert!(!st.order_is_complete(), "the NaN still declines aggregates");
}

/// `unbounded` is the comparison seeks' any-snapshot gate, NOT an
/// aggregate mark: a superseded out-of-window temporal keeps it even
/// behind a live ordering value (a seek whose statement snapshot still
/// sees that version must scan), so the reconcile never touches it — and
/// the aggregate decline the stale mark costs is the safe direction. A
/// node whose live version carries no value for the pair is marked by
/// nothing: the reconcile's clear is its own work.
#[test]
fn the_seek_gate_keeps_its_superset_while_the_mark_clears() {
    let nan = Value::Float(f64::NAN);
    let out_of_window = out_of_window_local_datetime();
    let mut st = RangeIndex::default();
    let live = Value::String("live".into());
    insert_order(&mut st, &out_of_window, 8);
    insert_order(&mut st, &live, 8);
    st.reconcile_live(8, Some(&live));
    assert!(
        st.unbounded.contains(&8),
        "the seek gate keeps the superset"
    );
    assert!(
        !st.order_is_complete(),
        "the stale mark safely declines aggregates"
    );

    // The live value itself out of the order window: `unbounded` holds
    // from the per-version post (the reconcile never touches it), and
    // the aggregate mark clears — the temporal reaches `eq_only`.
    st.reconcile_live(8, Some(&out_of_window));
    assert!(st.unbounded.contains(&8));
    assert!(!st.unposted.contains(&8));

    // No live value for the pair: the node contributes nothing at the
    // latest state, so the aggregate mark clears — the seek gate keeps
    // its superset.
    insert_order(&mut st, &nan, 8);
    st.reconcile_live(8, None);
    assert!(!st.unposted.contains(&8));
    assert!(st.unbounded.contains(&8));

    // The None path against a marked node: the per-version inserts set
    // nothing, so the clear is the reconcile's own work.
    let mut st = RangeIndex::default();
    insert_order(&mut st, &nan, 9);
    st.reconcile_live(9, Some(&nan));
    assert!(st.unposted.contains(&9));
    st.reconcile_live(9, None);
    assert!(!st.unposted.contains(&9), "the clear is the reconcile's");
    assert!(!st.unbounded.contains(&9));
}

/// REL8-2030 (corpus 26): DATE's two representations share one order, but
/// the key map orders by variant, which cannot express it — a negative
/// civil year sorts below every in-range date while its variant sits after
/// them. A civil date therefore has no key: it marks its node unbounded, so
/// a comparison seek declines instead of answering the Date variant alone
/// and silently dropping the far-year row.
#[test]
fn a_far_year_date_has_no_order_key_and_declines_the_seek() {
    let mut st = RangeIndex::default();
    let in_range = Value::Temporal(Temporal::Date(
        chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
    ));
    let far = Value::Temporal(Temporal::CivilDate {
        year: 300_000,
        month: 6,
        day: 15,
    });
    insert_order(&mut st, &in_range, 10);
    insert_order(&mut st, &far, 11);
    assert!(order_key(&in_range).is_some());
    assert!(order_key(&far).is_none());
    // `unbounded` is the comparison seek's gate: it must carry the node.
    assert!(st.unbounded.contains(&11));
    // The key scan alone would miss the row (the bound has no key either);
    // the unbounded mark is what makes the production seek scan instead.
    assert!(range_ids(&st.order, BinaryOp::Lt, &far).is_empty());
}

/// An infinite float bound's cross sweep stays total: `< Infinity` and
/// `> -Infinity` admit every integer, so the seek must scan the integer
/// variant too (the predicate re-checks the superset). The old numeric gate
/// dropped a non-finite bound's cross sweep entirely, silently losing every
/// integer row.
#[test]
fn an_infinite_float_bound_scans_the_integer_side() {
    let mut st = RangeIndex::default();
    insert_order(&mut st, &Value::Int(1), 1);
    insert_order(&mut st, &Value::Int(2), 2);
    insert_order(&mut st, &Value::Float(1.5), 3);
    let less = range_ids(&st.order, BinaryOp::Lt, &Value::Float(f64::INFINITY));
    assert!(
        less.is_superset(&HashSet::from([1, 2, 3])),
        "the int rows are swept too: {less:?}"
    );
    let greater = range_ids(&st.order, BinaryOp::Gt, &Value::Float(f64::NEG_INFINITY));
    assert!(
        greater.is_superset(&HashSet::from([1, 2, 3])),
        "the int rows are swept too: {greater:?}"
    );
}
