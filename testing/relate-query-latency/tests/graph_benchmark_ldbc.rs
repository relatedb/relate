//! The whole suite (ADR-0243): fetch, load, and every query's answer
//! checked. Latency is measured but not judged here; ADR-0245's compare
//! does that.
//!
//! - SF0.1, both index profiles, and `profile` end to end: gated on
//!   `RELATE_QUERY_LATENCY=1` (downloads 19 MB on first use and loads
//!   1.8 million elements per test).
//! - SF1, upstream's answers: gated on `RELATE_QUERY_LATENCY_SF1=1`.
//!   Local only (ADR-0243): it downloads 241 MB on first use, loads 20
//!   million elements (about 10 minutes) and peaks at 12 GB RSS.
//!
//! The data cache is `target/query-latency/data` or
//! `RELATE_QUERY_LATENCY_DATA`.
//!
//! ```text
//! RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test graph_benchmark_ldbc
//! RELATE_QUERY_LATENCY_SF1=1 cargo test --profile corpus -p relate-query-latency --test graph_benchmark_ldbc sf1
//! ```

use std::time::Duration;

use relate_query_latency::fetch::data_root;
use relate_query_latency::profile::{profile, ProfileOptions, Sampler};
use relate_query_latency::run::{RunConfig, Status};
use relate_query_latency::suite::{run_suite, GraphOptions, SuiteOptions};
use relate_query_latency::workload::{IndexProfile, ScaleFactor, QIDS};
use relate_test_support::profiler::{DEFAULT_FREQUENCY, IN_PROCESS_SAMPLER};
use relate_test_support::sampling::SAMPLING_SUPPORTED;

mod common;

use common::enabled;

fn run(sf: ScaleFactor, profile: IndexProfile) {
    let report = run_suite(&SuiteOptions {
        graph: GraphOptions {
            sf,
            profile,
            store: None,
            data_root: data_root(),
            download: true,
        },
        qids: QIDS.collect(),
        run: RunConfig {
            min_rounds: 1,
            min_time: Duration::ZERO,
            ..RunConfig::new(sf.default_timeout())
        },
    })
    .unwrap();
    let load = report.graph.load().expect("the suite loaded the graph");
    assert_eq!((load.nodes, load.relationships), sf.published_counts());
    assert_eq!(report.queries.len(), 30);
    // A platform with a sampler wires its peak RSS into the report
    // (`run_suite` samples the process; the report carries the sample, or
    // the documented null where no sample is available).
    assert_eq!(
        report.peak_rss_bytes.is_some_and(|b| b > 0),
        SAMPLING_SUPPORTED
    );
    let failures: Vec<_> = report
        .queries
        .iter()
        .filter(|q| q.status != Status::Ok)
        .map(|q| {
            format!(
                "q{} {:?}: {}",
                q.qid,
                q.status,
                q.detail.as_deref().unwrap_or("")
            )
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_answer_matches_at_sf0_1() {
    if enabled("RELATE_QUERY_LATENCY", "the SF0.1 suite") {
        run(ScaleFactor::Sf0_1, IndexProfile::Upstream);
    }
}

#[test]
fn every_answer_matches_at_sf0_1_with_text_indexes() {
    if enabled("RELATE_QUERY_LATENCY", "the SF0.1 suite") {
        run(ScaleFactor::Sf0_1, IndexProfile::Text);
    }
}

#[test]
fn every_answer_matches_upstream_at_sf1() {
    if enabled("RELATE_QUERY_LATENCY_SF1", "the local-only SF1 suite") {
        run(ScaleFactor::Sf1, IndexProfile::Upstream);
    }
}

#[test]
fn profile_writes_its_outputs_at_sf0_1() {
    if !enabled("RELATE_QUERY_LATENCY", "the SF0.1 suite") {
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let sf = ScaleFactor::Sf0_1;
    let summary = profile(&ProfileOptions {
        graph: GraphOptions {
            sf,
            profile: IndexProfile::Upstream,
            store: None,
            data_root: data_root(),
            download: true,
        },
        qid: 12,
        seconds: Duration::from_millis(500),
        frequency: DEFAULT_FREQUENCY,
        sampler: if IN_PROCESS_SAMPLER {
            Sampler::Pprof
        } else {
            Sampler::None
        },
        timeout: sf.default_timeout(),
        out_dir: Some(out.path().to_path_buf()),
    })
    .unwrap();
    assert_eq!(summary.qid, 12);
    assert!(summary.iterations >= 1);
    assert!(out.path().join("summary.json").is_file());
    // A sampling build records samples; `--sampler none` records none.
    assert_eq!(summary.sampling.is_some(), IN_PROCESS_SAMPLER);
    if let Some(sampling) = summary.sampling {
        assert!(sampling.samples > 0);
        let top = std::fs::read_to_string(out.path().join("top.txt")).unwrap();
        assert!(top.starts_with("q12 at SF0.1"), "{top}");
        assert!(top.contains("relate-executor"), "{top}");
        for f in ["folded.txt", "flame.svg"] {
            assert!(out.path().join(f).is_file(), "{f} missing");
        }
    }
}
