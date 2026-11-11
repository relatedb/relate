//! ADR-0555, ADR-0567: a shortest route's pair-bound probe keeps its peak
//! memory independent of how many nodes match the endpoint's property map.
//! The start side matches nothing, so the statement is only the probe of
//! the end side (an empty product, no search). One graph, two maps of the
//! same label that match 50 and 36,000 of its nodes: the probe stops one
//! match past the bound either way, so the live-byte peak of the two runs
//! stays within a small constant. The one test of this binary owns the
//! process's allocator counters.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use relate::Database;

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

    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let next = System.realloc(ptr, old, new_size);
        if !next.is_null() {
            allocated(new_size);
            LIVE.fetch_sub(old.size(), Ordering::Relaxed);
        }
        next
    }
}

/// The live-byte rise above the starting level while `f` runs.
fn peak_rise(f: impl FnOnce()) -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
    f();
    MEASURING.store(false, Ordering::Relaxed);
    PEAK.load(Ordering::Relaxed).saturating_sub(base)
}

#[test]
fn the_pair_bound_probe_peak_is_independent_of_the_match_count() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    // 40,000 nodes of one label; `few` marks 50 of them, `many` 36,000.
    conn.query(
        "UNWIND range(0, 39999) AS i \
         CREATE (:N {id: toString(i), few: i % 800 = 0, many: i % 10 <> 0})",
    )
    .unwrap();
    conn.query("MATCH (a:N {id: '0'}), (b:N {id: '1'}) CREATE (a)-[:R]->(b)")
        .unwrap();
    let probe = |prop: &str| {
        format!(
            "MATCH p = ANY SHORTEST (a {{id: 'nope'}})-[:R*..2]->(b:N {{{prop}: true}}) \
             RETURN length(p)"
        )
    };
    let (few, many) = (probe("few"), probe("many"));
    // The unindexed scan, then the index-served stream: neither may follow
    // the match count.
    for indexed in [false, true] {
        if indexed {
            conn.query("CREATE INDEX n_few FOR (n:N) ON (n.few)")
                .unwrap();
            conn.query("CREATE INDEX n_many FOR (n:N) ON (n.many)")
                .unwrap();
        }
        // Warm both plans so the measured runs carry no one-time allocation.
        assert!(conn.query(&few).unwrap().rows.is_empty());
        assert!(conn.query(&many).unwrap().rows.is_empty());
        let small = peak_rise(|| {
            conn.query(&few).unwrap();
        });
        let large = peak_rise(|| {
            conn.query(&many).unwrap();
        });
        // 36,000 matches listed in full would add ~290 KB of ids alone; the
        // probe's rise must not follow the match count.
        assert!(
            large <= small + 64 * 1024,
            "probe peak grew with the match count (indexed: {indexed}): \
             {small} bytes at 50 matches, {large} at 36,000"
        );
    }
}
