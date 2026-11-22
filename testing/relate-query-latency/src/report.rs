//! The run report: `target/query-latency/report-graph-benchmark-ldbc-sf<sf>.json`, and the
//! console table. Also the run-metadata helpers every report shares
//! (the started timestamp, the build kind, the pretty-JSON writer).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::load::LoadReport;
use crate::run::{QueryOutcome, Status};
use crate::Result;

/// Bumped when a field changes meaning or disappears.
pub const SCHEMA: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub suite: &'static str,
    pub workload: &'static str,
    pub sf: String,
    pub index_profile: String,
    /// Seconds since the Unix epoch when the run started.
    pub started_unix: u64,
    pub engine_version: &'static str,
    /// `release` unless the binary was built with debug assertions.
    pub build: &'static str,
    pub git: Git,
    pub config: ConfigReport,
    /// Where the graph came from: loaded by this run, or a reused store.
    pub graph: StoreOrigin,
    /// The process's peak RSS, or `null` when no sample was available
    /// (unsupported platform, or the platform query failed).
    pub peak_rss_bytes: Option<u64>,
    pub queries: Vec<QueryOutcome>,
}

/// How a run got its graph. Serialized with an `origin` tag:
/// `{"origin": "loaded", "store": ..., "load": {...}}` or
/// `{"origin": "reused", "store": ..., "open_seconds": ...}`.
#[derive(Debug, Serialize)]
#[serde(tag = "origin", rename_all = "lowercase")]
pub enum StoreOrigin {
    /// This run loaded the data set into `store` (`:memory:` or a new
    /// store path).
    Loaded { store: String, load: LoadReport },
    /// This run opened the existing store at `store` read-only.
    Reused { store: String, open_seconds: f64 },
}

impl StoreOrigin {
    /// The load, when this run built the graph.
    pub fn load(&self) -> Option<&LoadReport> {
        match self {
            Self::Loaded { load, .. } => Some(load),
            Self::Reused { .. } => None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Git {
    pub commit: Option<String>,
    pub dirty: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ConfigReport {
    pub min_rounds: usize,
    pub min_time_s: f64,
    pub timeout_s: f64,
    pub max_rounds: usize,
}

impl Report {
    /// Queries whose status is not `ok`.
    pub fn failures(&self) -> Vec<&QueryOutcome> {
        self.queries
            .iter()
            .filter(|q| q.status != Status::Ok)
            .collect()
    }

    pub fn write_json(&self, path: &Path) -> Result<()> {
        write_pretty_json(self, path)
    }

    /// The console table: one line per query, engine-elapsed p50 and mean.
    pub fn table(&self) -> String {
        let mut s = format!(
            "{:<5} {:<8} {:>6} {:>7} {:>12} {:>12} {:>12}\n",
            "query", "status", "rows", "rounds", "p50 ms", "mean ms", "p99 ms"
        );
        let ms = |ns: u64| format!("{:.3}", ns as f64 / 1e6);
        for q in &self.queries {
            s += &format!(
                "q{:<4} {:<8} {:>6} {:>7} {:>12} {:>12} {:>12}\n",
                q.qid,
                q.status,
                q.rows,
                q.rounds,
                ms(q.engine.p50_ns),
                ms(q.engine.mean_ns),
                ms(q.engine.p99_ns)
            );
        }
        for q in self.failures() {
            if let Some(d) = &q.detail {
                s += &format!("q{}: {d}\n", q.qid);
            }
        }
        s
    }
}

/// Seconds since the Unix epoch now — a report's `started_unix`. A
/// clock reading before the epoch is a broken-clock invariant, stated
/// here and reported loudly — never fabricated as zero.
pub fn started_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock is set after the Unix epoch")
        .as_secs()
}

/// `release` unless the binary was built with debug assertions.
pub(crate) fn build_kind() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// Write `value` as pretty JSON with a trailing newline, creating the
/// report's parent directories — the one writer every report delegates
/// to.
pub(crate) fn write_pretty_json<T: Serialize>(value: &T, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let json =
        serde_json::to_string_pretty(value).map_err(|e| format!("serializing the report: {e}"))?;
    std::fs::write(path, json + "\n").map_err(|e| format!("writing {}: {e}", path.display()))
}

/// The repository's HEAD commit and whether the work tree differs from it.
pub fn git_state() -> Git {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    Git {
        commit: run(&["rev-parse", "HEAD"]),
        dirty: run(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::Stats;

    fn report(status: Status) -> Report {
        Report {
            schema: SCHEMA,
            suite: "query-latency",
            workload: crate::workload::NAME,
            sf: "0.1".into(),
            index_profile: "upstream".into(),
            started_unix: 0,
            engine_version: "0.0.0",
            build: "release",
            git: Git {
                commit: None,
                dirty: None,
            },
            config: ConfigReport {
                min_rounds: 5,
                min_time_s: 1.0,
                timeout_s: 60.0,
                max_rounds: 10,
            },
            graph: StoreOrigin::Reused {
                store: "graph-benchmark-ldbc.rl".into(),
                open_seconds: 2.5,
            },
            peak_rss_bytes: None,
            queries: vec![QueryOutcome {
                qid: 7,
                query_sha256: "x".into(),
                status,
                rows: 1,
                rounds: 5,
                engine: Stats {
                    p50_ns: 1_500_000,
                    ..Stats::default()
                },
                wall: Stats::default(),
                detail: (status != Status::Ok).then(|| "a diff".into()),
            }],
        }
    }

    #[test]
    fn table_shows_milliseconds_and_failure_details() {
        let t = report(Status::Wrong).table();
        assert!(t.contains("q7"), "{t}");
        assert!(t.contains("wrong"), "{t}");
        assert!(t.contains("1.500"), "{t}");
        assert!(t.contains("q7: a diff"), "{t}");
        assert!(!report(Status::Ok).table().contains("a diff"));
    }

    #[test]
    fn json_report_round_trips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/report.json");
        report(Status::Ok).write_json(&p).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["queries"][0]["status"], "ok");
        assert_eq!(v["queries"][0]["engine"]["p50_ns"], 1_500_000);
        assert_eq!(
            v["graph"],
            serde_json::json!({
                "origin": "reused",
                "store": "graph-benchmark-ldbc.rl",
                "open_seconds": 2.5
            })
        );
        assert!(v["queries"][0].get("detail").is_none());
    }
}
