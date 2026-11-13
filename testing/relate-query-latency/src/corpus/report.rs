//! The corpus run's report types (ADR-0479): one entry's outcome and
//! its expectation, and the whole run's JSON report and console table —
//! the shapes `report-corpus-<workload>[-sf<SF>].json` writes — plus
//! the report-side shared vocabulary the lanes consume: the graph-benchmark-ldbc store
//! origin's one-line provenance ([`graph_benchmark_ldbc_store_line`]), the one spelling
//! of that fact the graph-benchmark-ldbc-store lanes (pathdb, snb-bi,
//! agg-stress, equi-join)
//! share.

use std::path::Path;

use serde::Serialize;

use super::{Classification, CorpusQuery, SeedPolicy};
use crate::report::{write_pretty_json, Git, StoreOrigin};
use crate::run::Status;
use crate::workload::ScaleFactor;
use crate::Result;

/// The report's one-line graph provenance for a graph-benchmark-ldbc
/// store origin — the one spelling of the fact, shared by the lanes that
/// run over the graph-benchmark-ldbc store (pathdb, snb-bi, agg-stress,
/// equi-join).
pub(super) fn graph_benchmark_ldbc_store_line(origin: StoreOrigin) -> String {
    match origin {
        StoreOrigin::Loaded { store, .. } => {
            format!("loaded the graph-benchmark-ldbc data set into {store}")
        }
        StoreOrigin::Reused { store, .. } => {
            format!("reused the graph-benchmark-ldbc store at {store}")
        }
    }
}

/// Bumped when a report field changes meaning or disappears. `2`:
/// `sf` and `seed_policy` became lane-scoped (a fraud report records
/// neither).
pub const CORPUS_SCHEMA: u32 = 2;

/// A seed the policy re-resolved differently than the params file
/// records (an expected outcome of a re-frozen data set): both sides,
/// id and degree, so a report shows what changed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SeedDiff {
    pub seed_key: String,
    pub recorded: i64,
    pub recorded_degree: u64,
    pub resolved: i64,
    pub resolved_degree: u64,
}

/// One entry's outcome.
#[derive(Debug, Clone, Serialize)]
pub struct CorpusEntry {
    pub id: String,
    pub classification: Classification,
    pub query_sha256: String,
    pub status: Status,
    /// Rows of the checked execution; `None` for `kept` entries, which
    /// are planned, never executed, and for `error-pin` entries, which
    /// fail on purpose.
    pub rows: Option<usize>,
    /// The first execution's wall time — `None` for every entry that
    /// never completed one (planned, pinned, or failed before
    /// measuring): 'not executed' has the one encoding across the
    /// entry. The one exception a `timeout` entry carries: the timeout
    /// itself as its bound (`timeout * 1000`), not a measurement — the
    /// entry did not complete, and the report still names the bound it
    /// was cancelled against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub med_ms: Option<f64>,
    /// Timed repeats that errored or timed out, so a median over a
    /// degraded sample is visible in the report — never silently
    /// smaller than the configured repeats.
    pub failed_repeats: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CorpusEntry {
    /// The base of every recorded entry: the identity fields from the
    /// query, the outcome, and the single-execution defaults (no repeat
    /// statistics). Callers set `min_ms`/`med_ms`/`failed_repeats` only
    /// when repeats ran.
    pub(super) fn of(
        q: &CorpusQuery,
        status: Status,
        rows: Option<usize>,
        first_ms: Option<f64>,
        detail: Option<String>,
    ) -> Self {
        Self {
            id: q.id.clone(),
            classification: q.classification.clone(),
            query_sha256: q.sha256.clone(),
            status,
            rows,
            first_ms,
            min_ms: None,
            med_ms: None,
            failed_repeats: 0,
            detail,
        }
    }

    /// Whether the entry met its classification's expectation: the
    /// ratchet for `ok`, a successful plan for `kept`, the authored
    /// error for `error-pin`.
    pub fn expectation_met(&self) -> bool {
        match self.classification {
            Classification::Ok | Classification::Kept => self.status == Status::Ok,
            Classification::ErrorPin { .. } => self.status == Status::Error,
        }
    }
}

/// A run's report: `report-corpus-<workload>-sf<SF>.json` and the
/// console table.
#[derive(Debug, Serialize)]
pub struct CorpusReport {
    pub schema: u32,
    pub suite: &'static str,
    pub workload: String,
    /// The scale factor the lane ran at, when its lane carries one
    /// (which lanes: `Lane::report_facts`, the one home of that fact).
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_word"
    )]
    pub sf: Option<ScaleFactor>,
    /// The seed policy the lane ran under, when its lane runs one (which
    /// lanes: `Lane::report_facts`, the one home of that fact).
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_word"
    )]
    pub seed_policy: Option<SeedPolicy>,
    pub started_unix: u64,
    pub engine_version: &'static str,
    pub build: &'static str,
    pub git: Git,
    pub config: CorpusConfigReport,
    /// Where the graph came from, in one line (the graph-benchmark-ldbc store origin, or
    /// the fraud generator / fixture).
    pub graph: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fraud: Option<FraudReport>,
    pub seed_diffs: Vec<SeedDiff>,
    pub peak_rss_bytes: Option<u64>,
    pub entries: Vec<CorpusEntry>,
}

#[derive(Debug, Serialize)]
pub struct CorpusConfigReport {
    pub timeout_s: f64,
    pub repeats: usize,
}

/// The fraud lane's generator knobs, when it ran.
#[derive(Debug, Serialize)]
pub struct FraudReport {
    pub scale: f64,
    pub seed: u64,
    pub fanout_k: usize,
    pub cycle_len: usize,
}

/// The one-word spelling a typed report fact serializes as — the shape
/// `ScaleFactor` and `SeedPolicy` share, each already carrying its
/// `as_str` (the report's lowercase spellings, byte-identical to the
/// format's committed reports).
trait AsWord {
    fn as_word(&self) -> &'static str;
}

impl AsWord for ScaleFactor {
    fn as_word(&self) -> &'static str {
        self.as_str()
    }
}

impl AsWord for SeedPolicy {
    fn as_word(&self) -> &'static str {
        self.as_str()
    }
}

/// Serialize a typed fact as its one word. `None` never reaches here —
/// the field's `skip_serializing_if` keeps a fact that did not run out
/// of the report entirely — but a `None` that did would serialize as
/// null, never a fabricated word.
fn serialize_word<S: serde::Serializer, T: AsWord>(
    fact: &Option<T>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match fact {
        Some(fact) => s.serialize_str(fact.as_word()),
        None => s.serialize_none(),
    }
}

impl CorpusReport {
    /// Entries whose classification was not met — the run's failures.
    pub fn failures(&self) -> Vec<&CorpusEntry> {
        self.entries
            .iter()
            .filter(|e| !e.expectation_met())
            .collect()
    }

    pub fn write_json(&self, path: &Path) -> Result<()> {
        write_pretty_json(self, path)
    }

    /// The console table: one line per entry.
    pub fn table(&self) -> String {
        let mut s = format!(
            "{:<22} {:<10} {:<8} {:>6} {:>10} {:>10}\n",
            "entry", "class", "status", "rows", "first ms", "median ms"
        );
        let opt = |ms: &Option<f64>| match ms {
            Some(v) => format!("{v:.1}"),
            None => "-".into(),
        };
        for e in &self.entries {
            s += &format!(
                "{:<22} {:<10} {:<8} {:>6} {:>10} {:>10}\n",
                e.id,
                e.classification.as_str(),
                e.status,
                e.rows.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
                opt(&e.first_ms),
                opt(&e.med_ms),
            );
        }
        for e in &self.entries {
            if e.failed_repeats > 0 {
                s += &format!(
                    "{}: {} timed repeat(s) failed; the median is over the rest\n",
                    e.id, e.failed_repeats
                );
            }
        }
        for e in self.failures() {
            if let Some(d) = &e.detail {
                s += &format!("{}: {d}\n", e.id);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_constructor_carries_the_querys_identity() {
        let q = CorpusQuery {
            id: "r008".into(),
            classification: Classification::Kept,
            text: "RETURN 1".into(),
            sha256: "abc".into(),
        };
        let e = CorpusEntry::of(&q, Status::Ok, None, None, Some("planned (X)".into()));
        assert_eq!((e.id.as_str(), e.query_sha256.as_str()), ("r008", "abc"));
        assert_eq!(e.classification, Classification::Kept);
        assert_eq!((e.min_ms, e.med_ms, e.failed_repeats), (None, None, 0));
    }

    #[test]
    fn the_ratchet_judges_each_classification_by_its_own_expectation() {
        let entry = |c: Classification, status: Status| CorpusEntry {
            id: "x".into(),
            classification: c,
            query_sha256: String::new(),
            status,
            rows: Some(1),
            first_ms: Some(1.0),
            min_ms: None,
            med_ms: None,
            failed_repeats: 0,
            detail: None,
        };
        // ok: only ok passes — a new timeout is the ratchet failure.
        assert!(entry(Classification::Ok, Status::Ok).expectation_met());
        assert!(!entry(Classification::Ok, Status::Timeout).expectation_met());
        assert!(!entry(Classification::Ok, Status::Error).expectation_met());
        assert!(!entry(Classification::Ok, Status::Wrong).expectation_met());
        // kept: a successful plan is ok; anything else fails.
        assert!(entry(Classification::Kept, Status::Ok).expectation_met());
        assert!(!entry(Classification::Kept, Status::Error).expectation_met());
        // error-pin: the error is the expectation.
        let pin = Classification::ErrorPin {
            error: "UnsafeRepeatableElements".into(),
        };
        assert!(entry(pin.clone(), Status::Error).expectation_met());
        assert!(!entry(pin, Status::Ok).expectation_met());
    }

    #[test]
    fn a_report_tables_its_entries_and_failures() {
        // A fraud-shaped report: no scale factor and no seed policy —
        // neither fact ran, neither is recorded.
        let report = CorpusReport {
            schema: CORPUS_SCHEMA,
            suite: "query-latency",
            workload: "fraud-qpp".into(),
            sf: None,
            seed_policy: None,
            started_unix: 0,
            engine_version: "0.0.0",
            build: "release",
            git: Git {
                commit: None,
                dirty: None,
            },
            config: CorpusConfigReport {
                timeout_s: 10.0,
                repeats: 2,
            },
            graph: "fixture".into(),
            fraud: None,
            seed_diffs: vec![],
            peak_rss_bytes: None,
            entries: vec![
                CorpusEntry {
                    id: "f06-sample-ring".into(),
                    classification: Classification::Ok,
                    query_sha256: "x".into(),
                    status: Status::Ok,
                    rows: Some(1),
                    first_ms: Some(0.4),
                    min_ms: Some(0.3),
                    med_ms: Some(0.4),
                    failed_repeats: 0,
                    detail: None,
                },
                CorpusEntry {
                    id: "f04-bipartite-unbounded".into(),
                    classification: Classification::Kept,
                    query_sha256: "y".into(),
                    status: Status::Ok,
                    rows: None,
                    first_ms: None,
                    min_ms: None,
                    med_ms: None,
                    failed_repeats: 0,
                    detail: Some("planned (Argument)".into()),
                },
                CorpusEntry {
                    id: "f01-monopartite-cycle".into(),
                    classification: Classification::Ok,
                    query_sha256: "z".into(),
                    status: Status::Timeout,
                    rows: None,
                    first_ms: Some(10_000.0),
                    min_ms: None,
                    med_ms: None,
                    failed_repeats: 0,
                    detail: Some("cancelled after 10.0 s".into()),
                },
                CorpusEntry {
                    id: "f02-simple-cycle".into(),
                    classification: Classification::Ok,
                    query_sha256: "w".into(),
                    status: Status::Ok,
                    rows: Some(8),
                    first_ms: Some(0.9),
                    min_ms: Some(0.8),
                    med_ms: Some(0.9),
                    failed_repeats: 2,
                    detail: None,
                },
            ],
        };
        let table = report.table();
        assert!(table.contains("f06-sample-ring"), "{table}");
        assert!(table.contains("kept"), "{table}");
        assert!(
            table.contains("f01-monopartite-cycle: cancelled after"),
            "{table}"
        );
        assert!(
            table.contains("f02-simple-cycle: 2 timed repeat(s) failed"),
            "a degraded median is visible in the table"
        );
        assert_eq!(report.failures().len(), 1, "the ratchet failure");
        let json = serde_json::to_value(&report).unwrap();
        assert!(
            json.get("sf").is_none() && json.get("seed_policy").is_none(),
            "a fraud report records neither a scale factor nor a seed policy"
        );
        assert_eq!(json["entries"][0]["classification"], "ok");
        assert_eq!(json["entries"][1]["classification"], "kept");
        assert_eq!(json["entries"][1]["rows"], serde_json::Value::Null);
        assert!(
            json["entries"][1].get("first_ms").is_none(),
            "a planned-never-executed entry serializes no first_ms"
        );
        assert_eq!(json["entries"][3]["failed_repeats"], 2);
    }

    /// The typed lane-scoped facts serialize as their one word — the
    /// lowercase spelling the report format has always carried, so the
    /// typed fields changed no report byte.
    #[test]
    fn typed_lane_facts_serialize_as_their_one_word() {
        let report = CorpusReport {
            schema: CORPUS_SCHEMA,
            suite: "query-latency",
            workload: "pathdb".into(),
            sf: Some(ScaleFactor::Sf0_1),
            seed_policy: Some(SeedPolicy::Median),
            started_unix: 0,
            engine_version: "0.0.0",
            build: "release",
            git: Git {
                commit: None,
                dirty: None,
            },
            config: CorpusConfigReport {
                timeout_s: 10.0,
                repeats: 0,
            },
            graph: "fixture".into(),
            fraud: None,
            seed_diffs: vec![],
            peak_rss_bytes: None,
            entries: vec![],
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["sf"], "0.1");
        assert_eq!(json["seed_policy"], "median");
    }
}
