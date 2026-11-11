//! The in-process sampler on a CPU-bound query over a small in-memory
//! graph: no data set needed, so this always runs where sampling is built
//! (unix, the `sampling` feature). It checks that samples are recorded and
//! attributed to the engine, not exact shares, which vary run to run.
#![cfg(all(unix, feature = "sampling"))]

use std::time::Duration;

use relate::Database;
use relate_query_latency::profile::query_loop;
use relate_query_latency::run::Watchdog;
use relate_query_latency::workload::Query;
use relate_test_support::profiler::sample;
use relate_test_support::stacks::top_text;

#[test]
fn sampling_a_cpu_bound_query_records_engine_stacks() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.execute("UNWIND range(1, 400) AS i CREATE (:N {i: i})")
        .unwrap();
    let q = Query {
        qid: 1,
        header: String::new(),
        text: "MATCH (a:N), (b:N) WHERE a.i < b.i RETURN count(*) AS c".into(),
        sha256: String::new(),
    };
    let watchdog = Watchdog::new();
    let sampled = sample(997, || {
        query_loop(
            &mut conn,
            &q,
            Duration::from_millis(500),
            Duration::from_secs(60),
            &watchdog,
        )
    })
    .unwrap();
    let stats = sampled.value.unwrap();
    assert!(stats.iterations >= 1);
    assert!(sampled.stacks.total() > 0, "no samples recorded");
    let executor = sampled
        .stacks
        .iter()
        .any(|(stack, _)| stack.iter().any(|f| f.group == "relate-executor"));
    assert!(executor, "no stack reaches the executor");
    assert!(sampled.flamegraph_svg.starts_with(b"<?xml"));
    let top = top_text(&sampled.stacks, &["header".to_string()], 10);
    assert!(top.contains("relate-executor"), "{top}");
}
