//! Measurement harness: a counting allocator, load-stamped timers, one report
//! line per kernel.
//!
//! Every number the lab reports carries the 1-minute load average at the
//! start and end of the measured loop: the box is shared, so a bare ns/op is
//! not evidence (quiet-box protocol). Allocation counts are load-robust and
//! are the primary signal for the property-store question.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A pass-through allocator that counts.
pub struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// One measurement: what the kernel cost, and how loaded the box was.
#[derive(Debug, Clone)]
pub struct Measured {
    pub name: String,
    pub iters: u64,
    pub ns_per_op: f64,
    pub allocs_per_op: f64,
    pub bytes_per_op: f64,
    pub load_start: f64,
    pub load_end: f64,
}

impl Measured {
    pub fn report(&self) {
        println!(
            "{:<44} {:>9.1} ns/op  {:>8.1} allocs/op  {:>9.0} B/op  load {:.1}->{:.1}",
            self.name,
            self.ns_per_op,
            self.allocs_per_op,
            self.bytes_per_op,
            self.load_start,
            self.load_end
        );
    }
}

/// Run `iters` iterations of `f`, counting allocations and stamping the load.
pub fn measure<F: FnMut(u64)>(name: &str, iters: u64, mut f: F) -> Measured {
    let load_start = loadavg1();
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    let t = Instant::now();
    for i in 0..iters {
        f(i);
    }
    let elapsed = t.elapsed();
    let allocs = ALLOCS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);
    let load_end = loadavg1();
    Measured {
        name: name.to_string(),
        iters,
        ns_per_op: elapsed.as_nanos() as f64 / iters as f64,
        allocs_per_op: allocs as f64 / iters as f64,
        bytes_per_op: bytes as f64 / iters as f64,
        load_start,
        load_end,
    }
}

/// The 1-minute load average. Shelling out once per measurement is fine —
/// a measurement is seconds long and the box state is the point.
pub fn loadavg1() -> f64 {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            // "{ 12.34 5.67 8.90 }"
            s.trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(f64::NAN)
}

/// Uncompressed bytes of a value-count, for the size table's denominator.
pub fn raw_bytes(cols: &[(String, crate::store::Ty, crate::store::Col)]) -> u64 {
    cols.iter().map(|(_, _, c)| c.raw_bytes()).sum()
}
