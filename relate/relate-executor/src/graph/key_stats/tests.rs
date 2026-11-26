use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;

use chrono::NaiveDate;
use relate_core::{Temporal, Value};
use relate_planner::Provenance;

use super::accum::Num;
use super::build::LabelScan;
use super::gate::{admission, Admission, KeyClass, MAX_TEXT_BYTES};
use super::ledger::Ledger;
use super::sketch::{identity_hash, Hll, TopK, TOP_K};
use super::*;

fn props(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// A committed graph of `n` `:P` nodes whose properties come from `make`.
fn graph(n: i64, make: impl Fn(i64) -> BTreeMap<String, Value>) -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    for i in 0..n {
        g.create_node(vec!["P".into()], make(i)).unwrap();
    }
    g.commit_write();
    g
}

/// Serve every requested label, the way the maintenance entry does with no
/// bound and no cancellation.
fn pump(g: &Graph) -> usize {
    g.build_pending_key_stats(usize::MAX, || true)
}

fn consult_and_build(g: &Graph, label: &str, key: &str) -> Option<Arc<KeyStats>> {
    assert!(g.key_stats_named(label, key).is_none(), "unbuilt is None");
    assert_eq!(pump(g), 1);
    g.key_stats_named(label, key)
}

#[test]
fn an_abandoned_scan_is_not_counted_as_built_and_stays_requested() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    assert!(g.key_stats_named("P", "k").is_none());
    // The pump's own check passes, the chunk boundary's does not.
    let asked = std::cell::Cell::new(0);
    let built = g.build_pending_key_stats(1, || {
        asked.set(asked.get() + 1);
        asked.get() == 1
    });
    assert_eq!(built, 0, "claimed and returned to the queue, not built");
    assert!(g.has_pending_key_stats(), "still requested");
    assert!(g.key_stats_named("P", "k").is_none());
    assert_eq!(pump(&g), 1, "a later statement builds it");
    assert!(g.key_stats_named("P", "k").is_some());
}

/// Create `n` more `:P` nodes in one committed write.
fn add_p_nodes(g: &Graph, n: i64) {
    g.begin_write().unwrap();
    for i in 0..n {
        g.create_node(vec!["P".into()], props(&[("k", Value::Int(10_000 + i))]))
            .unwrap();
    }
    g.commit_write();
}

/// The statistics epoch moves exactly when what a consult answers changes:
/// a label's statistics publish, age out once the writes pass the stale
/// threshold (once per transition, not per write), or are cleared; a bare
/// request, writes below the threshold and writes to an already stale
/// label move nothing. It never goes backwards.
#[test]
fn the_epoch_moves_when_statistics_publish_age_out_or_clear_and_only_then() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    let seen = std::cell::Cell::new(g.key_stats_epoch());
    // The epoch moved by `by` since the last check.
    let moved = |g: &Graph, by: u64, why: &str| {
        let now = g.key_stats_epoch();
        assert_eq!(now, seen.get() + by, "{why}");
        seen.set(now);
    };

    assert!(g.key_stats_named("P", "k").is_none());
    moved(&g, 0, "a request changes no answer");

    assert_eq!(pump(&g), 1);
    moved(&g, 1, "the label's statistics published");
    assert!(g.key_stats_named("P", "k").is_some());

    add_p_nodes(&g, 3);
    moved(&g, 0, "writes below the stale threshold");

    // 100 writes in several commits: one transition to stale, one bump.
    for _ in 0..10 {
        add_p_nodes(&g, 10);
    }
    moved(&g, 1, "the label turned stale, once, not once per write");
    assert!(g.key_stats_named("P", "k").is_none(), "stale answers None");
    add_p_nodes(&g, 10);
    moved(&g, 0, "writes to an already stale label");

    assert_eq!(pump(&g), 1);
    moved(&g, 1, "the rebuild published again");

    g.indexes.key_stats.clear();
    moved(&g, 1, "clearing dropped the published statistics");
    g.indexes.key_stats.clear();
    moved(&g, 0, "clearing nothing changes nothing");
}

/// A publication that leaves every answer as it was (every key of the
/// label declined, so nothing is published) does not move the epoch: a
/// plan that read the unmeasured label stays valid.
#[test]
fn publishing_nothing_leaves_the_epoch_alone() {
    let long = Value::String("x".repeat(MAX_TEXT_BYTES + 1).into());
    let g = graph(50, |_| props(&[("doc", long.clone())]));
    let before = g.key_stats_epoch();
    assert!(g.key_stats_named("P", "doc").is_none());
    assert_eq!(pump(&g), 1, "the label was scanned and published");
    assert!(
        g.key_stats_named("P", "doc").is_none(),
        "the key is declined"
    );
    assert_eq!(
        g.key_stats_epoch(),
        before,
        "nothing a consult sees changed"
    );
}

/// The pair build reads the key's published distinct count WITHOUT asking
/// for a build: a pair is built once, so a request made for it would scan the
/// label to benefit a build that has already happened.
#[test]
fn the_pair_builds_distinct_hint_never_requests_a_build() {
    let g = graph(2_000, |i| props(&[("k", Value::Int(i))]));
    let (label, key) = (
        g.interner.get_id("P").unwrap(),
        g.interner.get_id("k").unwrap(),
    );

    assert_eq!(g.published_distinct(label, key), None, "unbuilt");
    assert!(!g.has_pending_key_stats(), "a peek never requests");

    assert!(g.key_stats_named("P", "k").is_none(), "a consult requests");
    assert!(g.has_pending_key_stats());
    assert_eq!(
        g.published_distinct(label, key),
        None,
        "requested, not built"
    );
    assert_eq!(pump(&g), 1);

    let distinct = g.published_distinct(label, key).expect("published");
    assert!(
        (1_900..=2_100).contains(&distinct),
        "2,000 unique values: {distinct}"
    );
    assert!(!g.has_pending_key_stats(), "the peek asked for nothing");
}

#[test]
fn the_distinct_hint_is_withdrawn_when_the_label_goes_stale_and_for_the_star() {
    let g = graph(500, |i| props(&[("k", Value::Int(i))]));
    let (label, key) = (
        g.interner.get_id("P").unwrap(),
        g.interner.get_id("k").unwrap(),
    );
    consult_and_build(&g, "P", "k").expect("built");
    assert!(g.published_distinct(label, key).is_some());

    // Writes past the stale threshold age the statistics out.
    g.begin_write().unwrap();
    for i in 0..200 {
        g.create_node(vec!["P".into()], props(&[("k", Value::Int(10_000 + i))]))
            .unwrap();
    }
    g.commit_write();
    assert_eq!(
        g.published_distinct(label, key),
        None,
        "stale: the sample decides"
    );

    let star = g.interner.get_id("*").unwrap();
    assert_eq!(
        g.published_distinct(star, key),
        None,
        "the any-label bucket"
    );
}

#[test]
fn the_pending_flag_tracks_requests_for_the_seams_fast_path() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    assert!(!g.has_pending_key_stats(), "nothing asked yet");
    assert!(g.key_stats_named("P", "k").is_none());
    assert!(g.has_pending_key_stats(), "a consult recorded a request");
    assert_eq!(pump(&g), 1);
    assert!(!g.has_pending_key_stats(), "the pump served it");
}

#[test]
fn the_gate_admits_structured_values_and_short_strings_only() {
    let s = |n: usize| Value::String("x".repeat(n).into());
    let date = Value::Temporal(Temporal::Date(NaiveDate::from_ymd_opt(2020, 1, 2).unwrap()));
    let table = [
        (Value::Bool(true), Admission::Structured),
        (Value::Int(3), Admission::Structured),
        (Value::Float(1.5), Admission::Structured),
        (date, Admission::Structured),
        (s(MAX_TEXT_BYTES), Admission::Text),
        (s(MAX_TEXT_BYTES + 1), Admission::Declined),
        (Value::Float(f64::NAN), Admission::Declined),
        (Value::Null, Admission::Declined),
        (
            Value::List(Arc::new(vec![Value::Int(1)])),
            Admission::Declined,
        ),
        (Value::Map(Arc::new(BTreeMap::new())), Admission::Declined),
    ];
    for (value, expected) in table {
        assert_eq!(admission(&value), expected, "{value:?}");
    }
}

#[test]
fn hll_counts_distinct_values_within_its_error() {
    for n in [10u64, 1_000, 100_000] {
        let mut hll = Hll::new();
        for i in 0..n {
            hll.insert(identity_hash(&Value::Int(i as i64)).unwrap());
        }
        let est = hll.estimate() as f64;
        let err = (est - n as f64).abs() / n as f64;
        // 4.5 standard errors of a 1024-register sketch: deterministic in
        // practice although the hash seed is per process.
        assert!(err < 0.15, "n={n} estimate={est} err={err}");
    }
}

#[test]
fn hll_counts_a_repeated_value_once() {
    let mut hll = Hll::new();
    for _ in 0..10_000 {
        hll.insert(identity_hash(&Value::Int(7)).unwrap());
    }
    assert_eq!(hll.estimate(), 1);
}

#[test]
fn an_integer_and_its_float_are_one_value() {
    assert_eq!(
        identity_hash(&Value::Int(3)),
        identity_hash(&Value::Float(3.0))
    );
}

#[test]
fn top_k_keeps_a_value_above_a_tenth_of_the_rows() {
    let mut top = TopK::default();
    let total = 10_000u64;
    for i in 0..total {
        // Value 0 is 10% of the rows, interleaved with a long tail.
        let v = if i % 10 == 0 { 0 } else { 1 + (i % 997) as i64 };
        let value = Value::Int(v);
        top.observe(identity_hash(&value).unwrap(), &value);
    }
    let heavy = top.heavy(total);
    assert!(heavy.len() <= TOP_K);
    let zero = identity_hash(&Value::Int(0)).unwrap();
    let hit = heavy
        .iter()
        .find(|t| identity_hash(&t.value) == Some(zero))
        .expect("value 0 kept");
    assert!(hit.count >= 1_000, "an upper bound, never below the truth");
    assert_eq!(
        identity_hash(&heavy[0].value),
        Some(zero),
        "the most frequent comes first"
    );
}

/// The summary's floor is not a frequency: with no truly heavy value every
/// tracked slot is a recycled one whose upper-bound count sits near
/// `total / TOP_K`, and none may be published as a heavy value (the
/// validation against a 100k-row store found `age` and `city` reporting
/// 6,251 and 445 for values that occur a few times).
#[test]
fn a_near_uniform_key_publishes_no_heavy_values() {
    for distinct in [32i64, 997, 5_000] {
        let mut top = TopK::default();
        let total = 100_000u64;
        // A fixed xorshift stream: slot counts then vary around the floor
        // (a cyclic stream lands every slot on exactly `total / TOP_K`,
        // which a strict `>` happens to exclude).
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..total {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let value = Value::Int((x % distinct as u64) as i64);
            top.observe(identity_hash(&value).unwrap(), &value);
        }
        assert!(
            top.heavy(total).is_empty(),
            "{distinct} near-uniform values have no heavy hitter"
        );
    }
}

/// Real heavy hitters survive the lower-bound filter, whether they arrive
/// first or after the summary has already recycled its slots.
#[test]
fn heavy_hitters_survive_the_lower_bound_filter_whenever_they_arrive() {
    for late in [false, true] {
        let mut top = TopK::default();
        let total = 20_000u64;
        // Two heavy values (30% and 15% of the rows) and a long tail; for
        // the late case 5,000 tail rows come first, so the summary is full
        // of recycled slots when the heavy values start.
        let lead = if late { 5_000 } else { 0 };
        for i in 0..total {
            let v = if i < lead {
                10_000 + (i % 4_999) as i64
            } else {
                match i % 20 {
                    0..=5 => 1,
                    6..=8 => 2,
                    _ => 20_000 + (i % 3_001) as i64,
                }
            };
            let value = Value::Int(v);
            top.observe(identity_hash(&value).unwrap(), &value);
        }
        let heavy = top.heavy(total);
        let seen = |v: i64| {
            let h = identity_hash(&Value::Int(v));
            heavy.iter().any(|t| identity_hash(&t.value) == h)
        };
        assert!(seen(1) && seen(2), "late={late}: both heavy values found");
        assert_eq!(heavy.len(), 2, "late={late}: and nothing else: {heavy:?}");
    }
}

#[test]
fn count_min_max_sum_and_mean_of_integers() {
    let g = graph(100, |i| props(&[("k", Value::Int(i + 1))]));
    let st = consult_and_build(&g, "P", "k").expect("built");
    assert_eq!(st.count, 100);
    assert_eq!(st.declined, 0);
    assert_eq!(st.min, Some(Value::Int(1)));
    assert_eq!(st.max, Some(Value::Int(100)));
    assert_eq!(st.int_sum, 5050);
    assert_eq!(st.mean(), Some(50.5));
    assert_eq!(st.provenance, Provenance::Sketched);
}

#[test]
fn the_integer_sum_is_wide_and_a_nan_is_declined() {
    let g = Graph::default();
    g.begin_write().unwrap();
    for v in [i64::MAX, i64::MAX] {
        g.create_node(vec!["P".into()], props(&[("k", Value::Int(v))]))
            .unwrap();
    }
    g.create_node(vec!["P".into()], props(&[("k", Value::Float(f64::NAN))]))
        .unwrap();
    g.commit_write();
    let st = consult_and_build(&g, "P", "k").expect("built");
    assert_eq!(st.count, 2, "the NaN was not read");
    assert_eq!(st.declined, 1);
    assert_eq!(st.int_sum, 2 * i128::from(i64::MAX));
}

#[test]
fn mixed_numbers_compare_by_value() {
    let g = Graph::default();
    g.begin_write().unwrap();
    for v in [
        Value::Int(5),
        Value::Float(1.5),
        Value::Int(-2),
        Value::Float(9.25),
    ] {
        g.create_node(vec!["P".into()], props(&[("k", v)])).unwrap();
    }
    g.commit_write();
    let st = consult_and_build(&g, "P", "k").expect("built");
    assert_eq!(st.min, Some(Value::Int(-2)));
    assert_eq!(st.max, Some(Value::Float(9.25)));
    assert_eq!(st.numeric_count, 4);
}

#[test]
fn temporals_report_a_minimum_and_a_maximum() {
    let date =
        |d: u32| Value::Temporal(Temporal::Date(NaiveDate::from_ymd_opt(2020, 1, d).unwrap()));
    let g = Graph::default();
    g.begin_write().unwrap();
    for d in [9, 3, 27, 14] {
        g.create_node(vec!["P".into()], props(&[("d", date(d))]))
            .unwrap();
    }
    g.commit_write();
    let st = consult_and_build(&g, "P", "d").expect("built");
    assert_eq!(st.min, Some(date(3)));
    assert_eq!(st.max, Some(date(27)));
    assert_eq!(st.numeric_count, 0);
    assert_eq!(st.mean(), None);
}

#[test]
fn a_key_with_values_of_two_families_has_no_range() {
    let g = Graph::default();
    g.begin_write().unwrap();
    for v in [Value::Int(1), Value::Bool(true)] {
        g.create_node(vec!["P".into()], props(&[("k", v)])).unwrap();
    }
    g.commit_write();
    let st = consult_and_build(&g, "P", "k").expect("built");
    assert!(st.min.is_none() && st.max.is_none());
}

#[test]
fn a_key_that_is_mostly_declined_has_no_statistics() {
    let long = Value::String("y".repeat(MAX_TEXT_BYTES + 1).into());
    let g = graph(10, |i| {
        let v = if i < 3 { Value::Int(i) } else { long.clone() };
        props(&[("k", v), ("ok", Value::Int(i))])
    });
    assert_eq!(consult_and_build(&g, "P", "k").map(|s| s.count), None);
    assert!(g.key_stats_named("P", "ok").is_some());
}

#[test]
fn a_heavy_hitter_is_found_by_value() {
    let g = graph(2_000, |i| {
        props(&[("k", Value::Int(if i % 4 == 0 { 7 } else { 100 + i }))])
    });
    let st = consult_and_build(&g, "P", "k").expect("built");
    assert!(heavy_count(&st, &Value::Int(7)).unwrap() >= 500);
    assert_eq!(
        heavy_count(&st, &Value::Float(7.0)),
        heavy_count(&st, &Value::Int(7))
    );
    assert_eq!(heavy_count(&st, &Value::Int(-1)), None);
}

#[test]
fn an_unbuilt_label_answers_none_and_a_built_one_answers_a_hit() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    assert!(g.key_stats_named("P", "k").is_none());
    assert!(g.key_stats_named("P", "k").is_none(), "still unbuilt");
    assert_eq!(pump(&g), 1);
    assert_eq!(pump(&g), 0, "nothing left to claim");
    let a = g.key_stats_named("P", "k").expect("built");
    let b = g.key_stats_named("P", "k").expect("built");
    assert!(Arc::ptr_eq(&a, &b), "a hit shares the published statistics");
}

#[test]
fn a_consult_during_a_build_returns_none() {
    let g = graph(5_000, |i| props(&[("k", Value::Int(i))]));
    assert!(g.key_stats_named("P", "k").is_none());
    let label = g.interner.get_id("P").unwrap();
    assert_eq!(g.indexes.key_stats.claim_requested(), Some(label));
    let mut consulted = 0;
    g.build_key_stats_for(label, || {
        assert!(g.key_stats_named("P", "k").is_none());
        consulted += 1;
        true
    });
    assert!(consulted >= 2, "the build ran in chunks");
    assert!(g.key_stats_named("P", "k").is_some());
}

#[test]
fn a_few_writes_keep_the_statistics_and_many_make_them_stale() {
    let g = graph(1_000, |i| props(&[("k", Value::Int(i))]));
    assert!(consult_and_build(&g, "P", "k").is_some());
    let add = |n: i64| {
        g.begin_write().unwrap();
        for i in 0..n {
            g.create_node(vec!["P".into()], props(&[("k", Value::Int(5_000 + i))]))
                .unwrap();
        }
        g.commit_write();
    };
    add(10);
    assert_eq!(g.key_stats_named("P", "k").unwrap().count, 1_000, "kept");
    add(100);
    assert!(
        g.key_stats_named("P", "k").is_none(),
        "stale after the threshold"
    );
    assert_eq!(pump(&g), 1, "the consult asked again");
    assert_eq!(g.key_stats_named("P", "k").unwrap().count, 1_110);
}

#[test]
fn a_delete_counts_against_the_label() {
    let g = graph(200, |i| props(&[("k", Value::Int(i))]));
    assert!(consult_and_build(&g, "P", "k").is_some());
    g.begin_write().unwrap();
    for id in g.all_node_ids().into_iter().take(100) {
        g.delete_node(id, false).unwrap();
    }
    g.commit_write();
    assert!(g.key_stats_named("P", "k").is_none());
    pump(&g);
    assert_eq!(g.key_stats_named("P", "k").unwrap().count, 100);
}

#[test]
fn an_index_rebuild_forgets_the_statistics() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    assert!(consult_and_build(&g, "P", "k").is_some());
    g.rebuild_indexes();
    assert!(g.key_stats_named("P", "k").is_none());
}

#[test]
fn the_build_holds_no_store_lock_between_chunks() {
    let g = graph(9_000, |i| props(&[("k", Value::Int(i))]));
    assert!(g.key_stats_named("P", "k").is_none());
    let label = g.interner.get_id("P").unwrap();
    assert_eq!(g.indexes.key_stats.claim_requested(), Some(label));
    let first = g.all_node_ids()[0];
    g.build_key_stats_for(label, || {
        let (done, wait) = mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                g.begin_write().unwrap();
                g.set_property_node(first, "k", Value::Int(-1));
                g.commit_write();
                done.send(()).unwrap();
            });
            wait.recv_timeout(Duration::from_secs(20))
                .expect("a writer completes while the build is between chunks");
        });
        true
    });
}

#[test]
fn a_build_written_past_its_threshold_still_publishes_and_waits_a_full_threshold() {
    let g = graph(1_000, |i| props(&[("k", Value::Int(i))]));
    assert!(g.key_stats_named("P", "k").is_none());
    let label = g.interner.get_id("P").unwrap();
    assert_eq!(g.indexes.key_stats.claim_requested(), Some(label));
    let add = |n: i64| {
        g.begin_write().unwrap();
        for i in 0..n {
            g.create_node(vec!["P".into()], props(&[("k", Value::Int(9_000 + i))]))
                .unwrap();
        }
        g.commit_write();
    };
    // 200 writes land while the scan runs: past the 64-write floor.
    let mut wrote = false;
    g.build_key_stats_for(label, || {
        if !wrote {
            wrote = true;
            add(200);
        }
        true
    });
    assert!(g.key_stats_named("P", "k").is_some(), "published anyway");
    assert_eq!(pump(&g), 0, "and not requested again by the consult");
    // Fewer than a full threshold behind the publication keeps it...
    add(10);
    assert!(g.key_stats_named("P", "k").is_some());
    // ...and past the threshold turns it stale, so the next consult asks.
    add(100);
    assert!(g.key_stats_named("P", "k").is_none());
    assert_eq!(pump(&g), 1);
}

#[test]
fn an_abandoned_build_returns_its_label_to_the_queue() {
    let g = graph(9_000, |i| props(&[("k", Value::Int(i))]));
    assert!(g.key_stats_named("P", "k").is_none());
    assert_eq!(
        g.build_pending_key_stats(usize::MAX, || false),
        0,
        "none claimed"
    );
    let mut chunks = 0;
    let label = g.interner.get_id("P").unwrap();
    assert_eq!(g.indexes.key_stats.claim_requested(), Some(label));
    g.build_key_stats_for(label, || {
        chunks += 1;
        false
    });
    assert_eq!(chunks, 1, "stopped at the first chunk boundary");
    assert!(g.key_stats_named("P", "k").is_none(), "nothing published");
    assert!(g.indexes.key_stats.counters_agree());
    assert_eq!(pump(&g), 1, "the label is served by the next pump");
    assert!(g.key_stats_named("P", "k").is_some());
}

#[test]
fn the_pump_serves_at_most_the_bound_and_the_rest_wait() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    g.begin_write().unwrap();
    g.create_node(vec!["Q".into()], props(&[("k", Value::Int(1))]))
        .unwrap();
    g.commit_write();
    assert!(g.key_stats_named("P", "k").is_none());
    assert!(g.key_stats_named("Q", "k").is_none());
    assert_eq!(g.build_pending_key_stats(1, || true), 1);
    let built = [g.key_stats_named("P", "k"), g.key_stats_named("Q", "k")];
    assert_eq!(built.iter().filter(|s| s.is_some()).count(), 1, "one label");
    assert_eq!(g.build_pending_key_stats(1, || true), 1, "the other waits");
    assert!(g.key_stats_named("P", "k").is_some() && g.key_stats_named("Q", "k").is_some());
}

#[test]
fn the_any_label_bucket_is_unsupported_and_requests_nothing() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    let star = g.indexes.star_label_id;
    let key = g.interner.get_id("k").unwrap();
    assert!(g.key_stats(star, key).is_none());
    assert_eq!(pump(&g), 0, "no build was requested for it");
}

#[test]
fn a_structured_key_evicts_text_keys_and_a_text_key_never_evicts() {
    let mut ledger = Ledger::with_budget(100);
    let (text, structured) = (KeyClass::Text, KeyClass::Structured);
    assert_eq!(ledger.admit((1, 1), 40, text), Some(vec![]));
    assert_eq!(ledger.admit((1, 2), 40, text), Some(vec![]));
    assert_eq!(
        ledger.admit((1, 3), 40, text),
        None,
        "text is refused when full"
    );
    assert_eq!(
        ledger.admit((1, 4), 40, structured),
        Some(vec![(1, 1)]),
        "structured evicts text"
    );
    assert_eq!(ledger.used(), 80);
    assert_eq!(
        ledger.admit((1, 5), 90, structured),
        None,
        "text cannot make room"
    );
    assert_eq!(ledger.used(), 80, "a refused key disturbs nothing");
    assert_eq!(ledger.admit((1, 6), 40, structured), Some(vec![(1, 2)]));
    let mut released = ledger.release_label(1);
    released.sort_unstable();
    assert_eq!(released, vec![(1, 4), (1, 6)]);
    assert_eq!(ledger.used(), 0);
}

#[test]
fn the_default_budget_bounds_the_published_bytes() {
    let g = graph(200, |i| {
        props(&[("a", Value::Int(i)), ("b", Value::Int(i % 3))])
    });
    assert!(consult_and_build(&g, "P", "a").is_some());
    let used = g.indexes.key_stats.ledger.lock().unwrap().used();
    assert!(used > 0 && used < BUDGET_BYTES);
    let label = g.interner.get_id("P").unwrap();
    assert_eq!(g.key_stats_published(label).len(), 2);
}

/// A scan capped at two keys: the third key is refused from its first value
/// to its last, so it is absent, never published from a partial read, and
/// every published key accounts for each value read.
#[test]
fn a_key_past_the_scan_cap_is_never_published_partially() {
    let mut scan = LabelScan::with_cap(2);
    let text = |s: &str| Value::String(s.into());
    let long = Value::String("y".repeat(MAX_TEXT_BYTES + 1).into());
    for i in 0..10i64 {
        scan.observe(1, &text(&format!("t{}", i % 3)));
        scan.observe(2, &Value::Int(i));
        // Key 3 arrives after the cap is full and keeps arriving.
        scan.observe(3, &Value::Int(i));
        scan.observe(1, &long);
    }
    let finished = scan.finish();
    let keys: Vec<u32> = finished.iter().map(|(k, _, _)| *k).collect();
    assert_eq!(keys, vec![2, 1], "structured first; key 3 absent");
    for (key, _, stats) in &finished {
        match key {
            1 => assert_eq!((stats.count, stats.declined), (10, 10)),
            _ => assert_eq!((stats.count, stats.declined), (10, 0)),
        }
    }
}

/// An integer against a float through the range's own ordering.
fn int_float(i: i64, f: f64) -> std::cmp::Ordering {
    Num::Int(i).order(Num::Float(f))
}

#[test]
fn integers_above_two_to_the_53rd_order_exactly() {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let big = 9_007_199_254_740_993_i64; // 2^53 + 1
    assert_eq!(int_float(big, 9_007_199_254_740_992.0), Greater);
    assert_eq!(int_float(big - 1, 9_007_199_254_740_992.0), Equal);
    assert_eq!(int_float(i64::MAX, 9_223_372_036_854_775_808.0), Less);
    assert_eq!(int_float(i64::MIN, -9_223_372_036_854_775_808.0), Equal);
    assert_eq!(int_float(0, -0.5), Greater);
    assert_eq!(int_float(0, 0.5), Less);
    assert_eq!(int_float(-1, f64::NEG_INFINITY), Greater);
    assert_eq!(int_float(1, f64::INFINITY), Less);
    // `-0.0` ties with `0.0` and with `Int(0)`, the same on every pair.
    assert_eq!(int_float(0, -0.0), Equal);
    assert_eq!(Num::Float(-0.0).order(Num::Float(0.0)), Equal);

    let g = Graph::default();
    g.begin_write().unwrap();
    for v in [
        Value::Int(i64::MAX),
        Value::Int(i64::MAX - 1),
        Value::Int(big),
        Value::Float(9_007_199_254_740_992.0),
    ] {
        g.create_node(vec!["P".into()], props(&[("k", v)])).unwrap();
    }
    g.commit_write();
    let st = consult_and_build(&g, "P", "k").expect("built");
    assert_eq!(st.min, Some(Value::Float(9_007_199_254_740_992.0)));
    assert_eq!(st.max, Some(Value::Int(i64::MAX)));
    let g = Graph::default();
    g.begin_write().unwrap();
    for v in [Value::Int(i64::MAX), Value::Int(i64::MAX - 1)] {
        g.create_node(vec!["P".into()], props(&[("k", v)])).unwrap();
    }
    g.commit_write();
    let st = consult_and_build(&g, "P", "k").expect("built");
    assert_eq!(st.min, Some(Value::Int(i64::MAX - 1)));
    assert_eq!(st.max, Some(Value::Int(i64::MAX)));
}

/// An eviction removes the victim from the one record of membership, so a
/// later release of the victim's label lists only what is still there.
#[test]
fn an_evicted_key_is_not_listed_when_its_label_is_released() {
    let mut ledger = Ledger::with_budget(100);
    assert_eq!(ledger.admit((2, 1), 60, KeyClass::Text), Some(vec![]));
    assert_eq!(ledger.admit((1, 1), 30, KeyClass::Text), Some(vec![]));
    assert_eq!(
        ledger.admit((1, 2), 60, KeyClass::Structured),
        Some(vec![(2, 1)]),
        "the larger text key of another label made room"
    );
    assert!(!ledger.holds(&(2, 1)));
    assert!(ledger.release_label(2).is_empty());
    assert_eq!(ledger.release_label(1).len(), 2);
    assert_eq!(ledger.used(), 0);
}

#[test]
fn a_clear_leaves_the_counters_agreeing_with_the_labels() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    assert!(g.key_stats_named("P", "k").is_none());
    assert!(g.indexes.key_stats.counters_agree());
    g.rebuild_indexes();
    assert!(g.indexes.key_stats.counters_agree());
    assert_eq!(pump(&g), 0, "the request was forgotten");
    assert!(g.key_stats_named("P", "k").is_none(), "asks again");
    assert_eq!(pump(&g), 1, "and is served");
}

/// Requests racing clears never strand a requested label with a zero
/// counter: once the threads stop, the counters agree with the table, and a
/// pending request is always served.
#[test]
fn requests_racing_clears_are_never_stranded() {
    let g = graph(10, |i| props(&[("k", Value::Int(i))]));
    let label = g.interner.get_id("P").unwrap();
    std::thread::scope(|s| {
        s.spawn(|| {
            for _ in 0..2_000 {
                g.indexes.key_stats.clear();
            }
        });
        s.spawn(|| {
            for _ in 0..2_000 {
                g.indexes.key_stats.request(label);
            }
        });
    });
    assert!(g.indexes.key_stats.counters_agree());
    g.indexes.key_stats.request(label);
    assert!(g.indexes.key_stats.counters_agree());
    assert_eq!(pump(&g), 1, "the request is served");
}

#[test]
fn the_payload_of_stored_strings_is_charged() {
    let stat = |value: Value| KeyStats {
        provenance: Provenance::Sketched,
        count: 100,
        declined: 0,
        distinct: 10,
        min: Some(value.clone()),
        max: Some(value.clone()),
        numeric_count: 0,
        int_sum: 0,
        float_sum: 0.0,
        heavy: (0..TOP_K as u64)
            .map(|count| HeavyValue {
                value: value.clone(),
                count,
            })
            .collect(),
    };
    let ints = approx_bytes(&stat(Value::Int(1)));
    let strings = approx_bytes(&stat(Value::String("z".repeat(MAX_TEXT_BYTES).into())));
    assert_eq!(strings - ints, (TOP_K + 2) * MAX_TEXT_BYTES);
}
