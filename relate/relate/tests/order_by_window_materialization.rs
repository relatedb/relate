//! ADR-0555 §3: an `ORDER BY … LIMIT` window folds its producer by key.
//! A candidate the window rejects costs its keys and nothing else: no
//! projected row, no property read, no row of its own. The allocations of
//! the statement therefore follow the page, never the candidate count.
//!
//! One graph per size, the same statement over both: the allocation-count
//! rise from 4,000 to 16,000 labeled candidates must stay far below one
//! allocation per extra candidate. A path that builds and drops a row (or
//! reads a property into a value) per candidate allocates several per
//! candidate and fails this by an order of magnitude. The one test of this
//! binary owns the process's allocator counters.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use relate::Database;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static MEASURING: AtomicBool = AtomicBool::new(false);

fn counted() {
    if MEASURING.load(Ordering::Relaxed) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// SAFETY: System owns the allocations; the counter only observes calls.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        counted();
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        counted();
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        counted();
        System.realloc(ptr, old, new_size)
    }
}

/// The allocations one run of `statement` makes, after a warm-up run has
/// planned it.
fn allocations_of(conn: &mut relate::Connection, statement: &str) -> usize {
    conn.query(statement).unwrap();
    ALLOCATIONS.store(0, Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
    let result = conn.query(statement).unwrap();
    MEASURING.store(false, Ordering::Relaxed);
    assert_eq!(result.rows.len(), 100, "the page is the LIMIT");
    ALLOCATIONS.load(Ordering::Relaxed)
}

fn allocations_at(nodes: usize, statement: &str) -> usize {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.query(&format!(
        "UNWIND range(1, {nodes}) AS i CREATE (:N {{k: i}})"
    ))
    .unwrap();
    // A second label, so the scan is the labeled one.
    conn.query(&format!(
        "UNWIND range(1, {nodes}) AS i CREATE (:M {{k: i}})"
    ))
    .unwrap();
    allocations_of(&mut conn, statement)
}

#[test]
fn a_window_rejects_candidates_without_building_rows() {
    const SMALL: usize = 4_000;
    const LARGE: usize = 16_000;
    // The id-ordered page of a bare labeled scan. A key that is not the id
    // (`ORDER BY n.k`) still reaches the window through the batch rung's
    // finished container, which builds a row per candidate: that shape is
    // the ordered label index's (REL8-2264), not this test's.
    let statement = "MATCH (n:N) RETURN n.k AS k ORDER BY id(n) LIMIT 100";
    let small = allocations_at(SMALL, statement);
    let large = allocations_at(LARGE, statement);
    let extra_candidates = LARGE - SMALL;
    let per_candidate = (large as f64 - small as f64) / extra_candidates as f64;
    assert!(
        per_candidate < BUDGET_PER_CANDIDATE,
        "`{statement}` allocated {per_candidate:.2} per extra candidate \
         ({small} at {SMALL}, {large} at {LARGE}); a rejected candidate must \
         not build a row or read a property into a value"
    );
}

/// Allocations a rejected candidate may cost.
const BUDGET_PER_CANDIDATE: f64 = 0.5;
