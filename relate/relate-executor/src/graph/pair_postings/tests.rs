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
fn a_bucket_entry_is_no_wider_than_a_vec_and_a_word() {
    // The inline form is the point: a singleton must not pay a table.
    assert!(std::mem::size_of::<IdBag>() <= 32);
    assert_eq!(IdBag::One(1).as_slice(), &[1]);
}
