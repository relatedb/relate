//! ADR-0691: the memoized expanded-size walk — the same answer as the plain
//! walk, at a cost linear in the distinct payloads.

use super::*;

fn shared(n: i64) -> Value {
    Value::List(Arc::new((0..n).map(Value::Int).collect()))
}

#[test]
fn the_memoized_walk_agrees_with_the_plain_walk() {
    let v = shared(50);
    let rows = [
        Value::List(Arc::new(vec![v.clone(), v.clone()])),
        Value::List(Arc::new(vec![v.clone(), Value::Int(1)])),
        v,
    ];
    let plain = logical_bytes_within(rows.iter(), u64::MAX);
    let memo = logical_bytes_within_memo(rows.iter(), u64::MAX, &mut ExpansionMemo::new());
    assert_eq!(plain, memo);
    assert!(plain.is_some());
}

#[test]
fn the_memoized_walk_stops_at_the_cap_like_the_plain_walk() {
    let mut v = Value::Int(1);
    for _ in 0..60 {
        v = Value::List(Arc::new(vec![v.clone(), v]));
    }
    assert_eq!(
        logical_bytes_within_memo([&v], 1 << 20, &mut ExpansionMemo::new()),
        None
    );
}

/// A 100,000-element payload wrapped by 20,000 distinct containers is two
/// billion elements to a plain walk (seconds); the memo measures the payload
/// once, so the walk is milliseconds whatever wraps it.
#[test]
fn a_payload_wrapped_by_many_fresh_containers_is_measured_once() {
    let payload = shared(100_000);
    let wrapped: Vec<Value> = (0..20_000)
        .map(|i| Value::List(Arc::new(vec![payload.clone(), Value::Int(i)])))
        .collect();
    let started = std::time::Instant::now();
    let total = logical_bytes_within_memo(wrapped.iter(), u64::MAX, &mut ExpansionMemo::new())
        .expect("no cap");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "the memoized walk must be linear in distinct payloads: {:?}",
        started.elapsed()
    );
    let one = payload.logical_bytes();
    assert_eq!(total, 20_000 * (16 + one + 8));
}
