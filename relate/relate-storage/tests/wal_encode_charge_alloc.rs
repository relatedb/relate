//! The workspace is charged as a commit's redo is encoded, so concurrent
//! oversized commits cannot allocate past the workspace limit: an allocator
//! that counts live bytes sees the encoders' peak stay within the limit plus
//! one growth step, and the budget is whole again once every encoding and
//! every refused attempt has been dropped. The one test of this binary owns
//! the process's allocator counters.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use relate_core::tokens::{LedgerGuard, TokenLedger, WorkspaceCharge};
use relate_core::{Error, Value};
use relate_executor::RedoOp;
use relate_storage::EncodedOps;
use relate_test_support::peak_pool::FixedPool;

struct CountingAllocator;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static MEASURING: AtomicBool = AtomicBool::new(false);

fn allocated(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    if MEASURING.load(Ordering::Relaxed) {
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// SAFETY: System owns the allocations; counters only observe successful operations.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }

    // A reallocation is counted as the worst case it may be: the old and the
    // new block both live while the bytes are copied.
    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let next = System.realloc(ptr, old, new_size);
        if !next.is_null() {
            allocated(new_size);
            LIVE.fetch_sub(old.size(), Ordering::Relaxed);
        }
        next
    }
}

const MIB: usize = 1024 * 1024;
const WRITERS: usize = 8;
/// Each commit's body: its buffer grows through the doubling steps to 4 MiB.
const BODY_BYTES: usize = 3 * MIB;
/// The pool admits two of the eight 4 MiB buffers (a growth to 4 MiB
/// charges the 2 MiB buffer it copies from as well), not all of them.
const LIMIT: u64 = (9 * MIB) as u64;
/// The largest single growth step any of these commits takes.
const STEP: usize = 4 * MIB;

fn big_op() -> RedoOp {
    RedoOp::SetNodeProp {
        id: 1,
        key: "k".into(),
        value: Value::String("x".repeat(BODY_BYTES).into()),
    }
}

#[test]
fn concurrent_oversized_commits_peak_within_the_limit_plus_one_step() {
    let pool = Arc::new(FixedPool::new(LIMIT));
    // Every commit's ops exist before the measurement: the figure is the
    // encoders' own allocation.
    let commits: Vec<Vec<RedoOp>> = (0..WRITERS).map(|_| vec![big_op()]).collect();
    let encoded_together = Arc::new(Barrier::new(WRITERS));
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
    let outcomes: Vec<std::result::Result<u64, Error>> = std::thread::scope(|scope| {
        let handles: Vec<_> = commits
            .iter()
            .map(|ops| {
                let pool = Arc::clone(&pool);
                let encoded_together = Arc::clone(&encoded_together);
                scope.spawn(move || {
                    let mut ledger = TokenLedger::new(LIMIT, 0, pool);
                    let _installed = LedgerGuard::install(&mut ledger);
                    let mut charge = WorkspaceCharge::empty();
                    let encoded = EncodedOps::encode(ops, &mut charge);
                    // Hold every success until all attempts have finished,
                    // as the commit slots hold theirs until the waiter
                    // completes.
                    encoded_together.wait();
                    encoded.map(|encoded| {
                        assert_eq!(charge.bytes(), encoded.reservation_bytes());
                        charge.bytes()
                    })
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    MEASURING.store(false, Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed);

    let admitted: u64 = outcomes.iter().filter_map(|o| o.as_ref().ok()).sum();
    let refused = outcomes.iter().filter(|o| o.is_err()).count();
    assert!(
        admitted > 0,
        "the limit admits at least one commit: {outcomes:?}"
    );
    assert!(refused > 0, "the limit refuses some concurrent commit");
    assert!(admitted <= LIMIT, "admitted charges stay within the limit");
    for refusal in outcomes.iter().filter_map(|o| o.as_ref().err()) {
        assert!(
            matches!(
                refusal,
                Error::ResourceLimit { name, .. }
                    if *name == Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT
            ),
            "{refusal:?}"
        );
    }
    let measured = peak.saturating_sub(before);
    assert!(
        measured <= LIMIT as usize + STEP,
        "encoders peaked at {measured} bytes, past the limit {LIMIT} plus one step {STEP}"
    );
    // Completion and failure both released: the pool is whole again.
    assert_eq!(pool.reserved(), 0);
}
