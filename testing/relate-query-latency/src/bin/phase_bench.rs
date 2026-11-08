//! LABS-1120's measurement harness: the cost of the always-on fine phase
//! split on the point-read path, and the price of one `Instant::now` on
//! this machine. A dev tool beside `plan_bench` (not CI-gated): a bin
//! runs only when invoked, so the `RELATE_PHASE_BENCH` env gate of the
//! old test-harness form retired with the move. Run on a release build:
//!
//! ```text
//! cargo run --release -p relate-query-latency --bin phase_bench
//! ```
//!
//! The before/after protocol: run on the unmodified tree, apply the fine
//! stamps, run again — same fixture, same iteration counts — and compare
//! the p50s. The fixture is pokec-shaped (the LABS-790 workload the ticket
//! quotes its ~40µs floor from): a `:User {id}` population behind a range
//! index, point reads by literal id so the plan cache is the path that
//! serves them.

use std::time::Instant;

use relate::Database;
// The canonical percentile (the one every other report computes) — a
// benchmark with its own rank rule measures a different statistic.
use relate_test_support::percentile;

fn main() {
    instant_now_cost();
    point_read_p50_before_after_stamps();
}

/// The price of one clock read — the unit the fine split pays in. Measures
/// `Instant::now` back-to-back (the two-read difference would go negative
/// on coarse clocks); a single read's cost is bounded by the loop's
/// per-iteration overhead, which the same loop without the read
/// subtracts, so the printed number is an upper bound per stamp.
fn instant_now_cost() {
    const N: u64 = 10_000_000;
    let mut sink = 0u64;
    let started = Instant::now();
    for _ in 0..N {
        sink = sink.wrapping_add(Instant::now().elapsed().as_nanos() as u64);
    }
    let per = started.elapsed().as_nanos() as f64 / N as f64;
    // The empty loop's own overhead, measured the same way.
    let mut sink2 = 0u64;
    let started = Instant::now();
    for i in 0..N {
        sink2 = sink2.wrapping_add(i);
    }
    let per_empty = started.elapsed().as_nanos() as f64 / N as f64;
    std::hint::black_box((sink, sink2));
    println!("instant_now: <= {per:.2} ns/read (empty loop {per_empty:.2} ns/iter)");
}

/// The point-read distribution the fine-split stamps ride on: pokec-shaped
/// fixture (`:User {id}` behind a range index), point reads by literal id —
/// the plan-cache path — reported as p50/p99 over repeats, plus the engine's
/// own `elapsed`/`parse`/`plan` medians so the stamps' share of each phase
/// is readable off the same run.
fn point_read_p50_before_after_stamps() {
    const USERS: i64 = 20_000;
    const READS: usize = 20_000;
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE INDEX user_id FOR (n:User) ON (n.id)")
        .unwrap();
    conn.query(&format!(
        "UNWIND range(0, {USERS}) AS i CREATE (:User {{id: i, name: 'u' + toString(i)}})"
    ))
    .unwrap();
    // The read statements, pre-built: literal ids so the auto-parameterized
    // template (ADR-0503) serves every read after the first.
    let statements: Vec<String> = (0..READS)
        .map(|i| format!("MATCH (n:User {{id: {}}}) RETURN n.id", i as i64 % USERS))
        .collect();
    // Warmup: the first read compiles + inserts the template; a few more
    // settle the raw fast path.
    for statement in statements.iter().take(64) {
        conn.query(statement).unwrap();
    }
    for repeat in 0..3 {
        let mut wall: Vec<f64> = Vec::with_capacity(READS);
        let mut engine: Vec<(f64, f64, f64)> = Vec::with_capacity(READS);
        for statement in &statements {
            let started = Instant::now();
            let result = conn.query(statement).unwrap();
            wall.push(started.elapsed().as_secs_f64() * 1e3);
            engine.push((result.elapsed_ms(), result.parse_ms(), result.plan_ms()));
        }
        let elapsed: Vec<f64> = engine.iter().map(|e| e.0).collect();
        let parse: Vec<f64> = engine.iter().map(|e| e.1).collect();
        let plan: Vec<f64> = engine.iter().map(|e| e.2).collect();
        println!(
            "repeat {repeat}: wall p50 {:.3} ms p99 {:.3} ms | engine elapsed p50 {:.3} ms | parse p50 {:.3} ms | plan p50 {:.3} ms",
            percentile(&wall, 0.50),
            percentile(&wall, 0.99),
            percentile(&elapsed, 0.50),
            percentile(&parse, 0.50),
            percentile(&plan, 0.50),
        );
    }
}
