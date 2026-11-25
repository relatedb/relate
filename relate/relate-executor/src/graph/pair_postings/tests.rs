use super::*;
use relate_core::Value;

fn key(v: Value) -> GroupKey {
    GroupKey::for_property_index(&v).expect("indexable value")
}

#[test]
fn a_bucket_is_one_inline_id_until_a_second_arrives() {
    let mut p = PairPostings::default();
    let k = key(Value::Int(7));
    assert_eq!(p.bucket(&k), None);
    p.post(&k, 42);
    assert_eq!(p.ids(&k), &[42]);
    p.post(&k, 42);
    assert_eq!(p.ids(&k), &[42], "a repeated post is idempotent");
    p.post(&k, 5);
    assert_eq!(p.ids(&k), &[5, 42], "Many is ascending");
}

#[test]
fn many_stays_sorted_and_deduplicated_under_any_post_order() {
    let mut p = PairPostings::default();
    let k = key(Value::String("x".into()));
    for id in [9, 3, 7, 3, 1, 9, 5] {
        p.post(&k, id);
    }
    assert_eq!(p.ids(&k), &[1, 3, 5, 7, 9]);
}

#[test]
fn removal_demotes_to_one_and_an_emptied_bucket_leaves_the_table() {
    let mut p = PairPostings::default();
    let k = key(Value::Int(1));
    for id in [4, 8, 6] {
        p.post(&k, id);
    }
    p.unpost(&k, 6);
    assert_eq!(p.ids(&k), &[4, 8]);
    p.unpost(&k, 4);
    assert_eq!(p.ids(&k), &[8], "one id left is the inline form again");
    p.unpost(&k, 99);
    assert_eq!(p.ids(&k), &[8], "an absent id is a no-op");
    p.unpost(&k, 8);
    assert_eq!(p.bucket(&k), None);
    assert_eq!(p.len(), 0, "the empty bucket's entry is gone");
    p.unpost(&k, 8);
}

#[test]
fn an_integral_float_shares_the_integer_bucket() {
    let mut p = PairPostings::default();
    p.post(&key(Value::Int(3)), 10);
    p.post(&key(Value::Float(3.0)), 11);
    assert_eq!(p.ids(&key(Value::Int(3))), &[10, 11]);
    assert_eq!(p.ids(&key(Value::Float(3.0))), &[10, 11]);
    assert_eq!(p.len(), 1, "one equivalence class, one bucket");
    // A non-integral float is its own class, off the integer table.
    p.post(&key(Value::Float(3.5)), 12);
    assert_eq!(p.ids(&key(Value::Float(3.5))), &[12]);
    assert_eq!(p.ids(&key(Value::Int(3))), &[10, 11]);
}

#[test]
fn keys_come_back_in_the_canonical_group_key_order_across_both_tables() {
    let mut p = PairPostings::default();
    let values = [
        Value::String("b".into()),
        Value::Int(30),
        Value::Float(1.5),
        Value::Bool(true),
        Value::Int(-4),
        Value::String("a".into()),
    ];
    for (n, v) in values.iter().enumerate() {
        p.post(&key(v.clone()), n as i64);
    }
    let mut want: Vec<GroupKey> = values.iter().cloned().map(key).collect();
    want.sort();
    assert_eq!(p.keys_sorted(), want);
}

#[test]
fn an_ascending_bulk_post_appends_and_matches_a_shuffled_one() {
    let mut asc = PairPostings::default();
    let mut shuffled = PairPostings::default();
    let k = key(Value::Int(0));
    let ids: Vec<i64> = (0..2000).collect();
    for id in &ids {
        asc.post(&k, *id);
    }
    // A deterministic permutation of the same ids.
    for step in 0..ids.len() as i64 {
        shuffled.post(&k, (step * 7919) % ids.len() as i64);
    }
    assert_eq!(asc.ids(&k), shuffled.ids(&k));
    assert_eq!(asc.ids(&k).len(), 2000);
}

#[test]
fn a_bucket_entry_is_a_key_word_and_a_slot_word() {
    // The point of the slot: a unique key pays no `Vec` header.
    assert_eq!(std::mem::size_of::<Slot>(), 8);
    assert_eq!(std::mem::size_of::<(i64, Slot)>(), 16);
    let slab = Slab::default();
    assert_eq!(Slot::one(1).ids(&slab), &[1]);
}

#[test]
fn shrinking_after_a_build_keeps_every_bucket_and_leaves_no_slack() {
    let mut p = PairPostings::default();
    let many = key(Value::Int(1));
    let one = key(Value::String("solo".into()));
    for id in 0..100 {
        p.post(&many, id);
    }
    p.post(&one, 7);
    p.shrink_to_fit();
    assert_eq!(p.ids(&many).len(), 100);
    assert_eq!(p.ids(&one), &[7]);
    let Some(SlotKind::Many(index)) = p.ints.map.get(&1).map(|s| s.kind()) else {
        panic!("expected a Many bucket");
    };
    let list = &p.ints.slab.lists[index];
    assert_eq!(list.capacity(), list.len());
}

#[test]
fn a_demoted_bucket_returns_its_slab_slot_for_the_next_many_bucket() {
    let mut p = PairPostings::default();
    let (a, b) = (key(Value::Int(1)), key(Value::Int(2)));
    for id in [4, 8] {
        p.post(&a, id);
    }
    assert_eq!(p.ints.slab.lists.len(), 1);
    p.unpost(&a, 4);
    assert_eq!(p.ints.slab.free, vec![0], "the singleton freed its list");
    for id in [1, 2] {
        p.post(&b, id);
    }
    assert_eq!(p.ints.slab.lists.len(), 1, "the freed slot is reused");
    assert!(p.ints.slab.free.is_empty());
    assert_eq!(p.ids(&a), &[8]);
    assert_eq!(p.ids(&b), &[1, 2]);
}

#[test]
fn id_zero_and_a_large_id_stay_inline() {
    let mut p = PairPostings::default();
    let (z, big) = (key(Value::Int(0)), key(Value::Int(1)));
    p.post(&z, 0);
    p.post(&big, i64::MAX - 1);
    assert_eq!(p.ids(&z), &[0]);
    assert_eq!(p.ids(&big), &[i64::MAX - 1]);
    assert!(
        p.ints.slab.lists.is_empty(),
        "singletons never touch the slab"
    );
}

#[test]
#[should_panic(expected = "non-negative")]
fn a_virtual_negative_id_is_never_posted() {
    PairPostings::default().post(&key(Value::Int(1)), -3);
}

#[test]
#[should_panic(expected = "non-negative")]
fn a_negative_id_is_refused_into_an_existing_many_bucket() {
    let mut p = PairPostings::default();
    let k = key(Value::Int(1));
    p.post(&k, 4);
    p.post(&k, 9);
    p.post(&k, -3);
}

#[test]
fn a_nearly_unique_prefix_reserves_for_the_rest_of_the_build() {
    let mut p = PairPostings::default();
    for id in 0..PRESIZE_PREFIX as i64 {
        p.post(&key(Value::Int(id)), id);
    }
    let before = p.ints.map.capacity();
    p.presize(PRESIZE_PREFIX, 100_000);
    assert!(
        p.ints.map.capacity() >= 100_000,
        "a unique key reserves the remaining candidates: {before} -> {}",
        p.ints.map.capacity()
    );
    assert_eq!(p.others.map.capacity(), 0, "the other table is untouched");
}

#[test]
fn a_low_cardinality_prefix_reserves_nothing() {
    let mut p = PairPostings::default();
    for id in 0..PRESIZE_PREFIX as i64 {
        p.post(&key(Value::Int(id % 5)), id);
    }
    let before = p.ints.map.capacity();
    p.presize(PRESIZE_PREFIX, 10_000_000);
    assert_eq!(p.ints.map.capacity(), before);
}

#[test]
fn a_string_keyed_prefix_reserves_the_general_table_only() {
    let mut p = PairPostings::default();
    for id in 0..PRESIZE_PREFIX as i64 {
        p.post(&key(Value::String(format!("u{id}").into())), id);
    }
    p.presize(PRESIZE_PREFIX, 50_000);
    assert!(p.others.map.capacity() >= 50_000);
    assert_eq!(p.ints.map.capacity(), 0);
}

#[test]
fn removing_an_absent_id_from_a_many_bucket_changes_nothing() {
    let mut p = PairPostings::default();
    let k = key(Value::Int(2));
    for id in [1, 2, 3] {
        p.post(&k, id);
    }
    p.unpost(&k, 99);
    assert_eq!(p.ids(&k), &[1, 2, 3]);
}
