//! `DistinctSet` unit and differential tests (LABS-852): tier boundaries,
//! the per-class tables, null/NaN semantics, row arity, the owned workspace
//! charge (deltas vs the `bytes()` oracle, refusal rollback, clone), the
//! module's layering guard, and a seeded differential run against the
//! reference `HashSet<GroupKey>` / `HashSet<Vec<GroupKey>>`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use relate_core::tokens::{LedgerGuard, TokenLedger, CHARGE_QUANTUM_BYTES};
use relate_core::{Node, PointValue, PropertyMap, Relationship, Temporal, Value};
use relate_test_support::rng::Rng;
use relate_test_support::temporal::dst_overlap_half;

use crate::distinct_set::DistinctSet;
use crate::group_key::{row_key, GroupKey};
use crate::test_support::{
    assert_never_imports_exec_tree, refusing_ledger, roomy_ledger, value_map, PeakPoolDouble,
};

fn int(i: i64) -> Value {
    Value::Int(i)
}

fn nan() -> Value {
    Value::Float(f64::NAN)
}

fn node(id: i64) -> Value {
    Value::from_node(Node {
        id,
        labels: Arc::new(vec!["N".to_string()]),
        properties: PropertyMap::share(BTreeMap::new()),
    })
}

fn rel(id: i64) -> Value {
    Value::from_relationship(Relationship {
        id,
        start: 0,
        end: 0,
        rel_type: "R".to_string(),
        properties: PropertyMap::share(BTreeMap::new()),
    })
}

fn list(items: Vec<Value>) -> Value {
    Value::List(items.into())
}

fn insert(set: &mut DistinctSet, v: &Value) -> bool {
    set.insert_value(v).expect("no budget installed refuses")
}

#[test]
fn boundary_pins_at_15_16_17_words() {
    let mut set = DistinctSet::default();
    for i in 0..15 {
        assert!(insert(&mut set, &int(i)));
    }
    assert_eq!(set.len(), 15);
    assert!(!set.words_promoted(), "k = 2^4 - 1 stays inline");
    assert!(insert(&mut set, &int(15)));
    assert_eq!(set.len(), 16);
    assert!(!set.words_promoted(), "k = 2^4 fills T1 exactly");
    // A duplicate offer at len == 16 matches in T1 and never promotes.
    assert!(!insert(&mut set, &int(7)));
    assert!(!insert(&mut set, &Value::Float(15.0)));
    assert!(!set.words_promoted());
    assert_eq!(set.len(), 16);
    assert!(insert(&mut set, &int(16)));
    assert_eq!(set.len(), 17);
    assert!(set.words_promoted(), "the 17th distinct word promotes");
    for i in 0..17 {
        assert!(
            !insert(&mut set, &int(i)),
            "{i} stays a member after promotion"
        );
    }
    assert_eq!(set.len(), 17);
}

#[test]
fn a_tag_switch_promotes() {
    let mut set = DistinctSet::default();
    for i in 0..3 {
        assert!(insert(&mut set, &int(i)));
    }
    assert!(!set.words_promoted());
    assert!(insert(&mut set, &Value::Bool(true)));
    assert!(set.words_promoted(), "a second tag at k = 3 promotes");
    assert_eq!(set.len(), 4);
    for i in 0..3 {
        assert!(!insert(&mut set, &int(i)));
    }
    assert!(!insert(&mut set, &Value::Bool(true)));
    assert!(insert(&mut set, &Value::Bool(false)));
    assert_eq!(set.len(), 5);
}

#[test]
fn general_keys_do_not_move_the_word_tier() {
    let mut set = DistinctSet::default();
    assert!(insert(&mut set, &int(1)));
    assert!(insert(&mut set, &Value::String("a".into())));
    assert!(insert(&mut set, &list(vec![int(1)])));
    assert!(insert(&mut set, &nan()));
    assert!(!set.words_promoted());
    assert!(!insert(&mut set, &Value::String("a".into())));
    assert!(!insert(&mut set, &list(vec![Value::Float(1.0)])));
    assert!(!insert(&mut set, &int(1)));
    assert_eq!(set.len(), 4);
}

#[test]
fn injectivity_probes_stay_apart() {
    let mut set = DistinctSet::default();
    let apart = [
        int(1),
        node(1),
        rel(1),
        Value::Bool(true),
        Value::Float(1.5),
        // The float whose IEEE bits equal Int(1)'s payload.
        Value::Float(f64::from_bits(1)),
        int(1.5f64.to_bits() as i64),
        int(i64::MIN),
        int(i64::MAX),
        Value::Float(9_223_372_036_854_775_808.0),
        Value::Float(-9_223_372_036_854_775_808.0 * 2.0),
    ];
    for v in &apart {
        assert!(insert(&mut set, v), "{v:?} must be new");
    }
    assert_eq!(set.len(), apart.len());
    // The equivalences Norm already applied hold through the words.
    assert!(!insert(&mut set, &Value::Float(1.0)));
    let mut zeros = DistinctSet::default();
    assert!(insert(&mut zeros, &int(0)));
    assert!(!insert(&mut zeros, &Value::Float(0.0)));
    assert!(!insert(&mut zeros, &Value::Float(-0.0)));
    assert_eq!(zeros.len(), 1);
}

#[test]
fn nan_stays_apart_top_level_and_nested() {
    let mut set = DistinctSet::default();
    assert!(insert(&mut set, &nan()));
    assert!(insert(&mut set, &nan()));
    assert!(insert(&mut set, &list(vec![nan()])));
    assert!(insert(&mut set, &list(vec![nan()])));
    assert!(insert(&mut set, &value_map(&[("a", nan())])));
    assert!(insert(&mut set, &value_map(&[("a", nan())])));
    assert!(set.insert_row(&[nan(), int(1)]).unwrap());
    assert!(set.insert_row(&[nan(), int(1)]).unwrap());
    assert_eq!(set.len(), 8);
}

#[test]
fn null_is_reflexive_in_every_tier() {
    let mut set = DistinctSet::default();
    assert!(insert(&mut set, &Value::Null));
    assert!(!insert(&mut set, &Value::Null));
    for i in 0..20 {
        insert(&mut set, &int(i));
    }
    assert!(set.words_promoted());
    assert!(!insert(&mut set, &Value::Null));
    insert(&mut set, &Value::String("s".into()));
    assert!(!insert(&mut set, &Value::Null));
    assert!(!set.insert_row(&[Value::Null]).unwrap());
    assert_eq!(set.len(), 22);
    // A row of nulls is a row key, reflexive in its own table.
    assert!(set.insert_row(&[Value::Null, Value::Null]).unwrap());
    assert!(!set.insert_row(&[Value::Null, Value::Null]).unwrap());
}

#[test]
fn row_arity_zero_one_two_and_a_mixed_star_sequence() {
    let mut set = DistinctSet::default();
    assert!(set.insert_row(&[]).unwrap());
    assert!(!set.insert_row(&[]).unwrap());
    assert!(set.insert_row(&[int(1)]).unwrap());
    assert!(!set.insert_row(&[Value::Float(1.0)]).unwrap());
    // `insert_row(&[v])` is `insert_value(v)`.
    assert!(!insert(&mut set, &int(1)));
    assert!(set.insert_row(&[int(1), int(2)]).unwrap());
    assert!(!set.insert_row(&[Value::Float(1.0), int(2)]).unwrap());
    assert!(set.insert_row(&[int(2), int(1)]).unwrap());
    // `*` rows: `[x]` and `[x, y]` stay apart whatever the order.
    assert!(set.insert_row(&[int(5), int(6)]).unwrap());
    assert!(set.insert_row(&[int(5)]).unwrap());
    assert!(!set.insert_row(&[int(5), int(6)]).unwrap());
    assert!(!set.insert_row(&[int(5)]).unwrap());
    assert_eq!(set.len(), 6);
}

#[test]
fn summed_deltas_equal_the_bytes_oracle_across_promotions() {
    let mut ledger = roomy_ledger();
    let _guard = LedgerGuard::install(&mut ledger);
    let mut set = DistinctSet::default();
    let offers: Vec<Vec<Value>> = (0..40)
        .map(|i| vec![int(i)])
        .chain([vec![Value::Bool(true)], vec![node(3)], vec![rel(3)]])
        .chain((0..20).map(|i| vec![Value::Float(i as f64 + 0.5)]))
        .chain((0..20).map(|i| vec![Value::String(format!("s{i}").into())]))
        .chain((0..20).map(|i| vec![int(i), int(i % 3)]))
        .chain([vec![], vec![Value::Null], vec![nan()]])
        .collect();
    for (step, row) in offers.iter().enumerate() {
        set.insert_row(row).unwrap();
        assert_eq!(set.charged(), set.bytes(), "after offer {step}");
        // Every NaN is its own key, so a NaN row's re-offer is *new* (and
        // may pay a delta); any other re-offer is a duplicate that adds
        // nothing.
        let before = set.charged();
        let nan_row = row
            .iter()
            .any(|v| matches!(v, Value::Float(f) if f.is_nan()));
        assert_eq!(set.insert_row(row).unwrap(), nan_row);
        if !nan_row {
            assert_eq!(set.charged(), before);
        }
    }
    assert!(set.words_promoted());
}

#[test]
fn promotion_costs_no_delta() {
    let mut ledger = roomy_ledger();
    let _guard = LedgerGuard::install(&mut ledger);
    let mut set = DistinctSet::default();
    for i in 0..16 {
        insert(&mut set, &int(i));
    }
    let inline = set.charged();
    assert_eq!(inline, crate::workspace::word_set_bytes(16));
    insert(&mut set, &int(16));
    assert_eq!(
        set.charged() - inline,
        crate::workspace::word_insert_delta(16),
        "the 17th word pays buffer_growth(16, 17, 8) only"
    );
}

/// The refusal contract at a given offer: `len`, membership and `bytes()`
/// are unchanged, and the refused offer succeeds once the budget allows it
/// again. `offer` runs three times: refused under the refusing ledger, then
/// — after the members check — admitted. Members are row slices so value
/// and row cases share the one shape (`insert_row(&[v])` is
/// `insert_value(v)`).
fn assert_refused_unchanged(
    set: &mut DistinctSet,
    offer: impl Fn(&mut DistinctSet) -> relate_core::Result<bool>,
    members: &[&[Value]],
) {
    let (len, bytes) = (set.len(), set.bytes());
    {
        let mut ledger = refusing_ledger();
        let _guard = LedgerGuard::install(&mut ledger);
        assert!(offer(set).is_err(), "the budget refuses");
    }
    assert_eq!(set.len(), len);
    assert_eq!(set.bytes(), bytes);
    for m in members {
        assert!(!set.insert_row(m).unwrap(), "{m:?} is still a member");
    }
    assert!(offer(set).unwrap(), "the refused offer was rolled back");
}

#[test]
fn refused_charge_leaves_state_unchanged_in_every_tier() {
    // Every refused offer below lands where the power-of-two reservation
    // grows (a positive delta): 4 → 5 words, a fresh tag table, a first
    // single, a first row key.
    // T1.
    let mut set = DistinctSet::default();
    let members: Vec<Vec<Value>> = (0..4).map(|i| vec![int(i)]).collect();
    for m in &members {
        assert!(set.insert_row(m).unwrap());
    }
    let member_refs: Vec<&[Value]> = members.iter().map(Vec::as_slice).collect();
    assert_refused_unchanged(&mut set, |s| s.insert_value(&int(99)), &member_refs);

    // The promoting 17th insert: refused, yet the promotion stays.
    let mut set = DistinctSet::default();
    let members: Vec<Vec<Value>> = (0..16).map(|i| vec![int(i)]).collect();
    for m in &members {
        assert!(set.insert_row(m).unwrap());
    }
    let member_refs: Vec<&[Value]> = members.iter().map(Vec::as_slice).collect();
    assert_refused_unchanged(&mut set, |s| s.insert_value(&int(16)), &member_refs);
    assert!(set.words_promoted(), "rollback does not undo promotion");

    // T2 tag table, general singles, row keys.
    assert_refused_unchanged(
        &mut set,
        |s| s.insert_value(&Value::Bool(true)),
        &member_refs,
    );
    assert_refused_unchanged(
        &mut set,
        |s| s.insert_value(&Value::String("x".into())),
        &member_refs,
    );
    let mut rows = DistinctSet::default();
    assert!(rows.insert_row(&[int(1), int(2)]).unwrap());
    assert_refused_unchanged(
        &mut rows,
        |s| s.insert_row(&[int(3), int(4)]),
        &[&[int(1), int(2)]],
    );
}

#[test]
fn clone_charge_semantics() {
    let mut ledger = roomy_ledger();
    let guard = LedgerGuard::install(&mut ledger);
    let empty = DistinctSet::default();
    assert_eq!(
        empty.try_clone().unwrap().charged(),
        0,
        "an empty clone reserves nothing"
    );
    let mut set = DistinctSet::default();
    for i in 0..20 {
        insert(&mut set, &int(i));
    }
    insert(&mut set, &Value::String("s".into()));
    set.insert_row(&[int(1), int(2)]).unwrap();
    let copy = set.try_clone().unwrap();
    assert_eq!(
        copy.charged(),
        set.bytes(),
        "a filled clone retains bytes()"
    );
    assert_eq!(copy.len(), set.len());
    drop(guard);
    // A refused retain propagates; the consumer's fallback (DistinctAcc's
    // contract) is the uncharged contents copy.
    let mut refusing = refusing_ledger();
    let _guard = LedgerGuard::install(&mut refusing);
    assert!(set.try_clone().is_err());
    let fallback = set.clone_contents();
    assert_eq!(fallback.charged(), 0);
    assert_eq!(fallback.len(), set.len());
}

#[test]
fn the_module_never_imports_the_exec_tree_or_the_walker() {
    assert_never_imports_exec_tree("distinct_set.rs", include_str!("distinct_set.rs"));
}

/// A temporal or point value drawn from `i`: dates, local date-times,
/// local times, offset times, durations, the DST-overlap pair, and 2D/3D
/// points (integral and non-integral coordinates, one NaN coordinate).
fn random_temporal_or_point(rng: &mut Rng, i: i64) -> Value {
    let date = chrono::NaiveDate::from_ymd_opt(2020, 1, 1).expect("a valid date")
        + chrono::Duration::days(i % 9);
    let time = chrono::NaiveTime::from_hms_opt((i % 5) as u32, 0, 0).expect("a valid time");
    match rng.below(9) {
        0 => Value::Temporal(Temporal::Date(date)),
        1 => Value::Temporal(Temporal::LocalDateTime(date.and_time(time))),
        2 => Value::Temporal(Temporal::LocalTime(time)),
        3 => Value::Temporal(Temporal::Time {
            time,
            offset: 3600 * (i % 2) as i32,
        }),
        4 => Value::Temporal(Temporal::Duration {
            months: i % 2,
            days: i % 3,
            nanos: i128::from(i % 4) * 1_000_000_000,
        }),
        5 => dst_overlap_half(i % 2 == 1),
        6 => Value::Point(PointValue {
            srid: 7203,
            x: (i % 6) as f64,
            y: if i % 2 == 0 { 0.0 } else { -0.0 },
            z: None,
        }),
        7 => Value::Point(PointValue {
            srid: 9157,
            x: (i % 4) as f64 + 0.5,
            y: 1.0,
            z: Some((i % 3) as f64),
        }),
        _ => Value::Point(PointValue {
            srid: 7203,
            x: f64::NAN,
            y: 1.0,
            z: None,
        }),
    }
}

/// One random value of the differential vocabulary. `int_only` keeps every
/// value on the `Int` tag (ints, integral floats, ±0.0) so the run crosses
/// the T1 boundary by count rather than by a tag switch.
fn random_value(rng: &mut Rng, k: u64, int_only: bool) -> Value {
    // `below(0)` is 0 by contract (and still draws a word), so k = 0 needs
    // no guard.
    let i = rng.below(k) as i64;
    let kinds = if int_only { 3 } else { 16 };
    match rng.below(kinds) {
        0 => int(i),
        1 => Value::Float(i as f64),
        2 => Value::Float(if i % 2 == 0 { 0.0 } else { -0.0 }),
        3 => Value::Float(i as f64 + 0.25),
        4 => nan(),
        5 => Value::Null,
        6 => Value::Bool(i % 2 == 0),
        7 => Value::String(format!("s{}", i % 7).into()),
        8 => list(vec![int(i % 3), Value::Float((i % 2) as f64)]),
        9 => list(vec![nan()]),
        10 => value_map(&[("a", int(i % 4)), ("b", Value::Null)]),
        11 => node(i),
        12 => rel(i),
        13 | 14 => random_temporal_or_point(rng, i),
        _ => int(i64::MAX - i),
    }
}

#[test]
fn differential_against_the_reference_sets() {
    let mut ledger = roomy_ledger();
    let _guard = LedgerGuard::install(&mut ledger);
    let mut rng = Rng(0x852_d15f_1ac7_5e7d);
    for k in 0..=128u64 {
        for int_only in [true, false] {
            // Values. The callers emit the offers the set admits, so the
            // per-offer record pins first-seen order on top of each
            // decision.
            let mut set = DistinctSet::default();
            let mut reference: HashSet<GroupKey> = HashSet::new();
            // One record per offer: (offered value, new per the set, new
            // per the reference) — the two emitted streams are filters of
            // it, never pushed under separate conditions.
            let mut offers: Vec<(String, bool, bool)> = Vec::new();
            for step in 0..(3 * k + 8) {
                let v = random_value(&mut rng, k, int_only);
                let key = GroupKey::new(&v);
                let expected = !reference.contains(&key) && reference.insert(key);
                let got = set.insert_value(&v).unwrap();
                assert_eq!(got, expected, "k={k} int_only={int_only} step={step} {v:?}");
                assert_eq!(set.len(), reference.len());
                assert_eq!(set.charged(), set.bytes());
                offers.push((format!("{v:?}"), got, expected));
            }
            let emitted: Vec<&str> = offers
                .iter()
                .filter(|(_, got, _)| *got)
                .map(|(v, _, _)| v.as_str())
                .collect();
            let expected_emitted: Vec<&str> = offers
                .iter()
                .filter(|(_, _, expected)| *expected)
                .map(|(v, _, _)| v.as_str())
                .collect();
            assert_eq!(emitted, expected_emitted, "k={k} first-seen order");
            // Rows of arity 0-3, mixed in one sequence (the `*` shape).
            let mut set = DistinctSet::default();
            let mut reference: HashSet<Vec<GroupKey>> = HashSet::new();
            for step in 0..(3 * k + 8) {
                let arity = rng.below(4);
                let row: Vec<Value> = (0..arity)
                    .map(|_| random_value(&mut rng, k, int_only))
                    .collect();
                let key = row_key(&row);
                let expected = !reference.contains(&key) && reference.insert(key);
                let got = set.insert_row(&row).unwrap();
                assert_eq!(
                    got, expected,
                    "k={k} int_only={int_only} step={step} {row:?}"
                );
                assert_eq!(set.len(), reference.len());
                assert_eq!(set.charged(), set.bytes());
            }
        }
    }
}

#[test]
fn dst_overlap_halves_stay_two_keys_beside_every_tier() {
    // `=` holds between the halves, yet they are two grouping keys —
    // whether the word tier is empty, inline, or promoted beside them.
    let (first, second) = (dst_overlap_half(false), dst_overlap_half(true));
    for words in [0, 5, 20] {
        let mut set = DistinctSet::default();
        for i in 0..words {
            assert!(insert(&mut set, &int(i)));
        }
        assert!(insert(&mut set, &first));
        assert!(insert(&mut set, &second), "words={words}");
        assert!(!insert(&mut set, &first));
        assert!(!insert(&mut set, &second));
        assert!(set.insert_row(&[first.clone(), second.clone()]).unwrap());
        assert!(set.insert_row(&[second.clone(), first.clone()]).unwrap());
        assert!(!set.insert_row(&[first.clone(), second.clone()]).unwrap());
        assert_eq!(set.len(), words as usize + 4);
    }
}

#[test]
fn charge_tracks_the_ledger() {
    // The set's own reservation is what the ledger sees.
    let pool = Arc::new(PeakPoolDouble::new(1000 * CHARGE_QUANTUM_BYTES));
    let mut ledger = TokenLedger::new(1000 * CHARGE_QUANTUM_BYTES, 0, pool);
    let _guard = LedgerGuard::install(&mut ledger);
    let mut set = DistinctSet::default();
    for i in 0..100 {
        insert(&mut set, &int(i));
    }
    let (current, _, _) = relate_core::tokens::workspace_meter().unwrap();
    assert_eq!(current, set.bytes());
    drop(set);
    let (current, _, _) = relate_core::tokens::workspace_meter().unwrap();
    assert_eq!(current, 0, "dropping the set releases its charge");
}
