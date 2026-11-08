//! Calibrate physical memory per live logical byte.
//!
//! Every observation starts a fresh server, loads and warms its graph, parks
//! the same sessions with `RETURN 1`, then holds a named workspace family at
//! a configured logical-byte threshold. The report keeps both child VmRSS and
//! whole-cgroup deltas. Raw observations are authoritative; summaries only
//! describe the measured environment.

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::bolt::Session;
use crate::memory_controls::harness::{
    self, connect_addr, CaseReport, Config, ServeLimits, Server,
};

const BATCH: i64 = 500;
const PAYLOAD_BYTES: usize = 256;
/// The charge quantum. Shapes are multiples of it so a hold threshold lines
/// up with a reserved logical-byte total. The report records the bytes.
const QUANTUM_BYTES: u64 = 4096;
const FAMILIES: &[&str] = &["sort", "expand", "aggregation", "distinct", "create"];
/// Logical bytes per query, then holder count. The last three keep the same
/// 16,777,216-byte total and change the query count.
const SHAPES: &[(u64, usize)] = &[
    (1_048_576, 1),
    (4_194_304, 1),
    (16_777_216, 1),
    (8_388_608, 2),
    (4_194_304, 4),
];
/// The equal-total subset of [`SHAPES`]: one, two, and four holders.
const EQUAL_TOTAL: &[(u64, usize)] = &[(16_777_216, 1), (8_388_608, 2), (4_194_304, 4)];
const EQUAL_TOTAL_LOGICAL_BYTES: f64 = 16_777_216.0;
/// A read can intern a few strings into the logical meter. Growth past
/// this is graph data landing in the RSS delta.
const LOGICAL_READ_SLACK: u64 = 4 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct PeakCalibrationObservation {
    pub family: String,
    /// Logical bytes the shape asked the hold to reach.
    pub target_logical_bytes: u64,
    pub holders: usize,
    pub repetition: u32,
    /// Logical bytes reserved while the hold was sampled, summed over holders.
    pub held_logical_bytes: u64,
    pub parked_rss_bytes: u64,
    pub held_rss_bytes: u64,
    pub rss_delta_bytes: u64,
    pub rss_bytes_per_logical_byte: u64,
    pub rss_to_logical_ratio: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parked_cgroup_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_cgroup_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_delta_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_bytes_per_logical_byte: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_to_logical_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeakCalibrationSummary {
    pub family: String,
    pub observations: usize,
    pub rss_slope: f64,
    pub rss_intercept_bytes: f64,
    pub rss_r_squared: f64,
    pub rss_max_abs_residual_bytes: f64,
    pub max_rss_to_logical_ratio: f64,
    pub rss_repeat_spread: f64,
    pub rss_equal_total_concurrency_spread: f64,
    /// Slope over the one-holder shapes only: RSS per logical byte with no
    /// concurrency term mixed in.
    pub rss_single_holder_slope: f64,
    pub rss_single_holder_r_squared: f64,
    /// Fitted RSS bytes each extra concurrent query adds at a constant
    /// total of logical bytes. `None` for `overall`, which mixes families.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_per_holder_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_slope: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_single_holder_slope: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_single_holder_r_squared: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_per_holder_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_intercept_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_r_squared: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_max_abs_residual_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cgroup_to_logical_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_repeat_spread: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_equal_total_concurrency_spread: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeakCalibrationReport {
    pub engine_bytes_per_logical_byte: u64,
    pub repetitions: u32,
    pub observations: Vec<PeakCalibrationObservation>,
    pub summaries: Vec<PeakCalibrationSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conservative_cgroup_ratio: Option<f64>,
}

pub(crate) fn case_workspace_memory(
    cfg: &Config,
) -> Result<(CaseReport, PeakCalibrationReport), String> {
    if let Some(family) = cfg.calibration_family.as_deref() {
        if !FAMILIES.contains(&family) {
            return Err(format!(
                "unknown calibration family {family:?}; expected one of {}",
                FAMILIES.join(", ")
            ));
        }
    }
    let mut observations = Vec::new();
    let mut display_case = None;
    let families = FAMILIES
        .iter()
        .copied()
        .filter(|family| {
            cfg.calibration_family
                .as_deref()
                .is_none_or(|only| only == *family)
        })
        .collect::<Vec<_>>();
    let total = families.len() * SHAPES.len() * cfg.calibration_repeats as usize;
    let started = Instant::now();
    eprintln!(
        "workspace memory calibration: {total} observations ({} families, {} shapes, {} repeats). Each starts a fresh server and holds for {} ms.",
        families.len(),
        SHAPES.len(),
        cfg.calibration_repeats,
        cfg.calibration_hold_ms
    );
    for family in families {
        for &(target, holders) in SHAPES {
            for repetition in 0..cfg.calibration_repeats {
                let index = observations.len();
                // Progress on stderr: the report on stdout stays clean, and a
                // long matrix shows it is moving.
                eprintln!(
                    "  [{}/{total}] {family}  {} x {holders} holder(s)  repeat {}/{}  elapsed {}s",
                    index + 1,
                    crate::memory_controls::format_mem(target),
                    repetition + 1,
                    cfg.calibration_repeats,
                    started.elapsed().as_secs()
                );
                let (observation, case) =
                    run_observation(cfg, family, target, holders, repetition, index == 0)?;
                if case.is_some() {
                    display_case = case;
                }
                observations.push(observation);
            }
        }
    }
    let mut summaries = FAMILIES
        .iter()
        .copied()
        .filter(|family| {
            cfg.calibration_family
                .as_deref()
                .is_none_or(|only| only == *family)
        })
        .map(|family| summarize_family(family, &observations))
        .collect::<Vec<_>>();
    summaries.push(summarize_family("overall", &observations));
    let conservative_cgroup_ratio = observations
        .iter()
        .filter_map(|row| row.cgroup_to_logical_ratio)
        .reduce(f64::max);
    let case = display_case.ok_or("calibration produced no display case")?;
    Ok((
        case,
        PeakCalibrationReport {
            engine_bytes_per_logical_byte: 1,
            repetitions: cfg.calibration_repeats,
            observations,
            summaries,
            conservative_cgroup_ratio,
        },
    ))
}

fn run_observation(
    cfg: &Config,
    family: &'static str,
    target: u64,
    holders: usize,
    repetition: u32,
    make_case: bool,
) -> Result<(PeakCalibrationObservation, Option<CaseReport>), String> {
    let store = cfg.store.join(format!(
        "workspace-memory-{family}-b{target}-h{holders}-r{repetition}"
    ));
    let server = Server::start(
        &cfg.serve_bin,
        &store,
        ServeLimits {
            concurrent_logical_workspace_limit: Some(u64::MAX),
            peak_hold_ms: Some(cfg.calibration_hold_ms),
            // The engine compares this threshold to reserved logical bytes.
            peak_hold_logical_bytes: Some(target),
            peak_hold_family: Some(family),
            ..ServeLimits::default()
        },
    )?;
    let baseline = server.sample()?;
    let mut loader = connect_addr(server.bolt())?;
    let query = load_scenario(&mut loader, family, target)?;
    // Create calibrates statement-local scratch and intentionally has no
    // graph preload. This also gives that case a logical-meter sample.
    loader.run("RETURN 1")?;
    let logical = crate::processing_logical::logical_bytes(&server)?;
    drop(loader);
    let cold = server.sample()?;

    let mut parked_sessions = Vec::with_capacity(holders);
    for _ in 0..holders {
        let mut session = connect_addr(server.bolt())?;
        session.run("RETURN 1")?;
        parked_sessions.push(session);
    }
    let parked = server.sample()?.rss;
    let parked_cgroup = cgroup_memory_current();
    let session_cost = parked.saturating_sub(cold.rss) / holders as u64;

    let mut joins = Vec::with_capacity(holders);
    for mut session in parked_sessions {
        let query = query.clone();
        joins.push(thread::spawn(move || session.run(&query)));
    }
    server.expect_log_count_within("peak hold:", holders, Duration::from_secs(60))?;
    let mut held_rss = 0;
    let mut held_cgroup = None;
    for _ in 0..5 {
        held_rss = held_rss.max(server.sample()?.rss);
        held_cgroup = max_option(held_cgroup, cgroup_memory_current());
        thread::sleep(Duration::from_millis(50));
    }
    for join in joins {
        join.join()
            .map_err(|_| format!("{family} query thread panicked"))?
            .map_err(|err| format!("{err}\n{}", server.log_text()))?;
    }
    let holds = peak_holds(&server.log_text());
    let after = crate::processing_logical::logical_bytes(&server)?;
    let growth = after.saturating_sub(logical);
    if family != "create" && growth > LOGICAL_READ_SLACK {
        return Err(format!(
            "graph grew during the {family} hold: {logical} -> {after} ({growth} bytes)"
        ));
    }
    if holds.len() != holders {
        return Err(format!(
            "expected {holders} peak holds, saw {}\n{}",
            holds.len(),
            server.log_text()
        ));
    }
    if holds
        .iter()
        .any(|hold| hold.family != family || hold.held < target || hold.peak_bytes == 0)
    {
        return Err(format!(
            "{family} did not reach its peak reservation target\n{}",
            server.log_text()
        ));
    }
    // `held` in the engine log is logical bytes.
    let held_logical_bytes: u64 = holds.iter().map(|hold| hold.held).sum();
    let retained = held_rss.saturating_sub(parked);
    if held_logical_bytes == 0 || retained == 0 {
        return Err(format!(
            "logical-byte RSS calibration saw family={family} logical_bytes={held_logical_bytes} retained={retained} parked={parked} held={held_rss}\n{}",
            server.log_text()
        ));
    }
    let cgroup_delta = held_cgroup
        .zip(parked_cgroup)
        .map(|(held, parked)| held.saturating_sub(parked));
    let observation = PeakCalibrationObservation {
        family: family.to_string(),
        target_logical_bytes: target,
        holders,
        repetition,
        held_logical_bytes,
        parked_rss_bytes: parked,
        held_rss_bytes: held_rss,
        rss_delta_bytes: retained,
        rss_bytes_per_logical_byte: retained / held_logical_bytes,
        rss_to_logical_ratio: retained as f64 / held_logical_bytes as f64,
        parked_cgroup_bytes: parked_cgroup,
        held_cgroup_bytes: held_cgroup,
        cgroup_delta_bytes: cgroup_delta,
        cgroup_bytes_per_logical_byte: cgroup_delta.map(|delta| delta / held_logical_bytes),
        cgroup_to_logical_ratio: cgroup_delta.map(|delta| delta as f64 / held_logical_bytes as f64),
    };
    let idle = server.sample()?.rss;
    let case = if make_case {
        let mut case = harness::finish(
            "workspace-memory-calibration",
            &server,
            baseline,
            cold,
            logical,
            session_cost,
            0,
            vec![idle],
            "cross-family logical-byte-to-RSS calibration matrix",
        )?;
        case.peak_logical_bytes_held = Some(held_logical_bytes);
        case.retained_rss_bytes = Some(retained);
        case.rss_bytes_per_logical_byte = Some(retained / held_logical_bytes);
        Some(case)
    } else {
        None
    };
    Ok((observation, case))
}

fn load_scenario(
    session: &mut Session,
    family: &str,
    target_logical_bytes: u64,
) -> Result<String, String> {
    let quanta = target_logical_bytes / QUANTUM_BYTES;
    match family {
        "sort" | "aggregation" | "distinct" => {
            // Sixteen rows per quantum gives every family enough retained
            // workspace to reach the logical-byte threshold.
            let rows = i64::try_from(quanta.saturating_mul(16))
                .map_err(|_| "calibration row count overflow".to_string())?
                .max(1_000);
            load_rows(session, rows)?;
            Ok(match family {
                "sort" => "MATCH (n:Cal) RETURN n.id, n.payload ORDER BY n.id",
                "aggregation" => {
                    "MATCH (n:Cal) WITH n.id AS a, n.id AS b, n.id AS c, n.id AS d, n.id AS e, n.id AS f, n.id AS g, n.id AS h RETURN collect(a) AS ids"
                }
                "distinct" => {
                    "MATCH (n:Cal) RETURN DISTINCT n.id AS a, n.id AS b, n.id AS c, n.id AS d, n.id AS e, n.id AS f, n.id AS g, n.id AS h"
                }
                _ => unreachable!(),
            }
            .to_string())
        }
        "expand" => {
            let rows = i64::try_from(quanta.saturating_mul(64))
                .map_err(|_| "expand row count overflow".to_string())?
                .max(2_000);
            load_expand(session, rows)?;
            Ok("MATCH (:CalRoot)-[:CAL_EDGE]->(n:CalLeaf) RETURN n.id AS leaf".into())
        }
        "create" => {
            let rows = quanta.saturating_mul(32).max(2_000);
            let rows = i64::try_from(rows).map_err(|_| "create row count overflow".to_string())?;
            // Stream graph-owned seed rows. A giant `UNWIND range(...)`
            // materializes input that is not create scratch and would make
            // this family measure an unrelated accounting gap.
            load_rows(session, rows)?;
            Ok(
                "MATCH (s:Cal) CREATE (:CalCreate {id: s.id, payload: s.payload}) RETURN count(*)"
                    .into(),
            )
        }
        _ => Err(format!("unknown calibration family {family}")),
    }
}

fn load_rows(session: &mut Session, rows: i64) -> Result<(), String> {
    let payload = "p".repeat(PAYLOAD_BYTES);
    let mut start = 0;
    while start < rows {
        let end = (start + BATCH).min(rows) - 1;
        session.run(&format!(
            "UNWIND range({start}, {end}) AS i CREATE (:Cal {{id: i, payload: '{payload}'}})"
        ))?;
        start += BATCH;
    }
    Ok(())
}

fn load_expand(session: &mut Session, rows: i64) -> Result<(), String> {
    session.run("CREATE (:CalRoot {id: 0})")?;
    let mut start = 0;
    while start < rows {
        let end = (start + BATCH).min(rows) - 1;
        session.run(&format!(
            "MATCH (r:CalRoot {{id: 0}}) UNWIND range({start}, {end}) AS i CREATE (r)-[:CAL_EDGE]->(:CalLeaf {{id: i}})"
        ))?;
        start += BATCH;
    }
    Ok(())
}

fn cgroup_memory_current() -> Option<u64> {
    for path in [
        "/sys/fs/cgroup/memory.current",
        "/sys/fs/cgroup/memory/memory.usage_in_bytes",
    ] {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        if let Ok(bytes) = text.trim().parse::<u64>() {
            return Some(bytes);
        }
    }
    None
}

fn max_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn summarize_family(
    family: &str,
    observations: &[PeakCalibrationObservation],
) -> PeakCalibrationSummary {
    let rows = observations
        .iter()
        .filter(|row| family == "overall" || row.family == family)
        .collect::<Vec<_>>();
    let rss_points = rows
        .iter()
        .map(|row| (row.held_logical_bytes as f64, row.rss_delta_bytes as f64))
        .collect::<Vec<_>>();
    let cgroup_points = rows
        .iter()
        .filter_map(|row| {
            row.cgroup_delta_bytes
                .map(|delta| (row.held_logical_bytes as f64, delta as f64))
        })
        .collect::<Vec<_>>();
    let (rss_slope, rss_intercept, rss_r_squared) = linear_fit(&rss_points);
    let cgroup_fit = (!cgroup_points.is_empty()).then(|| linear_fit(&cgroup_points));
    let rss_max_abs_residual_bytes = max_abs_residual(&rss_points, rss_slope, rss_intercept);
    let cgroup_max_abs_residual_bytes =
        cgroup_fit.map(|fit| max_abs_residual(&cgroup_points, fit.0, fit.1));
    let single = rows
        .iter()
        .copied()
        .filter(|row| row.holders == 1)
        .collect::<Vec<_>>();
    let rss_single = linear_fit(&logical_byte_points(&single, |row| {
        Some(row.rss_delta_bytes)
    }));
    let cgroup_single_points = logical_byte_points(&single, |row| row.cgroup_delta_bytes);
    let cgroup_single =
        (!cgroup_single_points.is_empty()).then(|| linear_fit(&cgroup_single_points));
    let per_family = family != "overall";
    PeakCalibrationSummary {
        family: family.to_string(),
        observations: rows.len(),
        rss_slope,
        rss_intercept_bytes: rss_intercept,
        rss_r_squared,
        rss_max_abs_residual_bytes,
        max_rss_to_logical_ratio: rows
            .iter()
            .map(|row| row.rss_to_logical_ratio)
            .fold(0.0, f64::max),
        rss_repeat_spread: repeat_spread(&rows, |row| Some(row.rss_to_logical_ratio))
            .unwrap_or(0.0),
        rss_equal_total_concurrency_spread: equal_total_spread(&rows, |row| {
            Some(row.rss_delta_bytes as f64)
        })
        .unwrap_or(0.0),
        rss_single_holder_slope: rss_single.0,
        rss_single_holder_r_squared: rss_single.2,
        rss_per_holder_bytes: per_family
            .then(|| per_holder_bytes(&rows, |row| Some(row.rss_delta_bytes)))
            .flatten(),
        cgroup_slope: cgroup_fit.map(|fit| fit.0),
        cgroup_single_holder_slope: cgroup_single.map(|fit| fit.0),
        cgroup_single_holder_r_squared: cgroup_single.map(|fit| fit.2),
        cgroup_per_holder_bytes: per_family
            .then(|| per_holder_bytes(&rows, |row| row.cgroup_delta_bytes))
            .flatten(),
        cgroup_intercept_bytes: cgroup_fit.map(|fit| fit.1),
        cgroup_r_squared: cgroup_fit.map(|fit| fit.2),
        cgroup_max_abs_residual_bytes,
        max_cgroup_to_logical_ratio: rows
            .iter()
            .filter_map(|row| row.cgroup_to_logical_ratio)
            .reduce(f64::max),
        cgroup_repeat_spread: repeat_spread(&rows, |row| row.cgroup_to_logical_ratio),
        cgroup_equal_total_concurrency_spread: equal_total_spread(&rows, |row| {
            row.cgroup_delta_bytes.map(|bytes| bytes as f64)
        }),
    }
}

fn logical_byte_points(
    rows: &[&PeakCalibrationObservation],
    delta: impl Fn(&PeakCalibrationObservation) -> Option<u64>,
) -> Vec<(f64, f64)> {
    rows.iter()
        .filter_map(|row| delta(row).map(|bytes| (row.held_logical_bytes as f64, bytes as f64)))
        .collect()
}

/// Bytes per extra holder from the equal-total shapes. Each delta is
/// rescaled to 16,777,216 logical bytes first, so a few bytes of hold
/// noise are not read as holder cost.
fn per_holder_bytes(
    rows: &[&PeakCalibrationObservation],
    delta: impl Fn(&PeakCalibrationObservation) -> Option<u64>,
) -> Option<f64> {
    let points = rows
        .iter()
        .filter(|row| EQUAL_TOTAL.contains(&(row.target_logical_bytes, row.holders)))
        .filter_map(|row| {
            let bytes = delta(row)? as f64;
            let scale = EQUAL_TOTAL_LOGICAL_BYTES / row.held_logical_bytes.max(1) as f64;
            Some((row.holders as f64, bytes * scale))
        })
        .collect::<Vec<_>>();
    let holder_counts = points
        .iter()
        .map(|point| point.0 as usize)
        .collect::<std::collections::BTreeSet<_>>();
    (holder_counts.len() >= 2).then(|| linear_fit(&points).0)
}

fn max_abs_residual(points: &[(f64, f64)], slope: f64, intercept: f64) -> f64 {
    points
        .iter()
        .map(|point| (point.1 - (intercept + slope * point.0)).abs())
        .fold(0.0, f64::max)
}

fn repeat_spread(
    rows: &[&PeakCalibrationObservation],
    value: impl Fn(&PeakCalibrationObservation) -> Option<f64>,
) -> Option<f64> {
    let mut largest = None::<f64>;
    for family in FAMILIES {
        for &(target, holders) in SHAPES {
            let values = rows
                .iter()
                .filter(|row| {
                    row.family == *family
                        && row.target_logical_bytes == target
                        && row.holders == holders
                })
                .filter_map(|row| value(row))
                .collect::<Vec<_>>();
            if values.len() > 1 {
                let min = values.iter().copied().fold(f64::INFINITY, f64::min);
                let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                largest = Some(largest.unwrap_or(0.0).max(max - min));
            }
        }
    }
    largest
}

fn equal_total_spread(
    rows: &[&PeakCalibrationObservation],
    value: impl Fn(&PeakCalibrationObservation) -> Option<f64>,
) -> Option<f64> {
    let mut largest = None::<f64>;
    for family in FAMILIES {
        let means = EQUAL_TOTAL
            .iter()
            .filter_map(|&(target, holders)| {
                let values = rows
                    .iter()
                    .filter(|row| {
                        row.family == *family
                            && row.target_logical_bytes == target
                            && row.holders == holders
                    })
                    .filter_map(|row| value(row))
                    .collect::<Vec<_>>();
                (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
            })
            .collect::<Vec<_>>();
        if means.len() == 3 {
            let min = means.iter().copied().fold(f64::INFINITY, f64::min);
            let max = means.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if min > 0.0 {
                largest = Some(largest.unwrap_or(0.0).max(max / min - 1.0));
            }
        }
    }
    largest
}

fn linear_fit(points: &[(f64, f64)]) -> (f64, f64, f64) {
    if points.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let n = points.len() as f64;
    let mean_x = points.iter().map(|point| point.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|point| point.1).sum::<f64>() / n;
    let sxx = points
        .iter()
        .map(|point| (point.0 - mean_x).powi(2))
        .sum::<f64>();
    let sxy = points
        .iter()
        .map(|point| (point.0 - mean_x) * (point.1 - mean_y))
        .sum::<f64>();
    let slope = if sxx == 0.0 { 0.0 } else { sxy / sxx };
    let intercept = mean_y - slope * mean_x;
    let residual = points
        .iter()
        .map(|point| (point.1 - (intercept + slope * point.0)).powi(2))
        .sum::<f64>();
    let total = points
        .iter()
        .map(|point| (point.1 - mean_y).powi(2))
        .sum::<f64>();
    let r_squared = if total == 0.0 {
        1.0
    } else {
        1.0 - residual / total
    };
    (slope, intercept, r_squared)
}

struct PeakHold {
    family: String,
    held: u64,
    peak_bytes: u64,
}

fn peak_holds(log: &str) -> Vec<PeakHold> {
    log.lines().filter_map(parse_peak_hold).collect()
}

fn parse_peak_hold(line: &str) -> Option<PeakHold> {
    let rest = line.trim().strip_prefix("peak hold:")?;
    let mut family = None;
    let mut held = None;
    let mut peak_bytes = None;
    for part in rest.split_whitespace() {
        if let Some(value) = part.strip_prefix("family=") {
            family = Some(value.to_string());
        } else if let Some(value) = part.strip_prefix("held=") {
            held = value.parse().ok();
        } else if let Some(value) = part.strip_prefix("peak_bytes=") {
            peak_bytes = value.parse().ok();
        }
    }
    Some(PeakHold {
        family: family.unwrap_or_else(|| "sort".to_string()),
        held: held?,
        peak_bytes: peak_bytes?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_hold_line_carries_the_reservation() {
        let hold = parse_peak_hold(
            "peak hold: family=expand peak_logical_bytes=2236416 held=2236416 peak_bytes=2239488",
        )
        .expect("hold");
        assert_eq!(hold.family, "expand");
        assert_eq!(hold.held, 2_236_416);
        assert_eq!(hold.peak_bytes, 2_239_488);
        assert!(parse_peak_hold("logical-bytes 12").is_none());
    }

    #[test]
    fn linear_fit_recovers_slope_and_intercept() {
        let (slope, intercept, r_squared) = linear_fit(&[(1.0, 7.0), (2.0, 9.0), (3.0, 11.0)]);
        assert!((slope - 2.0).abs() < f64::EPSILON);
        assert!((intercept - 5.0).abs() < f64::EPSILON);
        assert!((r_squared - 1.0).abs() < f64::EPSILON);
    }

    fn observation(target: u64, holders: usize, held: u64, rss: u64) -> PeakCalibrationObservation {
        PeakCalibrationObservation {
            family: "create".into(),
            target_logical_bytes: target,
            holders,
            repetition: 0,
            held_logical_bytes: held,
            parked_rss_bytes: 0,
            held_rss_bytes: rss,
            rss_delta_bytes: rss,
            rss_bytes_per_logical_byte: rss / held.max(1),
            rss_to_logical_ratio: rss as f64 / held.max(1) as f64,
            parked_cgroup_bytes: None,
            held_cgroup_bytes: None,
            cgroup_delta_bytes: None,
            cgroup_bytes_per_logical_byte: None,
            cgroup_to_logical_ratio: None,
        }
    }

    #[test]
    fn summary_separates_single_query_slope_from_holder_cost() {
        let bytes = |quanta: u64| quanta * QUANTUM_BYTES;
        let holder = 1_000_000;
        let rows = vec![
            observation(bytes(256), 1, bytes(256), bytes(256) * 2),
            observation(bytes(1024), 1, bytes(1024), bytes(1024) * 2),
            observation(bytes(4096), 1, bytes(4096), bytes(4096) * 2),
            observation(bytes(2048), 2, bytes(4096), bytes(4096) * 2 + holder),
            observation(bytes(1024), 4, bytes(4096), bytes(4096) * 2 + 3 * holder),
        ];
        let summary = summarize_family("create", &rows);
        assert!((summary.rss_single_holder_slope - 2.0).abs() < 1e-9);
        assert!((summary.rss_single_holder_r_squared - 1.0).abs() < 1e-9);
        let per_holder = summary.rss_per_holder_bytes.expect("per-holder cost");
        assert!((per_holder - holder as f64).abs() < 1.0, "{per_holder}");
        assert!(summary.rss_slope > summary.rss_single_holder_slope);
        assert!(summarize_family("overall", &rows)
            .rss_per_holder_bytes
            .is_none());
    }

    #[test]
    fn observation_serializes_raw_process_and_cgroup_samples() {
        let observation = PeakCalibrationObservation {
            family: "sort".into(),
            target_logical_bytes: 40_960,
            holders: 2,
            repetition: 0,
            held_logical_bytes: 90_112,
            parked_rss_bytes: 100,
            held_rss_bytes: 200,
            rss_delta_bytes: 100,
            rss_bytes_per_logical_byte: 4,
            rss_to_logical_ratio: 0.5,
            parked_cgroup_bytes: Some(300),
            held_cgroup_bytes: Some(500),
            cgroup_delta_bytes: Some(200),
            cgroup_bytes_per_logical_byte: Some(9),
            cgroup_to_logical_ratio: Some(1.0),
        };
        let json = serde_json::to_value(observation).expect("serialize");
        assert_eq!(json["family"], "sort");
        assert_eq!(json["held_logical_bytes"], 90_112);
        assert!(json.get("held_tokens").is_none());
        assert_eq!(json["cgroup_delta_bytes"], 200);
    }
}
