//! Measurement harness: a counting allocator, load-stamped timers, one report
//! line per kernel.
//!
//! Every number the lab reports carries the 1-minute load average at the
//! start and end of the measured loop: the box is shared, so a bare ns/op is
//! not evidence (quiet-box protocol). Allocation counts are load-robust and
//! are the primary signal for the property-store question.

use std::time::Instant;

/// The harnesses' one counting allocator (shared with the pokec groups): the
/// measuring binaries install it as their `#[global_allocator]`; `lib.rs`
/// declares this process's.
pub use relate_query_latency::alloc::Counting;

/// Bytes currently allocated by the process — a cache arm's resident cost,
/// measured as the delta around the arm's build.
pub fn live_bytes() -> u64 {
    relate_query_latency::alloc::live_u64()
}

/// Resident bytes of this process, from the workspace's one sampler. A
/// platform with no sampler fails the measurement instead of reporting a
/// fabricated zero.
pub fn rss_bytes() -> Result<u64, Box<dyn std::error::Error>> {
    relate_test_support::sampling::process_rss_bytes()
        .ok_or_else(|| "no process RSS sampler on this platform".into())
}

/// A deterministic pseudo-random row of a column, spread over `rows`: Knuth's
/// multiplicative step, reduced mod the row count. The one spelling of "pick
/// a probe row" in the lab.
pub fn spread_row(i: u64, rows: u64) -> usize {
    (i.wrapping_mul(2_654_435_761) % rows.max(1)) as usize
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
    relate_query_latency::alloc::reset_counts();
    let t = Instant::now();
    for i in 0..iters {
        f(i);
    }
    let elapsed = t.elapsed();
    let allocs = relate_query_latency::alloc::allocs();
    let bytes = relate_query_latency::alloc::alloc_bytes();
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
/// a measurement is seconds long and the box state is the point. macOS
/// sysctl first, then Linux /proc/loadavg (the CI lane runs there; the
/// first lane run printed load=NaN because sysctl does not exist on it).
pub fn loadavg1() -> f64 {
    if let Some(v) = std::process::Command::new("sysctl")
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
    {
        return v;
    }
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(f64::NAN)
}
