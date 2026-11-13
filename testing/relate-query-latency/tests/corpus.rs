//! The QPP/LDBC corpus lane end to end (ADR-0479, LABS-646).
//!
//! - Ungated, every workload (fast, no data set): every corpus file must
//!   plan — `ok` and `kept` entries EXPLAIN, `error-pin` entries fail
//!   with their authored error — and the fraud workload's full ratchet
//!   runs against generated and fixture graphs (CI-scale generation).
//! - The pathdb corpus at SF0.1: gated on `RELATE_QUERY_LATENCY=1`
//!   (loads the 1.8-million-element graph-benchmark-ldbc graph on first use), asserting
//!   the whole ratchet: every `ok` entry ok, every `kept` entry planned,
//!   every `error-pin` entry failed with its authored error, and the
//!   degree-median policy reproducing the recorded seeds exactly (the
//!   data set is pinned, so a seed diff means the params file and the
//!   data drifted apart).
//! - The snb-bi corpus at SF0.1: gated on the same
//!   `RELATE_QUERY_LATENCY=1` (the same graph-benchmark-ldbc graph), asserting its
//!   ratchet — every `ok` entry answers within the timeout with its
//!   recorded parameters, every `kept` entry plans.
//! - The ukrailway corpus: gated on the same
//!   `RELATE_QUERY_LATENCY=1` (fetches the 18 MB dogofbrian/ukrailway
//!   data set on first use, then imports it into `:memory:`), asserting
//!   its ratchet — both `ok` entries answer within the lane's own
//!   minutes-scale timeout and match the verified oracle exactly.
//! - The agg-stress corpus at SF0.1: gated on the same
//!   `RELATE_QUERY_LATENCY=1` (the same graph-benchmark-ldbc graph), asserting its
//!   ratchet — every `ok` entry answers within the lane's own
//!   tens-of-seconds-scale timeout and matches the verified oracle
//!   exactly (an engine-self oracle: the LABS-733 workload's own
//!   recorded answers over the pinned data).
//! - The equi-join corpus at SF0.1: gated on the same
//!   `RELATE_QUERY_LATENCY=1` (the same graph-benchmark-ldbc graph), asserting its
//!   ratchet — every `ok` entry answers within the lane's own timeout
//!   and matches the verified oracle exactly (an engine-self oracle:
//!   the LABS-1531 workload's own recorded pair counts over the pinned
//!   data).
//! - The pathdb SF1 subset (26 queries, the spike's recorded seeds):
//!   gated on `RELATE_QUERY_LATENCY_SF1=1`, local only — the same
//!   data cost as the graph-benchmark-ldbc lane's SF1 test.
//!
//! ```text
//! cargo test -p relate-query-latency --test corpus
//! RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus pathdb
//! RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus snb_bi
//! RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus ukrailway
//! RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus agg_stress
//! RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus equi_join
//! RELATE_QUERY_LATENCY_SF1=1 cargo test --profile corpus -p relate-query-latency --test corpus sf1
//! ```

use relate::Database;
use relate_datasets::fraud::FraudConfig;
use relate_query_latency::corpus::{
    fraud_oracle, pin_met, run_corpus, workload_queries, Classification, CorpusConfig,
    CorpusReport, FraudCheck, Lane, SeedPolicy, Workload,
};
use relate_query_latency::fetch::data_root;
use relate_query_latency::run::Status;
use relate_query_latency::suite::GraphOptions;
use relate_query_latency::workload::{IndexProfile, ScaleFactor};

mod common;

use common::enabled;

/// The config a run of `workload` runs under, its per-entry timeout
/// resolved off the built lane — the same resolution the binary's
/// corpus arm runs. `sf` feeds the graph-benchmark-ldbc-store lanes' graph options (the
/// fraud and ukrailway lanes carry no graph scale factor).
fn corpus_config(workload: Workload, sf: ScaleFactor) -> CorpusConfig {
    // The graph-benchmark-ldbc-store lanes' graph options, constructed in exactly the
    // arms that open a graph-benchmark-ldbc store; the fraud and ukrailway lanes open
    // none.
    let graph = || GraphOptions {
        sf,
        profile: IndexProfile::Upstream,
        store: None,
        data_root: data_root(),
        download: true,
    };
    let lane = match workload {
        Workload::Pathdb => Lane::Pathdb {
            graph: graph(),
            seed_policy: SeedPolicy::Median,
        },
        Workload::SnbBi => Lane::SnbBi { graph: graph() },
        Workload::FraudQpp => Lane::Fraud {
            // RELATE_FULL_DATA=1 opts the fraud lane up to the
            // generator's full scale (the datasets crate's own gate).
            fraud: FraudConfig::from_env(),
        },
        Workload::Ukrailway => Lane::Ukrailway {
            data_root: data_root(),
            download: true,
        },
        Workload::AggStress => Lane::AggStress { graph: graph() },
        Workload::EquiJoin => Lane::EquiJoin { graph: graph() },
    };
    CorpusConfig {
        timeout: lane.default_timeout(),
        lane,
        repeats: 0,
    }
}

fn assert_ratchet(report: &CorpusReport) {
    let failures: Vec<_> = report
        .failures()
        .into_iter()
        .map(|e| {
            format!(
                "{} [{:?} {:?}]: {}",
                e.id,
                e.classification,
                e.status,
                e.detail.as_deref().unwrap_or("")
            )
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} entries missed their expectation:\n{}",
        failures.len(),
        report.entries.len(),
        failures.join("\n")
    );
}

/// Every corpus file, every workload: `ok` and `kept` entries plan,
/// `error-pin` entries fail with the error their marker names. No data —
/// this is the dialect net that catches a parse or plan regression in
/// any of the 231 files the moment it lands.
#[test]
fn every_corpus_file_plans_or_pins_its_error() {
    for workload in Workload::ALL {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        for q in workload_queries(workload).unwrap() {
            match &q.classification {
                Classification::Ok | Classification::Kept => {
                    conn.explain_plan(&q.text)
                        .unwrap_or_else(|e| panic!("{}: {e}", q.id));
                }
                Classification::ErrorPin { error } => {
                    // A pin's authored fail-closed error fires before any
                    // parameter is bound, so it is checked with none.
                    let err = conn
                        .query_with_params(&q.text, Default::default())
                        .err()
                        .unwrap_or_else(|| {
                            panic!(
                                "{}: the pin query succeeded; it must fail with {error}",
                                q.id
                            )
                        })
                        .to_string();
                    // The one pin-match spelling, shared with the runner.
                    assert!(
                        pin_met(error, &err),
                        "{}: expected the authored {error} pin, got: {err}",
                        q.id
                    );
                }
            }
        }
    }
}

/// The fraud workload's whole ratchet in the normal lane: f01/f02 find
/// the planted ring in the generated graph, f06 answers its fixture
/// oracle exactly, the kept entries plan.
#[test]
fn the_fraud_workload_meets_its_ratchet() {
    let report = run_corpus(&corpus_config(Workload::FraudQpp, ScaleFactor::Sf0_1)).unwrap();
    assert_eq!(report.entries.len(), 6);
    assert_ratchet(&report);
    // The oracle wiring is the point of this workload: every executable
    // entry was checked, not merely run.
    for id in [
        "f01-monopartite-cycle",
        "f02-simple-cycle",
        "f06-sample-ring",
    ] {
        let e = report.entries.iter().find(|e| e.id == id).unwrap();
        assert_eq!(e.status, Status::Ok, "{id}: {:?}", e.detail);
        assert!(e.rows.is_some(), "{id}: an executed entry reports rows");
    }
    assert!(fraud_oracle().unwrap().len() >= 6);
}

/// The pathdb corpus at SF0.1: the full ratchet over the graph-benchmark-ldbc store, plus
/// the seed-policy self-check (the recorded seeds must be exactly what
/// the degree-median policy resolves on the pinned data).
#[test]
fn the_pathdb_workload_meets_its_ratchet_at_sf0_1() {
    if !enabled("RELATE_QUERY_LATENCY", "the SF0.1 pathdb corpus") {
        return;
    }
    let report = run_corpus(&corpus_config(Workload::Pathdb, ScaleFactor::Sf0_1)).unwrap();
    assert_eq!(report.entries.len(), 207, "142 queries + 65 restrictors");
    assert_ratchet(&report);
    assert!(
        report.seed_diffs.is_empty(),
        "the pinned data resolved different seeds than the params file records: {:?}",
        report.seed_diffs
    );
}

/// The snb-bi corpus at SF0.1: the full ratchet over the graph-benchmark-ldbc store with
/// the workload's recorded parameters — every `ok` entry answers within
/// the timeout, every `kept` entry plans. Run-and-report (ADR-0479): no
/// oracle exists for SNB BI over a graph-benchmark-ldbc store (upstream's parameter ids
/// reference their own datagen run), so what gates is completion and the
/// ratchet, never the answers.
#[test]
fn the_snb_bi_workload_meets_its_ratchet_at_sf0_1() {
    if !enabled("RELATE_QUERY_LATENCY", "the SF0.1 snb-bi corpus") {
        return;
    }
    let report = run_corpus(&corpus_config(Workload::SnbBi, ScaleFactor::Sf0_1)).unwrap();
    assert_eq!(
        report.entries.len(),
        16,
        "bi-01..bi-18 minus bi-10 and bi-15 (bi-19 and bi-20 are skipped too, \
         outside that range)"
    );
    assert_ratchet(&report);
    // The lane ran over the graph-benchmark-ldbc store and recorded the scale factor —
    // and no seed policy, which it does not run.
    assert_eq!(report.sf, Some(ScaleFactor::Sf0_1));
    assert_eq!(report.seed_policy, None);
}

/// The ukrailway corpus: the full ratchet over the workload's own
/// imported graph — the fetched, checksum-pinned dogofbrian/ukrailway
/// data set loaded into `:memory:` by the workload's import script — with
/// both `ok` entries answer-checked against the 2026-11-02 verified
/// oracle under the lane's own minutes-scale timeout (r02 measures in
/// minutes; the default 10 s bound would fail the ratchet on day one).
#[test]
fn the_ukrailway_workload_meets_its_ratchet() {
    if !enabled("RELATE_QUERY_LATENCY", "the ukrailway corpus") {
        return;
    }
    let report = run_corpus(&corpus_config(Workload::Ukrailway, ScaleFactor::Sf0_1)).unwrap();
    assert_eq!(report.entries.len(), 2, "r02 and r03");
    assert_ratchet(&report);
    // Every entry executed and was answer-checked: one verified row each.
    for e in &report.entries {
        assert_eq!(e.status, Status::Ok, "{}: {:?}", e.id, e.detail);
        assert_eq!(e.rows, Some(1), "{}: one verified row", e.id);
    }
    // The lane ran no scale factor and no seed policy — neither fact
    // exists for a one-size, literal-anchored workload.
    assert_eq!(report.sf, None);
    assert_eq!(report.seed_policy, None);
}

/// The agg-stress corpus at SF0.1: the full ratchet over the graph-benchmark-ldbc store
/// — every `ok` entry answers within the lane's own tens-of-seconds
/// timeout (a02, the composite-key fold, measures 15.3 s and a01, the
/// headline trail-count fold, 10.3 s; the default 10 s graph-benchmark-ldbc-store bound
/// would fail the ratchet on day one) and
/// matches the 2026-11-02 verified oracle exactly. The oracle is
/// engine-self (no upstream query of the workload's shapes exists), so
/// it gates answer regressions in the fold itself, never upstream
/// behaviour.
#[test]
fn the_agg_stress_workload_meets_its_ratchet_at_sf0_1() {
    // Ten compact rows per entry, a04's complete fold output among them.
    assert_store_lane_ratchet(
        "the SF0.1 agg-stress corpus",
        Workload::AggStress,
        5,
        "a01..a05",
        10,
    );
}

/// The equi-join workload at SF0.1 (LABS-1531): the three anchorless
/// two-pattern entries run answer-checked against the engine-self
/// oracle — a single count row each, the pair counts the product
/// admits — so it gates answer regressions in the join itself, never
/// upstream behaviour.
#[test]
fn the_equi_join_workload_meets_its_ratchet_at_sf0_1() {
    // One count row each — e01 2184 selective pairs, e02 723576 fan-out
    // pairs, e03 3537568 output pairs.
    assert_store_lane_ratchet(
        "the SF0.1 equi-join corpus",
        Workload::EquiJoin,
        3,
        "e01..e03",
        1,
    );
}

/// The store-lane ratchet's shared body: run the lane, then the four
/// facts every rows-oracle store lane asserts — the entry count, the
/// ratchet, the recorded rows per executed entry, and the lane-scoped
/// facts (the graph-benchmark-ldbc store's scale factor recorded, no seed
/// policy, which none of these lanes runs). The gate variable is part of
/// this body, not a caller choice: every rows-oracle store lane gates on
/// the same one, so leaving it open would invite a test that silently
/// skips. Each workload's test is one call with its own counts and its
/// own prose about what its oracle gates.
fn assert_store_lane_ratchet(
    what: &str,
    workload: Workload,
    entries: usize,
    ids: &str,
    rows_per_entry: usize,
) {
    if !enabled("RELATE_QUERY_LATENCY", what) {
        return;
    }
    let report = run_corpus(&corpus_config(workload, ScaleFactor::Sf0_1)).unwrap();
    assert_eq!(report.entries.len(), entries, "{ids}");
    assert_ratchet(&report);
    for e in &report.entries {
        assert_eq!(e.status, Status::Ok, "{}: {:?}", e.id, e.detail);
        assert_eq!(e.rows, Some(rows_per_entry), "{}: its recorded rows", e.id);
    }
    assert_eq!(report.sf, Some(ScaleFactor::Sf0_1));
    assert_eq!(report.seed_policy, None);
}

/// The pathdb SF1 subset: the 26 queries with recorded SF1 seeds, at the
/// manifest's 20 s timeout. Local only (ADR-0243 data cost).
#[test]
fn the_pathdb_workload_meets_its_ratchet_at_sf1() {
    if !enabled(
        "RELATE_QUERY_LATENCY_SF1",
        "the local-only SF1 pathdb corpus",
    ) {
        return;
    }
    let report = run_corpus(&corpus_config(Workload::Pathdb, ScaleFactor::Sf1)).unwrap();
    let run = report.entries.iter().filter(|e| e.rows.is_some()).count();
    assert_eq!(run, 26, "the spike's SF1 subset executes");
    assert_ratchet(&report);
}

/// A `rows` oracle parses into the same shape the graph-benchmark-ldbc lane compares
/// with (the f06 fixture check), and a malformed check fails loudly.
#[test]
fn fraud_oracle_checks_are_typed() {
    let oracle = fraud_oracle().unwrap();
    assert!(matches!(oracle["f06-sample-ring"], FraudCheck::Rows(_)));
    assert!(matches!(
        oracle["f01-monopartite-cycle"],
        FraudCheck::PlantedRing
    ));
    assert!(matches!(
        oracle["f04-bipartite-unbounded"],
        FraudCheck::None
    ));
}
