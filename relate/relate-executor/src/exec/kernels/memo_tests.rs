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
        MemoLive::default(),
    );
    let meter = Capped(1100); // page + directory fit, a second page is refused
    let mut calls = 0;
    // Fill the first page (ids 0..4096), then touch a second page.
    for id in 0..4096i64 {
        memo.answer(&meter, id, || Ok(true)).unwrap();
    }
    assert!(!memo.is_frozen(), "the first page fit under the cap");
    memo.answer(&meter, 9000, || Ok(true)).unwrap();
    assert!(memo.is_frozen(), "the second page crossed the cap");
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
    let mut cold = Memo::<PagedBits>::new(policy, live.clone());
    for id in 0..200i64 {
        cold.answer(&Unmetered, id * 5000, || Ok(true)).unwrap();
    }
    let stats = cold.stats();
    assert!(stats.disabled, "no repeats: not paying");
    assert_eq!((stats.bytes, live.live_bytes()), (0, 0));
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

    let mut hot = Memo::<PagedBits>::new(policy, live.clone());
    for id in 0..200i64 {
        hot.answer(&Unmetered, id % 5, || Ok(true)).unwrap();
    }
    assert!(!hot.stats().disabled, "mostly repeats: keep it");
    assert!(live.live_bytes() > 0);
    drop(hot);
    assert_eq!(live.live_bytes(), 0, "dropped memo returns its bytes");
}

// The query-wide budget freezes a memo that would cross it.
#[test]
fn the_query_budget_freezes_memos_in_total() {
    let live = MemoLive::default();
    live.charge(live.budget() - 100);
    let policy = MemoPolicy {
        warm_up: 0,
        ..MemoPolicy::MODERATE
    };
    let mut memo = Memo::<PagedBits>::new(policy, live);
    memo.answer(&Unmetered, 1, || Ok(true)).unwrap();
    assert!(memo.is_frozen(), "the first page crosses the shared budget");
    memo.answer(&Unmetered, 99_999, || Ok(true)).unwrap();
    assert!(memo.stats().refused >= 1);
}

// ADR-0657: a zero budget allocates no table and runs the function every
// time; the ceiling derives as half the configured budget.
#[test]
fn a_zero_budget_allocates_no_table_and_always_computes() {
    let live = MemoLive::new(0);
    let mut memo = Memo::<PagedBits>::new(MemoPolicy::moderate(&live), live.clone());
    let mut calls = 0;
    for id in 0..5000i64 {
        memo.answer(&Unmetered, id % 3, || {
            calls += 1;
            Ok(true)
        })
        .unwrap();
    }
    assert_eq!(calls, 5000);
    let s = memo.stats();
    assert_eq!((s.probes, s.inserts, s.bytes), (0, 0, 0));
    assert_eq!(live.live_bytes(), 0);
}

#[test]
fn the_table_ceiling_is_half_the_configured_budget() {
    for mib in [1u64, 8, 32] {
        let live = MemoLive::new(mib << 20);
        assert_eq!(
            MemoPolicy::moderate(&live).max_bytes,
            usize::try_from(mib << 19).unwrap()
        );
    }
    assert_eq!(MemoPolicy::MODERATE.max_bytes, 8 << 20);
}

// The wired sites (frontier guard, re-arrival, distinct endpoint) all
// run `Memoized` under `MODERATE`. The harness prints no hit rate for
// them (wired_site_numbers), so each is modelled by its key stream: a
// cold pass over a large domain that later repeats, which the review must
// keep, and a stream that never repeats, which it must still drop. A
// window as short as the pre-fix 4096 reads the cold pass as zero reuse
// and disables the memo that pays.
#[test]
fn the_review_window_keeps_a_memo_over_a_large_domain_that_repeats() {
    const PRE_FIX_WINDOW: u32 = 4096;
    let probes = i64::from(MemoPolicy::MODERATE.window);
    let domain = 60_000i64;
    let run = |policy: MemoPolicy, keys: &dyn Fn(i64) -> i64| {
        let mut memo = Memo::<PagedBits>::new(policy, MemoLive::default());
        for i in 0..(i64::from(policy.warm_up) + probes) {
            memo.answer(&Unmetered, keys(i), || Ok(true)).unwrap();
        }
        memo.stats()
    };
    let repeating = |i: i64| i % domain;

    let kept = run(MemoPolicy::MODERATE, &repeating);
    assert!(!kept.disabled, "cumulative hit rate over the window pays");
    assert!(kept.hits * 100 >= kept.probes * 90, "{kept:?}");

    let short = MemoPolicy {
        window: PRE_FIX_WINDOW,
        ..MemoPolicy::MODERATE
    };
    assert!(
        run(short, &repeating).disabled,
        "a window of {PRE_FIX_WINDOW} misjudges the cold start as no reuse"
    );

    let never_repeats = run(MemoPolicy::MODERATE, &|i| i);
    assert!(never_repeats.disabled, "all-distinct keys still disable");
}

/// A meter that adds up what the memo charges, for the bytes pin below.
#[derive(Default)]
struct Summing(std::cell::Cell<u64>);
impl MemoMeter for Summing {
    fn charge(&self, total: &mut u64, added: u64) -> Result<()> {
        *total += added;
        self.0.set(self.0.get() + added);
        Ok(())
    }
}

// ADR-0654: the PROFILE counters are the stats unchanged in meaning, the
// disabled flag counted; `bytes` equals the table's allocation and every
// one of those bytes was charged to the ADR-0463 retained meter.
#[test]
fn profile_counters_are_the_stats_and_their_bytes_are_the_charged_bytes() {
    let meter = Summing::default();
    let mut memo = NodeMemo::default();
    for node in 0..WARMUP_CHECKS {
        memo.answer(&meter, i64::from(node), || Ok(true)).unwrap();
    }
    for node in 0..10_000 {
        memo.answer(&meter, node, || Ok(node % 2 == 0)).unwrap();
        memo.answer(&meter, node, || Ok(node % 2 == 0)).unwrap();
    }
    let stats = memo.stats();
    let counters = stats.counters();
    assert_eq!(counters.probes, stats.probes);
    assert_eq!(counters.hits, stats.hits);
    assert_eq!(counters.inserts, stats.inserts);
    assert_eq!(counters.refused, stats.refused);
    assert_eq!(counters.disabled_count, 0);
    assert!(counters.hits > 0 && counters.bytes > 0);
    assert_eq!(counters.bytes, memo.heap_bytes() as u64);
    assert_eq!(meter.0.get(), counters.bytes, "every table byte is charged");
}

#[test]
fn a_disabled_memo_counts_once_in_the_sums() {
    let mut memo = NodeMemo::new(
        MemoPolicy {
            warm_up: 0,
            window: 8,
            min_hit_percent: 50,
            max_bytes: 1 << 20,
        },
        MemoLive::default(),
    );
    for node in 0..32 {
        memo.answer(&Unmetered, node, || Ok(true)).unwrap();
    }
    let mut sum = memo.stats().counters();
    sum += memo.stats().counters();
    assert_eq!(sum.disabled_count, 2, "two disabled memos sum to two");
    assert_eq!(sum.bytes, 0, "the dropped table reads zero");
}
