//! The measurement harnesses' one counting allocator (the pokec groups and
//! the Vortex lab's kernels).
//!
//! The type is not declared `#[global_allocator]` here: the latency suite is
//! the one binary that must not pay a per-allocation atomic, so only the
//! measuring binaries (`pokec_profile`, `relate-vortex-lab`,
//! `relate-vortex-ab`) install it. All of them read these same counters.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// A pass-through allocator that counts.
pub struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
/// Bytes currently allocated — the resident-bytes signal for a cache arm.
static LIVE: AtomicI64 = AtomicI64::new(0);
/// High-water mark of [`LIVE`] since the last [`reset_peak`].
static PEAK_LIVE: AtomicI64 = AtomicI64::new(0);

fn live_grew(by: i64) {
    let now = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK_LIVE.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method tallies the counters with relaxed atomic arithmetic
// on statics — no allocator memory is read or written — and forwards the
// caller's pointer and layout to the identical `System` method unchanged.
// `GlobalAlloc`'s contract is therefore `System`'s own, and the counters
// are advisory figures no allocation depends on.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        live_grew(layout.size() as i64);
        // SAFETY: `layout` is the caller's valid layout; `System` is the
        // platform allocator and receives it unmodified.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        // SAFETY: `ptr`/`layout` are the pair the caller received from this
        // allocator, which delegated to `System`; forwarding them to
        // `System.dealloc` is the same deallocation `System` promises.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        live_grew(new_size as i64 - layout.size() as i64);
        // SAFETY: `ptr`/`layout` came from this allocator (and so from
        // `System`), and `new_size` is the caller's requested size; both
        // reach `System.realloc` unmodified.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Allocations since the last [`reset_counts`].
pub fn allocs() -> u64 {
    ALLOCS.load(Ordering::Relaxed)
}

/// Bytes requested from the heap since the last [`reset_counts`], summed over
/// every allocation and growth: the A/B figure, stable across allocator
/// behavior.
pub fn alloc_bytes() -> u64 {
    BYTES.load(Ordering::Relaxed)
}

/// Bytes currently allocated by the process. A negative counter is broken
/// accounting (a dealloc without its alloc), never a plausible zero, so it
/// fails loudly instead of reading as an empty heap.
pub fn live_u64() -> u64 {
    u64::try_from(LIVE.load(Ordering::Relaxed)).expect("live bytes are non-negative")
}

/// [`live_u64`] as a signed delta — a harness subtracts a baseline, so a
/// freed-while-measuring run must not wrap.
pub fn live() -> i64 {
    LIVE.load(Ordering::Relaxed)
}

/// The high-water mark of live heap bytes since the last [`reset_peak`].
pub fn peak_live() -> i64 {
    PEAK_LIVE.load(Ordering::Relaxed)
}

/// Restart the high-water mark at the current live size.
pub fn reset_peak() {
    PEAK_LIVE.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Restart the allocation count and byte sum.
pub fn reset_counts() {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
}
