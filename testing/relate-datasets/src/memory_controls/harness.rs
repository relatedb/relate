//! The processing family's shared harness: the run's inputs and report, the
//! serve-child lifecycle every case drives, the retry connect, the case
//! finisher, and the one [`run`] that dispatches the family's probes.
//!
//! The cases themselves keep their own files — the processing-memory
//! invariants ([`crate::memory_controls::controls`]), the logical-byte calibration
//! ([`crate::memory_controls::calibration`]) and the configured workload
//! ([`crate::memory_controls::workload`]) — and import this harness rather than any
//! one case module, so no case module is another's home.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use relate_test_support::sampling::process_status;
use serde::Serialize;

use crate::serve_child::{free_ports, serve_command, ServeChild, READY_DEADLINE};
use relate_test_support::bolt::client::Session;

/// The cases `--case` names, in run order: the memory control verifications, the
/// logical-byte calibration, then the configured workload (which also runs by
/// default when no probe is named).
const CASE_NAMES: &[&str] = &[
    "connection-cap",
    "unlimited-connections",
    "concurrent-logical-workspace-limit",
    "workspace-memory-calibration",
    "workload",
];

/// Whether a case runs: the one named case, the workload by default, or every
/// other case unless `--skip-controls` asked for the workload alone.
fn selected(cfg: &Config, name: &str) -> bool {
    if let Some(case) = cfg.case.as_deref() {
        return case == name;
    }
    if name == "workload" {
        return true;
    }
    !cfg.skip_controls
}

/// A case's idle residue budget: RSS above the first idle sample that much
/// later in the same case is a leak, not measurement noise.
const IDLE_RESIDUE_BYTES: u64 = 8 * 1024 * 1024;

/// One processing run's inputs.
#[derive(Debug, Clone)]
pub struct Config {
    pub serve_bin: PathBuf,
    pub store: PathBuf,
    pub out: PathBuf,
    pub controls: crate::memory_controls::workload::Controls,
    /// When set, record only the configured workload.
    pub skip_controls: bool,
    /// Run one named case. `None` runs the control verifications, the
    /// calibration, and the workload.
    pub case: Option<String>,
    /// Repetitions for every workspace-memory calibration shape.
    pub calibration_repeats: u32,
    /// Milliseconds each calibration query remains paused at its live peak.
    pub calibration_hold_ms: u64,
    /// Optional workspace family filter for workspace-memory calibration.
    pub calibration_family: Option<String>,
}

/// The processing family's report: the invariant cases, the configured
/// workload's rounds and the calibration, in one document.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub name: String,
    pub started_at: String,
    pub finished_at: String,
    pub controls: crate::memory_controls::workload::Controls,
    pub cases: Vec<CaseReport>,
    pub workloads: Vec<crate::memory_controls::workload::WorkloadReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_memory_calibration:
        Option<crate::memory_controls::calibration::PeakCalibrationReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup_memory_peak_bytes: Option<u64>,
}

/// The Given / When / Then text stored on a case.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioText {
    pub given: String,
    pub when: String,
    pub then: String,
    pub outcome: String,
}

impl ScenarioText {
    pub fn empty() -> Self {
        Self {
            given: String::new(),
            when: String::new(),
            then: String::new(),
            outcome: String::new(),
        }
    }
}

/// One case's measured decomposition.
#[derive(Debug, Clone, Serialize)]
pub struct CaseReport {
    /// `control` or `calibration`.
    pub kind: String,
    pub name: String,
    pub scenario: ScenarioText,
    pub baseline_rss_bytes: u64,
    pub cold_rss_bytes: u64,
    pub cold_hwm_bytes: u64,
    pub floor_rss_bytes: u64,
    pub graph_logical_bytes: u64,
    pub processing_rss_min_bytes: u64,
    pub processing_rss_max_bytes: u64,
    pub peak_hwm_bytes: u64,
    pub session_cost_bytes: u64,
    pub max_connections: u64,
    pub idle_rss_bytes: Vec<u64>,
    pub failure: String,
    /// Logical bytes reserved while the calibration sorts were held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_logical_bytes_held: Option<u64>,
    /// RSS of those sorts above the same sessions after `RETURN 1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_rss_bytes: Option<u64>,
    /// `retained_rss_bytes / peak_logical_bytes_held`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes_per_logical_byte: Option<u64>,
}

/// The default run directory: a fresh `/tmp` root per run, so a re-run never
/// measures a store an earlier one left.
pub fn default_store() -> PathBuf {
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/memory-controls-{run}"))
}

/// Run the cases this configuration selects and write the report, handing
/// each case to `on_case` and each workload round to `on_workload` as it
/// completes — the family's one run.
pub fn run(
    cfg: &Config,
    mut on_case: impl FnMut(&CaseReport),
    on_workload: impl FnMut(&crate::memory_controls::workload::WorkloadReport),
) -> Result<Report, String> {
    if process_status(std::process::id()).is_none() {
        return Err(
            "memory-controls RSS sampling needs Linux /proc/<pid>/status (VmRSS and VmHWM)".into(),
        );
    }
    for (label, path) in [("relate", &cfg.serve_bin)] {
        if !path.is_file() {
            return Err(format!(
                "{label} binary not found at {} — build it before this loadtest",
                path.display()
            ));
        }
    }
    cfg.controls.validate()?;
    let started = stamp();
    let mut cases = Vec::new();
    let mut finish_case = |case: Result<CaseReport, String>| -> Result<(), String> {
        let mut case = case?;
        super::scenario::fill_case(cfg, &mut case);
        on_case(&case);
        cases.push(case);
        Ok(())
    };
    if let Some(case) = cfg.case.as_deref() {
        if !CASE_NAMES.contains(&case) {
            return Err(format!(
                "unknown case {case:?}; expected one of {}",
                CASE_NAMES.join(", ")
            ));
        }
    }
    if selected(cfg, "connection-cap") {
        finish_case(crate::memory_controls::controls::case_connections(cfg))?;
    }
    if selected(cfg, "unlimited-connections") {
        finish_case(crate::memory_controls::controls::case_unlimited(cfg))?;
    }
    if selected(cfg, "concurrent-logical-workspace-limit") {
        finish_case(crate::memory_controls::controls::case_active_queries(cfg))?;
    }
    let workspace_memory_calibration = if selected(cfg, "workspace-memory-calibration") {
        let (case, calibration) = crate::memory_controls::calibration::case_workspace_memory(cfg)?;
        finish_case(Ok(case))?;
        Some(calibration)
    } else {
        None
    };
    let outcome = if selected(cfg, "workload") {
        crate::memory_controls::workload::run(cfg, on_workload)?
    } else {
        crate::memory_controls::workload::WorkloadOutcome {
            reports: Vec::new(),
            gate_error: None,
        }
    };
    let report = Report {
        schema_version: 4,
        name: "memory-controls".into(),
        started_at: started,
        finished_at: stamp(),
        controls: cfg.controls.clone(),
        cases,
        workloads: outcome.reports,
        workspace_memory_calibration,
        cgroup_memory_peak_bytes: cgroup_memory_peak(),
    };
    crate::report::write_report(&cfg.out, &report)?;
    if let Some(err) = outcome.gate_error {
        return Err(err);
    }
    Ok(report)
}

/// One RSS and high-water sample of the serve child.
#[derive(Clone, Copy)]
pub(crate) struct Sample {
    pub(crate) rss: u64,
    pub(crate) hwm: u64,
}

/// The serve child a case measures, under the limits the case asks for.
pub(crate) struct Server {
    child: ServeChild,
    /// The `--http` address the child serves, when the case bound one.
    pub(crate) http: Option<String>,
}

/// The serve flags a case starts its child with.
#[derive(Clone, Copy, Default)]
pub(crate) struct ServeLimits {
    /// `None` omits `--max-connections`, which is the server default of 128
    /// (ADR-0451). `Some(0)` is the explicit unlimited spelling.
    pub(crate) max_connections: Option<u64>,
    pub(crate) concurrent_logical_workspace_limit: Option<u64>,
    /// Also bind the HTTP Query API. The connection cap is shared with Bolt.
    pub(crate) http: bool,
    pub(crate) hold_ms: Option<u64>,
    /// Sleep inside a charged sort, while the retained rows are alive.
    pub(crate) peak_hold_ms: Option<u64>,
    /// Pause only after this many logical bytes are reserved.
    /// Passed through `RELATE_PEAK_HOLD_LOGICAL_BYTES`. The engine
    /// compares it with the reserved logical-byte hold.
    pub(crate) peak_hold_logical_bytes: Option<u64>,
    /// Pause only for this workspace family.
    pub(crate) peak_hold_family: Option<&'static str>,
}

/// The one engine pair the memory-controls child runs.
const ENGINE_PAIR: &str = "in-memory,local";

impl Server {
    /// Reset `store` and start a child on it.
    pub(crate) fn start(bin: &Path, store: &Path, limits: ServeLimits) -> Result<Self, String> {
        reset_dir(store)?;
        Self::launch(bin, store, limits)
    }

    /// Open a store a previous process already committed. Does not delete it.
    pub(crate) fn open_existing(
        bin: &Path,
        store: &Path,
        limits: ServeLimits,
    ) -> Result<Self, String> {
        Self::launch(bin, store, limits)
    }

    fn launch(bin: &Path, store: &Path, limits: ServeLimits) -> Result<Self, String> {
        let ports = free_ports(if limits.http { 2 } else { 1 })?;
        let bolt_port = ports[0];
        let http_port = ports.get(1).copied();
        let mut cmd = serve_command(bin, store, bolt_port);
        cmd.env("RELATE_ENGINE_PAIR", ENGINE_PAIR);
        if let Some(port) = http_port {
            cmd.arg("--http").arg(port.to_string());
        }
        if let Some(value) = limits.max_connections {
            cmd.arg("--max-connections").arg(value.to_string());
        }
        if let Some(value) = limits.concurrent_logical_workspace_limit {
            cmd.arg("--concurrent-logical-workspace-limit")
                .arg(value.to_string());
        }
        // The harness env policy (knob scrub + telemetry opt-out) already
        // ran inside serve_command; this case's deliberate env goes on top.
        cmd.env("RELATE_LOG_LOGICAL_BYTES", "1");
        if let Some(ms) = limits.hold_ms {
            cmd.env("RELATE_QUERY_ADMIT_HOLD_MS", ms.to_string());
        }
        if let Some(ms) = limits.peak_hold_ms {
            cmd.env("RELATE_PEAK_HOLD_MS", ms.to_string());
        }
        if let Some(bytes) = limits.peak_hold_logical_bytes {
            cmd.env("RELATE_PEAK_HOLD_LOGICAL_BYTES", bytes.to_string());
        }
        if let Some(family) = limits.peak_hold_family {
            cmd.env("RELATE_PEAK_HOLD_FAMILY", family);
        }
        let child = ServeChild::spawn(&mut cmd, bolt_port, READY_DEADLINE)?;
        let http = http_port.map(|port| format!("127.0.0.1:{port}"));
        if let Some(addr) = &http {
            child.wait_for(addr)?;
        }
        // The readiness probe drops its socket before the spawn's wait
        // returns. The server still holds that connection's slot until the
        // handler reads EOF. There is no log line for a normal close, so this
        // waits out that handler before a case that fills `--max-connections`.
        thread::sleep(Duration::from_millis(200));
        Ok(Self { child, http })
    }

    /// The bolt address the child serves.
    pub(crate) fn bolt(&self) -> &str {
        &self.child.addr
    }

    pub(crate) fn sample(&self) -> Result<Sample, String> {
        let pid = self.child.pid();
        let status =
            process_status(pid).ok_or_else(|| format!("failed to read /proc/{pid}/status"))?;
        self.child.alive()?;
        Ok(Sample {
            rss: status.rss_bytes,
            hwm: status.peak_rss_bytes,
        })
    }

    pub(crate) fn session_cost(&self, cold_rss: u64) -> Result<u64, String> {
        let session =
            connect_addr(self.bolt()).map_err(|err| format!("{err}\n{}", self.log_text()))?;
        let rss = self.sample()?.rss;
        drop(session);
        Ok(rss.saturating_sub(cold_rss))
    }

    pub(crate) fn log_text(&self) -> String {
        self.child.log_text()
    }

    pub(crate) fn expect_log(&self, needle: &str) -> Result<(), String> {
        self.expect_log_count(needle, 1)
    }

    pub(crate) fn expect_log_count(&self, needle: &str, count: usize) -> Result<(), String> {
        self.expect_log_count_within(needle, count, Duration::from_secs(2))
    }
    pub(crate) fn expect_log_count_within(
        &self,
        needle: &str,
        count: usize,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let seen = self.log_text().matches(needle).count();
            if seen >= count {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "log contained {needle:?} {seen} times, wanted {count}\n{}",
                    self.log_text()
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish(
    name: &str,
    server: &Server,
    baseline: Sample,
    cold: Sample,
    graph_logical_bytes: u64,
    session_cost: u64,
    max_connections: u64,
    idle_rss_bytes: Vec<u64>,
    failure: &str,
) -> Result<CaseReport, String> {
    server.child.alive()?;
    let log = server.log_text();
    for needle in [
        "panicked",
        "stack overflow",
        "fatal runtime error",
        "aborted",
    ] {
        if log.to_ascii_lowercase().contains(needle) {
            return Err(format!("{name}: stderr contains {needle:?}\n{log}"));
        }
    }
    let peak = server.sample()?;
    if let Some(first) = idle_rss_bytes.first().copied() {
        for (index, idle) in idle_rss_bytes.iter().enumerate().skip(1) {
            let growth = idle.saturating_sub(first);
            if growth > IDLE_RESIDUE_BYTES {
                return Err(format!(
                    "{name}: idle RSS grew by {growth} bytes on repeat {index} (residue {IDLE_RESIDUE_BYTES})"
                ));
            }
        }
    }
    let levels = decompose(baseline.rss, cold.rss, peak.hwm, &idle_rss_bytes);
    Ok(CaseReport {
        kind: String::new(),
        name: name.into(),
        scenario: ScenarioText::empty(),
        baseline_rss_bytes: baseline.rss,
        cold_rss_bytes: cold.rss,
        cold_hwm_bytes: cold.hwm,
        floor_rss_bytes: levels.floor,
        graph_logical_bytes,
        processing_rss_min_bytes: levels.processing_min,
        processing_rss_max_bytes: levels.processing_max,
        peak_hwm_bytes: peak.hwm,
        session_cost_bytes: session_cost,
        max_connections,
        idle_rss_bytes,
        failure: failure.to_string(),
        peak_logical_bytes_held: None,
        retained_rss_bytes: None,
        rss_bytes_per_logical_byte: None,
    })
}

struct Decomposition {
    floor: u64,
    processing_min: u64,
    processing_max: u64,
}

/// Split a case's samples into the memory model. Floor is cold Graph RSS
/// above the empty process. Processing min is the least idle RSS still
/// above that cold graph. Processing max is the high-water mark above it.
fn decompose(baseline: u64, cold: u64, peak: u64, idles: &[u64]) -> Decomposition {
    let processing_max = peak.saturating_sub(cold);
    let processing_min = idles
        .iter()
        .map(|idle| idle.saturating_sub(cold))
        .min()
        .unwrap_or(0)
        .min(processing_max);
    Decomposition {
        floor: cold.saturating_sub(baseline),
        processing_min,
        processing_max,
    }
}

/// The crate's canonical retry connect: a fresh serve child accepts its
/// listeners slightly before its Bolt handshake is ready, so a connection
/// refused or cut at the handshake is retried inside a short window, while
/// every other failure is returned at once.
pub(crate) fn connect_addr(addr: &str) -> Result<Session, String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match Session::connect(addr) {
            Ok(session) => return Ok(session),
            Err(err) if err.contains("handshake") || err.starts_with("connect ") => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn reset_dir(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|err| format!("reset {}: {err}", path.display()))?;
    }
    fs::create_dir_all(path).map_err(|err| format!("create {}: {err}", path.display()))
}

fn cgroup_memory_peak() -> Option<u64> {
    for path in [
        "/sys/fs/cgroup/memory.peak",
        "/sys/fs/cgroup/memory/memory.max_usage_in_bytes",
    ] {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        if let Ok(bytes) = text.trim().parse::<u64>() {
            if bytes < 1 << 60 {
                return Some(bytes);
            }
        }
    }
    None
}

fn stamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::decompose;

    #[test]
    fn decompose_splits_baseline_floor_and_processing_range() {
        let levels = decompose(10, 12, 20, &[15, 16, 18]);
        assert_eq!(levels.floor, 2);
        assert_eq!(levels.processing_min, 3);
        assert_eq!(levels.processing_max, 8);
    }

    #[test]
    fn decompose_keeps_processing_min_inside_the_high_water_mark() {
        let levels = decompose(10, 12, 14, &[20]);
        assert_eq!(levels.processing_min, 2);
        assert_eq!(levels.processing_max, 2);
    }
}
