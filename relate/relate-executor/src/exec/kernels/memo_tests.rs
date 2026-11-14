use relate_core::Error;

use super::*;

const WARMUP_CHECKS: u32 = MemoPolicy::MODERATE.warm_up;

/// A meter with no cap.
struct Unmetered;
impl MemoMeter for Unmetered {
    fn charge(&self, _: &mut u64, _: u64) -> Result<()> {
        Ok(())
    }
}

/// A meter that refuses once the running total passes a cap.
struct Capped(u64);
impl MemoMeter for Capped {
    fn charge(&self, total: &mut u64, added: u64) -> Result<()> {
        *total += added;
        if *total > self.0 {
            Err(Error::Execution("over the cap".into()))
        } else {
            Ok(())
        }
    }
}

/// A memo past its warm-up, so the next check is a memoising one.
fn warm() -> NodeMemo {
    let mut memo = NodeMemo::default();
    for node in 0..WARMUP_CHECKS {
        assert!(memo
            .answer(&Unmetered, i64::from(node) + 1_000_000, || Ok(true))
            .unwrap());
    }
    memo
}

// The warm-up: the first checks run the function every time and
// allocate nothing.
#[test]
fn warm_up_never_allocates_or_remembers() {
    let mut memo = NodeMemo::default();
    let mut calls = 0;
    for _ in 0..WARMUP_CHECKS {
        memo.answer(&Unmetered, 7, || {
            calls += 1;
            Ok(true)
        })
        .unwrap();
    }
    assert_eq!(calls, WARMUP_CHECKS, "every warm-up check runs");
    assert_eq!(memo.heap_bytes(), 0, "no table during the warm-up");
}

// Past the warm-up each distinct node is checked once, whatever the
// answer, and repeats are served from the table.
#[test]
fn warm_checks_run_once_per_distinct_node() {
    let mut memo = warm();
    let mut calls = 0;
    for round in 0..5 {
        for node in [1, 2, 3] {
            let want = node % 2 == 1;
            let got = memo
                .answer(&Unmetered, node, || {
                    calls += 1;
                    Ok(want)
                })
                .unwrap();
            assert_eq!(got, want, "round {round} node {node}");
        }
    }
    assert_eq!(calls, 3, "three distinct nodes, three checks");
}

// An error is never remembered: the next ask runs the function again.
#[test]
fn an_error_is_not_cached() {
    let mut memo = warm();
    let failed = memo.answer(&Unmetered, 9, || Err(Error::Execution("boom".into())));
    assert!(failed.is_err());
    let mut calls = 0;
    let retried = memo
        .answer(&Unmetered, 9, || {
            calls += 1;
            Ok(false)
        })
        .unwrap();
    assert!(!retried);
    assert_eq!(calls, 1, "the failed node was not remembered");
}

// The footprint follows the distinct nodes touched, not the number of
// checks: a million repeats over ten nodes stay a ten-entry table.
#[test]
fn footprint_follows_distinct_nodes_not_checks() {
    let mut memo = warm();
    let before = memo.heap_bytes();
    for i in 0..1_000_000i64 {
        memo.answer(&Unmetered, i % 10, || Ok(true)).unwrap();
    }
    let grown = memo.heap_bytes() - before.min(memo.heap_bytes());
    assert!(
        grown <= 64 * 1024,
        "ten nodes cannot grow the table by {grown} bytes"
    );
}

// A batch past the warm-up sends only the misses to the function, in
// one call, and serves the rest from the table.
#[test]
fn batch_computes_only_the_misses_in_one_call() {
    let mut memo = warm();
    let mut out = Vec::new();
    let mut calls = Vec::new();
    let mut run = |memo: &mut NodeMemo, keys: &[i64]| {
        memo.answer_batch(&Unmetered, keys, &mut out, |misses, fresh| {
            calls.push(misses.to_vec());
            fresh.extend(misses.iter().map(|k| k % 2 == 0));
            Ok(())
        })
        .unwrap();
        out.clone()
    };
    assert_eq!(run(&mut memo, &[2, 3, 4]), [true, false, true]);
    assert_eq!(run(&mut memo, &[3, 4, 5, 6]), [false, true, false, true]);
    assert_eq!(calls, vec![vec![2, 3, 4], vec![5, 6]]);
}

// A batch error remembers nothing, and the warm-up batch computes all.
#[test]
fn batch_error_is_not_cached_and_warm_up_computes_all() {
    let mut memo = warm();
    let mut out = Vec::new();
    let failed = memo.answer_batch(&Unmetered, &[7, 8], &mut out, |_, _| {
        Err(Error::Execution("boom".into()))
    });
    assert!(failed.is_err());
    let mut asked = 0;
    memo.answer_batch(&Unmetered, &[7, 8], &mut out, |misses, fresh| {
        asked = misses.len();
        fresh.extend(misses.iter().map(|_| true));
        Ok(())
    })
    .unwrap();
    assert_eq!(asked, 2, "neither key was remembered");

    let mut cold = NodeMemo::default();
    let mut seen = 0;
    cold.answer_batch(&Unmetered, &[1, 2, 3], &mut out, |keys, fresh| {
        seen = keys.len();
        fresh.extend(keys.iter().map(|_| false));
        Ok(())
    })
    .unwrap();
    assert_eq!((seen, cold.heap_bytes()), (3, 0));
}

// The value table memoises any `Copy` value, not only booleans.
#[test]
fn value_table_memoises_any_copy_value() {
    let mut memo = Memo::<NodeValues<u32>>::default();
    for node in 0..WARMUP_CHECKS {
        memo.answer(&Unmetered, i64::from(node) + 9_000_000, || Ok(0))
            .unwrap();
    }
    let mut calls = 0;
    for _ in 0..3 {
        let got = memo
            .answer(&Unmetered, 41, || {
                calls += 1;
                Ok(7)
            })
            .unwrap();
        assert_eq!(got, 7);
    }
    assert_eq!(calls, 1);
}

// Tier 0: computed once per stage; an error is retried.
#[test]
fn once_computes_once_and_retries_after_an_error() {
    let mut once = Once::<u64>::default();
    assert!(once
        .get_or_init(|| Err(Error::Execution("boom".into())))
        .is_err());
    let mut calls = 0;
    for _ in 0..3 {
        let got = once
            .get_or_init(|| {
                calls += 1;
                Ok(99)
            })
            .unwrap();
        assert_eq!(got, 99);
    }
    assert_eq!(calls, 1);
}

// Only a per-key-expensive expression gets the keyed memo.
#[test]
fn only_per_key_expensive_is_keyed() {
    assert!(MemoClass::PerKeyExpensive.policy().is_some());
    assert!(MemoClass::PerKeyModerate.policy().is_some());
    assert!(MemoClass::Cheap.policy().is_none());
    assert!(MemoClass::ConstantPerStage.policy().is_none());
}

// The bitmap's bytes, measured: clustered ids cost 1 KiB per 4096-id
// page plus the directory; scattered ids flatten at the page count.
#[test]
fn paged_bits_bytes_follow_the_pages_touched() {
    let mut clustered = PagedBits::default();
    for id in 0..4096 {
        clustered.insert(id, true);
    }
    assert_eq!(
        clustered.heap_bytes(),
        1024 + clustered.pages.capacity() * 8
    );
    let mut scattered = PagedBits::default();
    for id in 0..4096i64 {
        scattered.insert(id * 4096, id % 2 == 0);
    }
    assert_eq!(scattered.get(4096 * 3), Some(false));
    assert_eq!(scattered.get(4096 * 4), Some(true));
    assert_eq!(scattered.get(1), None);
    assert!(scattered.heap_bytes() <= 4096 * 1024 + 4096 * 8);
}

// A refusing meter freezes the table: no error reaches the caller,
// hits are still served, new answers are computed and not stored.
#[test]
fn a_refusing_meter_freezes_the_table_without_failing() {
    let mut memo = Memo::<PagedBits>::new(
        MemoPolicy {
            warm_up: 0,
            window: u32::MAX,
            ..MemoPolicy::MODERATE
        },
        None,
    );
    let meter = Capped(1100); // page + directory fit, a second page is refused
    let mut calls = 0;
    // Fill the first page (ids 0..4096), then touch a second page.
    for id in 0..4096i64 {
        memo.answer(&meter, id, || Ok(true)).unwrap();
    }
    assert!(!memo.frozen, "the first page fit under the cap");
    memo.answer(&meter, 9000, || Ok(true)).unwrap();
    assert!(memo.frozen, "the second page crossed the cap");
    // Frozen: a new key is computed every time, an old key still hits.
    for _ in 0..3 {
        memo.answer(&meter, 9001, || {
            calls += 1;
            Ok(true)
        })
        .unwrap();
    }
    assert_eq!(calls, 3);
    assert!(memo
        .answer(&meter, 5, || panic!("a stored key must hit"))
        .unwrap());
    assert!(memo.stats().refused >= 3);
}

// The counters tell PROFILE what happened.
#[test]
fn stats_count_probes_hits_inserts_and_bytes() {
    let mut memo = warm();
    let base = memo.stats();
    for _ in 0..4 {
        for node in [10, 11, 12] {
            memo.answer(&Unmetered, node, || Ok(true)).unwrap();
        }
    }
    let s = memo.stats();
    assert_eq!(s.probes - base.probes, 12);
    assert_eq!(s.hits - base.hits, 9);
    assert_eq!(s.inserts - base.inserts, 3);
    assert_eq!(s.refused, 0);
    assert!(s.bytes > 0);
}

// A policy-driven warm-up: the expensive class starts almost at once.
#[test]
fn the_class_policy_sets_the_trigger() {
    let mut memo = Memo::<PagedBits>::new(MemoPolicy::EXPENSIVE, None);
    for node in 0..MemoPolicy::EXPENSIVE.warm_up {
        memo.answer(&Unmetered, i64::from(node), || Ok(true))
            .unwrap();
    }
    assert_eq!(memo.heap_bytes(), 0, "still warming");
    memo.answer(&Unmetered, 7, || Ok(true)).unwrap();
    assert!(memo.heap_bytes() > 0, "memoising after eight inputs");
    assert!(MemoClass::PerKeyModerate.policy().is_some());
    assert!(MemoClass::Cheap.policy().is_none());
}

// The review: a memo whose answers are used once drops its table,
// returns its bytes, and bypasses itself; a memo that hits keeps it.
#[test]
fn a_memo_that_never_hits_disables_itself_and_returns_its_bytes() {
    let live: MemoLive = Default::default();
    let policy = MemoPolicy {
        warm_up: 4,
        window: 64,
        min_hit_percent: 10,
        max_bytes: 1 << 20,
    };
    let mut cold = Memo::<PagedBits>::new(policy, Some(live.clone()));
    for id in 0..200i64 {
        cold.answer(&Unmetered, id * 5000, || Ok(true)).unwrap();
    }
    let stats = cold.stats();
    assert!(stats.disabled, "no repeats: not paying");
    assert_eq!((stats.bytes, live.load(Ordering::Relaxed)), (0, 0));
    // Bypassed: the function runs every time, nothing is stored.
    let mut calls = 0;
    for _ in 0..3 {
        cold.answer(&Unmetered, 1, || {
            calls += 1;
            Ok(true)
        })
        .unwrap();
    }
    assert_eq!(calls, 3);

    let mut hot = Memo::<PagedBits>::new(policy, Some(live.clone()));
    for id in 0..200i64 {
        hot.answer(&Unmetered, id % 5, || Ok(true)).unwrap();
    }
    assert!(!hot.stats().disabled, "mostly repeats: keep it");
    assert!(live.load(Ordering::Relaxed) > 0);
    drop(hot);
    assert_eq!(
        live.load(Ordering::Relaxed),
        0,
        "dropped memo returns its bytes"
    );
}

// The query-wide budget freezes a memo that would cross it.
#[test]
fn the_query_budget_freezes_memos_in_total() {
    let live: MemoLive = Arc::new(AtomicU64::new(MEMO_QUERY_BUDGET - 100));
    let policy = MemoPolicy {
        warm_up: 0,
        ..MemoPolicy::MODERATE
    };
    let mut memo = Memo::<PagedBits>::new(policy, Some(live));
    memo.answer(&Unmetered, 1, || Ok(true)).unwrap();
    assert!(memo.frozen, "the first page crosses the shared budget");
    memo.answer(&Unmetered, 99_999, || Ok(true)).unwrap();
    assert!(memo.stats().refused >= 1);
}
