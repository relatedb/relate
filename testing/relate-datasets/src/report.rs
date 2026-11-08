//! The dataset harnesses' report vocabulary and file writer: the latency
//! stats the timing reports publish, the rank policy their percentiles index
//! with, the one place a report becomes pretty JSON on disk or as a body, and
//! the unit conversions and stdout contract the tables and progress lines
//! print through.

use std::fs;
use std::path::Path;

use relate_test_support::nearest_rank;
use serde::{Deserialize, Serialize};

/// One nanosecond count as milliseconds — the conversion a report's latency
/// table and its progress lines print through, so one run's p50 cannot print
/// at two scales.
pub fn nanos_to_millis(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

/// Bytes as MiB (1024×1024), so a 32 MiB logical target prints as 32.0 — the
/// conversion the datasets reports print their MiB figures through.
pub fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Which rank policy a sample set's percentiles are indexed with.
///
/// The two policies differ on an even-sized sample set: [`Rank::Canonical`]
/// takes the `ceil(p * n)`-th value, [`Rank::FrozenEnvelope`] the
/// `round((n - 1) * p)`-th.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rank {
    /// The workspace's canonical nearest-rank rule
    /// ([`relate_test_support::percentile`], i.e. [`nearest_rank`]): the
    /// policy the query-latency suite, the graph-examples differential and the
    /// agent A/B harness share.
    Canonical,
    /// The pre-canonical index the fraud and identity envelopes under
    /// `baselines/` were published against. Kept as its own policy because
    /// those envelopes are frozen (ADR-0046): moving them to
    /// [`Rank::Canonical`] moves every published p50/p95/p99, so the move
    /// needs a re-freeze, not a refactor.
    FrozenEnvelope,
}

/// The latency stats every dataset report publishes, in nanoseconds.
///
/// Promoted out of `loadtest`, where the fraud/identity harness defined it,
/// so every report-carrying harness shares the one type instead of each
/// carrying its own. The `mean` key keeps the spelling
/// `scripts/freeze-baseline-envelope.py` merges: its generic path widens
/// whatever numeric stats a usecase carries, so a second key for the mean
/// would fork the vocabulary the frozen envelopes are compared against.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LatencyNs {
    pub min: u64,
    pub max: u64,
    pub mean: f64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
}

impl LatencyNs {
    /// The stats over `samples`, or `None` when there is no sample to
    /// summarize; the samples are sorted once and the percentiles follow
    /// `rank` (see [`Rank`]) over that order.
    pub fn from_samples(samples: &[u64], rank: Rank) -> Option<LatencyNs> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let sum: u128 = sorted.iter().map(|sample| *sample as u128).sum();
        let at = |p: f64| match rank {
            Rank::Canonical => sorted[nearest_rank(sorted.len(), p)],
            Rank::FrozenEnvelope => sorted[frozen_envelope_rank(sorted.len(), p)],
        };
        Some(LatencyNs {
            min: sorted[0],
            max: sorted[sorted.len() - 1],
            mean: sum as f64 / sorted.len() as f64,
            p50: at(0.50),
            p95: at(0.95),
            p99: at(0.99),
        })
    }
}

/// The frozen envelopes' index, like [`nearest_rank`] an index into `len`
/// sorted samples: `round((len - 1) * p)`, clamped to the last one. See
/// [`Rank::FrozenEnvelope`].
fn frozen_envelope_rank(len: usize, p: f64) -> usize {
    let idx = ((len as f64 - 1.0) * p).round() as usize;
    idx.min(len - 1)
}

/// The pretty-JSON body of `report`, plus the trailing newline every dataset
/// report carries.
pub fn report_json<T: Serialize>(report: &T) -> Result<String, String> {
    let body = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
    Ok(body + "\n")
}

/// Write a report `body` from [`report_json`] to `path`, creating the parent
/// directory.
pub fn write_body(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }
    fs::write(path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Write `report` as pretty JSON (plus a trailing newline) to `path`, creating
/// the parent directory.
pub fn write_report<T: Serialize>(path: &Path, report: &T) -> Result<(), String> {
    write_body(path, &report_json(report)?)
}

/// Print `json` — the pretty body [`report_json`] produced — to stdout, and
/// persist it to `out` when the run asked for a file, printing the one
/// `wrote <path>` line a persisted run adds. The one stdout/file contract the
/// dataset binaries' reports share.
pub fn print_report(json: &str, out: Option<&Path>) -> Result<(), String> {
    print!("{json}");
    if let Some(out) = out {
        write_body(out, json)?;
        println!("wrote {}", out.display());
    }
    Ok(())
}

/// The invoking command line, recorded as a report's `repro_command` so a
/// frozen report can be reproduced from its own JSON.
pub fn repro_command() -> String {
    std::env::args().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nearest rank and the frozen envelopes' index disagree on an
    /// even-sized sample set — the canonical rule takes the 2nd value of
    /// four, the frozen one the 3rd — and each policy publishes exactly its
    /// own choice; the mean is exact and the ends are min/max either way.
    #[test]
    fn from_samples_indexes_through_the_rank_it_is_given() {
        let samples = [40, 10, 30, 20];

        let canonical = LatencyNs::from_samples(&samples, Rank::Canonical).unwrap();
        assert_eq!(
            (
                canonical.min,
                canonical.max,
                canonical.mean,
                canonical.p50,
                canonical.p95,
                canonical.p99
            ),
            (10, 40, 25.0, 20, 40, 40)
        );

        let frozen = LatencyNs::from_samples(&samples, Rank::FrozenEnvelope).unwrap();
        assert_eq!(
            (
                frozen.min,
                frozen.max,
                frozen.mean,
                frozen.p50,
                frozen.p95,
                frozen.p99
            ),
            (10, 40, 25.0, 30, 40, 40)
        );

        assert!(LatencyNs::from_samples(&[], Rank::Canonical).is_none());
        assert!(LatencyNs::from_samples(&[], Rank::FrozenEnvelope).is_none());
    }

    /// A one-sample set has the same stats under either policy, at every
    /// percentile.
    #[test]
    fn from_samples_keeps_a_single_sample_at_every_percentile() {
        for rank in [Rank::Canonical, Rank::FrozenEnvelope] {
            let stats = LatencyNs::from_samples(&[7], rank).unwrap();
            assert_eq!(
                (stats.min, stats.max, stats.mean, stats.p50, stats.p95, stats.p99),
                (7, 7, 7.0, 7, 7, 7)
            );
        }
    }

    /// The report lands as pretty JSON plus one trailing newline, and a
    /// missing parent directory is created.
    #[test]
    fn write_report_creates_the_directory_and_writes_pretty_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("report.json");
        let report = serde_json::json!({"schema_version": 1, "name": "x"});

        write_report(&path, &report).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            "{\n  \"name\": \"x\",\n  \"schema_version\": 1\n}\n"
        );
    }

    /// The unit conversions are the one spelling a table and a progress line
    /// print through: 1_500_000 ns is 1.5 ms, and 32 MiB is 32.0.
    #[test]
    fn the_unit_conversions_publish_the_report_scale() {
        assert_eq!(nanos_to_millis(1_500_000), 1.5);
        assert_eq!(mib(32 * 1024 * 1024), 32.0);
    }

    /// The persist half of the print contract: a run given `--out` writes the
    /// same body it printed, and a run without one writes no file.
    #[test]
    fn print_report_persists_the_body_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        print_report("{}\n", None).unwrap();
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            0,
            "a run without --out writes no file"
        );
        let path = dir.path().join("nested").join("report.json");
        print_report("{\"a\":1}\n", Some(&path)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
    }
}
