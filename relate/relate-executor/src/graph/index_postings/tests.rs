use std::cell::Cell;

use relate_core::Value;

use super::*;

/// What a pair build over candidate ids `0..n` (key `value_of(id)`) did to
/// its integer table: how many times the table GREW while the tail (every
/// candidate after the sampling prefix) was being posted, and its capacity
/// at the end of the build. Every growth is a rehash that holds the old and
/// the new bucket arrays at once, so a build with no growth in the tail has
/// its peak at its steady state.
struct Build {
    tail_growths: usize,
    capacity: usize,
    buckets: usize,
}

fn build(n: i64, value_of: impl Fn(i64) -> i64, distinct: Option<u64>) -> Build {
    let mut st = PropPairIndex::default();
    let last = Cell::new(0usize);
    let growths = Cell::new(0usize);
    let feed = |id: i64, st: &mut PropPairIndex| {
        let gk = GroupKey::for_property_index(&Value::Int(value_of(id))).unwrap();
        st.postings.post(&gk, id);
        let cap = st.postings.capacities().0;
        // The first tail post sees the presize's own reservation; growth is
        // what happens after it.
        if id > PRESIZE_PREFIX as i64 && cap != last.get() {
            growths.set(growths.get() + 1);
        }
        last.set(cap);
    };
    feed_build((0..n).collect(), &mut st, &feed, distinct);
    Build {
        tail_growths: growths.get(),
        capacity: st.postings.capacities().0,
        buckets: st.postings.len(),
    }
}

/// A key whose leading `PRESIZE_PREFIX` candidates are one value and whose
/// rest are all distinct: the sample sees a one-value key and reserves
/// nothing, so the table doubles its way up through the whole tail, each
/// doubling holding two bucket arrays. The published distinct count sizes
/// the table once, so the tail never grows it.
#[test]
fn a_published_distinct_count_stops_an_unrepresentative_prefix_from_growing_the_table() {
    let n = 60_000;
    let value_of = |id: i64| if id < PRESIZE_PREFIX as i64 { 7 } else { id };
    let distinct = (n - PRESIZE_PREFIX as i64 + 1) as u64;

    let sampled = build(n, value_of, None);
    let stated = build(n, value_of, Some(distinct));

    assert_eq!(sampled.buckets, stated.buckets, "the same pair either way");
    assert!(
        sampled.tail_growths >= 5,
        "the sample path doubled through the tail: {}",
        sampled.tail_growths
    );
    assert_eq!(stated.tail_growths, 0, "reserved once, never grown");
    assert!(stated.capacity >= distinct as usize);
}

/// Where the sample already works (a representative, unique prefix) the
/// statistics change nothing: both paths reserve once and never grow.
#[test]
fn a_representative_prefix_builds_the_same_with_or_without_statistics() {
    let n = 40_000;
    let sampled = build(n, |id| id, None);
    let stated = build(n, |id| id, Some(n as u64));
    assert_eq!(sampled.buckets, stated.buckets);
    assert_eq!(sampled.tail_growths, 0);
    assert_eq!(stated.tail_growths, 0);
    assert_eq!(sampled.capacity, stated.capacity);
}
