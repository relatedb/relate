//! The corpus run itself (ADR-0479): build the lane once, route every
//! entry onto its full disposition, execute it under the watchdog, and
//! assemble the report.
//!
//! The lane reframe in one breath: [`build_lane`] matches the workload
//! ONCE, each arm a one-line delegation into its lane module
//! (`pathdb::PathdbLane`, `fraud::FraudGraphs`, `snbbi::SnbBiLane`,
//! `ukrailway::UkrailwayLane`, the shared
//! `oracle_store::RowsOracleStoreLane` the rows-oracle workloads build), which owns
//! its own construction and routing policy — after which the run holds
//! ONE [`CorpusLane`] and the entry loop is flat:
//! [`CorpusLane::route`] resolves the entry's full disposition (one
//! call — including the parameters it runs with), then one three-arm
//! `match` runs, plans or pins it. No nested workload matches, no lane
//! invariants carried by `expect`. The shared execution types
//! ([`Target`], [`Routed`], the graph handles) and the lane interface
//! itself live in the corpus tree's `graph` module; this module imports
//! the lanes, never the reverse.

use std::collections::BTreeMap;

use relate::Connection;

use relate_test_support::sampling::process_peak_rss_bytes;

use super::fraud;
use super::graph::{CorpusLane, Routed, Target};
use super::oracle_store;
use super::pathdb;
use super::report;
use super::snbbi;
use super::ukrailway;
use super::{CorpusConfig, CorpusQuery, Lane};
use crate::report::{build_kind, git_state, started_unix};
use crate::run::{execute_with_params, Stats, Status, Watchdog};
use crate::Result;

/// Build the lane the run executes — the run's ONE workload match, each
/// arm a one-line delegation into the lane's own module (a lane's
/// construction reads its own knobs, so the match is over the config's
/// lane, never over built lane state — everything after construction is
/// the one [`CorpusLane`] interface). A lane whose setup fails cannot
/// run at all: the error fails the run.
fn build_lane(cfg: &CorpusConfig, queries: &[CorpusQuery]) -> Result<Box<dyn CorpusLane>> {
    Ok(match &cfg.lane {
        Lane::Pathdb { graph, seed_policy } => {
            Box::new(pathdb::PathdbLane::new(graph, *seed_policy, queries)?)
        }
        Lane::Fraud { fraud } => Box::new(fraud::FraudGraphs::new(fraud.clone(), queries)?),
        Lane::SnbBi { graph } => Box::new(snbbi::SnbBiLane::new(graph, queries)?),
        Lane::Ukrailway {
            data_root,
            download,
        } => Box::new(ukrailway::UkrailwayLane::new(
            data_root, *download, queries,
        )?),
        // The two rows-oracle workloads share the one lane: its own
        // workload (derived, never re-stated per arm) drives the oracle
        // path; the workload modules own their facts and tests only.
        Lane::AggStress { graph } | Lane::EquiJoin { graph } => Box::new(
            oracle_store::RowsOracleStoreLane::new(cfg.lane.workload(), graph, queries)?,
        ),
    })
}

/// Run one corpus workload end to end. Entry-level failures are recorded
/// in the report, not returned as errors; an `Err` means the run itself
/// could not happen (data, load, store, malformed workload).
pub fn run_corpus(cfg: &CorpusConfig) -> Result<report::CorpusReport> {
    let queries = super::workload_queries(cfg.lane.workload())?;
    let started_unix = started_unix();
    let watchdog = Watchdog::new();
    let mut lane = build_lane(cfg, &queries)?;

    let mut entries = Vec::with_capacity(queries.len());
    for q in &queries {
        let entry = match lane.route(q)? {
            Routed::Run(target) => run_entry(target, q, cfg, &watchdog),
            Routed::Plan(conn) => plan_entry(conn, q),
            Routed::Pin(conn, error) => pin_entry(conn, q, cfg, &watchdog, error),
        };
        eprintln!(
            "query-latency: {:<22} {:<10} {:<8}",
            q.id,
            q.classification.as_str(),
            entry.status
        );
        entries.push(entry);
    }

    let (sf, seed_policy) = cfg.lane.report_facts();

    Ok(report::CorpusReport {
        schema: report::CORPUS_SCHEMA,
        suite: "query-latency",
        workload: cfg.lane.workload().as_str().into(),
        sf,
        seed_policy,
        started_unix,
        engine_version: env!("CARGO_PKG_VERSION"),
        build: build_kind(),
        git: git_state(),
        config: report::CorpusConfigReport {
            timeout_s: cfg.timeout.as_secs_f64(),
            repeats: cfg.repeats,
        },
        graph: lane.graph_line(),
        fraud: lane.fraud_report(),
        seed_diffs: lane.seed_diffs().to_vec(),
        peak_rss_bytes: process_peak_rss_bytes(),
        entries,
    })
}

/// Execute an `ok` entry once (plus timed repeats of cheap queries) and
/// answer-check it where the lane carries a check. The parameters it
/// runs with arrive on the target — the lane resolved them at route.
fn run_entry(
    target: Target<'_>,
    q: &CorpusQuery,
    cfg: &CorpusConfig,
    watchdog: &Watchdog,
) -> report::CorpusEntry {
    let Target {
        conn,
        params,
        check,
    } = target;
    let (first, wall) = match execute_with_params(conn, &q.text, &params, cfg.timeout, watchdog) {
        Ok((r, w)) => (r, w),
        Err((status, msg)) => {
            // A timeout is bounded by the timeout itself; any other
            // failure measured nothing.
            let first_ms = (status == Status::Timeout).then_some(cfg.timeout.as_secs_f64() * 1e3);
            return report::CorpusEntry::of(q, status, None, first_ms, Some(msg));
        }
    };
    let rows = first.rows.len();
    let first_ms = wall.as_secs_f64() * 1e3;
    // The lane's check, applied uniformly: the difference when the
    // answer does not match, `None` when it does (or when the lane
    // carries no check).
    if let Some(diff) = check.as_deref().and_then(|check| check(&first)) {
        return report::CorpusEntry::of(q, Status::Wrong, Some(rows), Some(first_ms), Some(diff));
    }
    drop(first);
    // Cheap successful queries repeat, for a min/median sample; slow
    // ones ran once (the spike driver's rule, `REPEAT_LIMIT`).
    if wall > super::REPEAT_LIMIT || cfg.repeats == 0 {
        return report::CorpusEntry::of(q, Status::Ok, Some(rows), Some(first_ms), None);
    }
    let mut samples = vec![wall];
    let mut failed_repeats = 0;
    for _ in 0..cfg.repeats {
        match execute_with_params(conn, &q.text, &params, cfg.timeout, watchdog) {
            Ok((_, w)) => samples.push(w),
            // A repeat that errors or times out is counted, never
            // silently dropped: the median is over the samples that ran,
            // and the report says how many did not.
            Err(_) => failed_repeats += 1,
        }
    }
    let stats = Stats::of(&samples);
    let mut entry = report::CorpusEntry::of(q, Status::Ok, Some(rows), Some(first_ms), None);
    entry.min_ms = Some(stats.min_ns as f64 / 1e6);
    entry.med_ms = Some(stats.p50_ns as f64 / 1e6);
    entry.failed_repeats = failed_repeats;
    entry
}

/// A `kept` entry: parse and plan only — it must EXPLAIN, never execute.
/// A plan failure is the entry's failure, not the run's.
fn plan_entry(conn: &mut Connection, q: &CorpusQuery) -> report::CorpusEntry {
    match conn.explain_plan(&q.text) {
        Ok(plan) => report::CorpusEntry::of(
            q,
            Status::Ok,
            None,
            None,
            Some(format!("planned ({})", plan.operator)),
        ),
        Err(e) => report::CorpusEntry::of(
            q,
            Status::Error,
            None,
            None,
            Some(format!("EXPLAIN of a kept entry failed: {e}")),
        ),
    }
}

/// An `error-pin` entry: the query must fail, naming its authored error
/// — the one pin-match spelling, [`super::pin_met`]. The authored error
/// fires before any parameter is bound, so a pin executes with none.
fn pin_entry(
    conn: &mut Connection,
    q: &CorpusQuery,
    cfg: &CorpusConfig,
    watchdog: &Watchdog,
    error: &str,
) -> report::CorpusEntry {
    let prefix = format!("{error}:");
    let (status, detail) =
        match execute_with_params(conn, &q.text, &BTreeMap::new(), cfg.timeout, watchdog) {
            Ok(_) => (
                Status::Ok,
                Some(format!(
                    "the query succeeded; the pin expects the authored {prefix} failure"
                )),
            ),
            // The authored failure itself: met, no note.
            Err((Status::Error, msg)) if super::pin_met(error, &msg) => (Status::Error, None),
            // Any other failure misses the pin — an error that is not the
            // authored one as much as a success: the authored name is the
            // entry's oracle, and the ratchet counts the miss.
            Err((Status::Error, msg)) => (
                Status::Wrong,
                Some(format!(
                    "expected the authored {prefix} failure, got: {msg}"
                )),
            ),
            Err((status, msg)) => (
                status,
                Some(format!(
                    "expected the authored {prefix} failure, got: {msg}"
                )),
            ),
        };
    report::CorpusEntry::of(q, status, None, None, detail)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{ok_query, pin_query};
    use super::super::fraud::{answer_diff, FraudCheck};
    use super::*;
    use crate::answers::Expected;
    use relate::Database;
    use std::time::Duration;

    fn config(repeats: usize) -> CorpusConfig {
        CorpusConfig {
            lane: Lane::Fraud {
                fraud: relate_datasets::fraud::FraudConfig::ci(),
            },
            timeout: Duration::from_secs(10),
            repeats,
        }
    }

    /// The open-connect-seed fixture the entry tests share: an
    /// in-memory store holding ten `:N` nodes.
    fn seeded_conn() -> (Database, Connection) {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("UNWIND range(1, 10) AS i CREATE (:N {i: i})")
            .unwrap();
        (db, conn)
    }

    /// An unchecked `ok` target over `conn`: no parameters, no check.
    fn ok_target(conn: &mut Connection) -> Target<'_> {
        Target {
            conn,
            params: BTreeMap::new(),
            check: None,
        }
    }

    /// A cheap `ok` entry repeats and reports its min and median over
    /// the successful samples, with no failed repeat.
    #[test]
    fn cheap_ok_entries_repeat_and_report_min_and_median() {
        let (_db, mut conn) = seeded_conn();
        let q = ok_query("t1", "MATCH (n:N) RETURN count(n) AS c");
        let entry = run_entry(ok_target(&mut conn), &q, &config(2), &Watchdog::new());
        assert_eq!(entry.status, Status::Ok, "{:?}", entry.detail);
        assert_eq!(entry.rows, Some(1));
        assert!(entry.first_ms.is_some(), "the execution was measured");
        assert!(entry.min_ms.is_some(), "the repeats were measured");
        assert!(entry.med_ms.is_some(), "the repeats were measured");
        assert_eq!(entry.failed_repeats, 0);
        assert!(entry.min_ms.unwrap() <= entry.med_ms.unwrap());
    }

    /// A run configured with no repeats measures once: no min/median,
    /// and the first execution's time is the entry's — the same
    /// single-shot shape a query past the repeat threshold takes.
    #[test]
    fn runs_without_repeats_measure_once() {
        let (_db, mut conn) = seeded_conn();
        let q = ok_query("t2", "MATCH (n:N) RETURN count(n) AS c");
        let entry = run_entry(ok_target(&mut conn), &q, &config(0), &Watchdog::new());
        assert_eq!(entry.status, Status::Ok);
        assert_eq!(entry.rows, Some(1));
        assert_eq!(entry.min_ms, None);
        assert_eq!(entry.med_ms, None);
        assert_eq!(entry.failed_repeats, 0);
    }

    /// A `rows` check that misses is a `wrong` entry carrying the
    /// difference; one that hits leaves the entry ok.
    #[test]
    fn answer_checks_mark_wrong_entries_with_their_difference() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("UNWIND range(1, 3) AS i CREATE (:N {i: i})")
            .unwrap();
        let q = ok_query("t3", "MATCH (n:N) RETURN count(n) AS c");
        let expected = Expected {
            rows: vec![serde_json::Map::from_iter([(
                "c".into(),
                serde_json::json!(2),
            )])],
            order_sensitive: true,
        };
        let check = FraudCheck::Rows(expected);
        let target = Target {
            conn: &mut conn,
            params: BTreeMap::new(),
            check: Some(Box::new(move |r| answer_diff(&check, &[], r))),
        };
        let entry = run_entry(target, &q, &config(0), &Watchdog::new());
        assert_eq!(entry.status, Status::Wrong);
        assert!(entry.detail.unwrap().contains("expected"), "the diff");
        // The same check over the right answer stays ok.
        let check = FraudCheck::Rows(Expected {
            rows: vec![serde_json::Map::from_iter([(
                "c".into(),
                serde_json::json!(3),
            )])],
            order_sensitive: true,
        });
        let result = conn.query(&q.text).unwrap();
        assert_eq!(answer_diff(&check, &[], &result), None);
    }

    /// A pin that succeeds is the entry's failure; a pin failing with
    /// its authored error meets its expectation; a pin failing with any
    /// OTHER error misses it — the authored name is the entry's oracle.
    #[test]
    fn pins_fail_when_the_query_succeeds_or_fails_wrongly() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let q = pin_query("t4", "RETURN 1");
        let entry = pin_entry(
            &mut conn,
            &q,
            &config(0),
            &Watchdog::new(),
            "UnsafeRepeatableElements",
        );
        assert_eq!(entry.status, Status::Ok, "the query succeeded");
        assert!(!entry.expectation_met());
        let entry = pin_entry(
            &mut conn,
            &pin_query(
                "t5",
                "MATCH REPEATABLE ELEMENTS p = (a)-[:R*1..]-(a) RETURN p",
            ),
            &config(0),
            &Watchdog::new(),
            "UnsafeRepeatableElements",
        );
        assert_eq!(entry.status, Status::Error, "{:?}", entry.detail);
        assert_eq!(entry.detail, None, "the authored error needs no note");
        assert_eq!(entry.first_ms, None, "a pin that failed measured nothing");
        assert!(entry.expectation_met());
        // An error that is not the authored one is a missed pin, never
        // a met one: the ratchet counts it.
        let entry = pin_entry(
            &mut conn,
            &pin_query("t6", "RETURN count(p)"),
            &config(0),
            &Watchdog::new(),
            "UnsafeRepeatableElements",
        );
        assert_eq!(entry.status, Status::Wrong, "{:?}", entry.detail);
        assert!(!entry.expectation_met(), "the wrong error misses the pin");
    }
}
