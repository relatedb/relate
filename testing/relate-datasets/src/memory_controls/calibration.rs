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

use crate::memory_controls::harness::{
    self, connect_addr, CaseReport, Config, ServeLimits, Server,
};
use relate_test_support::bolt::client::Session;

mod report;

use report::{summarize_family, Scope};
pub use report::{PeakCalibrationObservation, PeakCalibrationReport, PeakCalibrationSummary};

const BATCH: i64 = 500;
const PAYLOAD_BYTES: usize = 256;
/// How a family's graph is seeded before the measured statement runs.
#[derive(Debug)]
enum Seed {
    /// `:Cal` nodes with id and payload columns.
    Rows,
    /// One `:CalRoot` with `:CalLeaf` edges hanging off it.
    Expand,
}

/// One family's calibration scenario: the load's row density per charge
/// quantum, the smallest load, how the graph is seeded, whether the
/// measured statement writes (so the read-only growth guard exempts it),
/// and the statement the observation holds. The one record a family's
/// density recalibration edits — nothing else in the module spells
/// per-family data.
#[derive(Debug)]
struct Scenario {
    rows_per_quantum: u64,
    floor: i64,
    seed: Seed,
    /// True when the statement grows the logical graph (the create
    /// family): the read-only growth guard exempts exactly these.
    grows_graph: bool,
    statement: &'static str,
}

/// One calibration family: its wire name with its scenario. The
/// [`FAMILIES`] table is the vocabulary's single home — every
/// family-scoped path resolves through it, and a new family is one row.
#[derive(Debug)]
pub struct FamilySpec {
    /// The wire name: the `--calibration-family` value, the store
    /// directory, the progress line, the `RELATE_PEAK_HOLD_FAMILY` value,
    /// and the report's family field.
    pub name: &'static str,
    scenario: Scenario,
}

/// The calibration families, in run order.
pub const FAMILIES: &[FamilySpec] = &[
    FamilySpec {
        name: "sort",
        scenario: Scenario {
            // Sixteen rows per quantum gives the sort buffer enough
            // retained workspace to reach the logical-byte threshold.
            rows_per_quantum: 16,
            floor: 1_000,
            seed: Seed::Rows,
            grows_graph: false,
            statement: "MATCH (n:Cal) RETURN n.id, n.payload ORDER BY n.id",
        },
    },
    FamilySpec {
        name: "expand",
        scenario: Scenario {
            rows_per_quantum: 64,
            floor: 2_000,
            seed: Seed::Expand,
            grows_graph: false,
            statement: "MATCH (:CalRoot)-[:CAL_EDGE]->(n:CalLeaf) RETURN n.id AS leaf",
        },
    },
    FamilySpec {
        name: "aggregation",
        scenario: Scenario {
            // The group table is the aggregation's one workspace-charged
            // retention: every group landing bills its entry, key, seed and
            // accumulator slots to the ledger, where the hold samples. The
            // old keyless `collect` shape crossed only through the input
            // vector the statement billed at open — a materialization
            // ADR-0555 streaming (LABS-1439) removed, leaving one keyless
            // group that never reaches any target. Grouping on the eight id
            // columns lands one group per row (~330 bytes a landing, eight
            // word keys), so thirty-two rows per quantum crosses every
            // target mid-fold with ~2.6x margin at the smallest shape.
            rows_per_quantum: 32,
            floor: 1_000,
            seed: Seed::Rows,
            grows_graph: false,
            statement:
                "MATCH (n:Cal) WITH n.id AS a, n.id AS b, n.id AS c, n.id AS d, n.id AS e, n.id AS f, n.id AS g, n.id AS h RETURN a, b, c, d, e, f, g, h, collect(a) AS ids",
        },
    },
    FamilySpec {
        name: "distinct",
        scenario: Scenario {
            // Sixteen rows per quantum gives the distinct set's entries
            // enough retained workspace to reach the threshold.
            rows_per_quantum: 16,
            floor: 1_000,
            seed: Seed::Rows,
            grows_graph: false,
            statement:
                "MATCH (n:Cal) RETURN DISTINCT n.id AS a, n.id AS b, n.id AS c, n.id AS d, n.id AS e, n.id AS f, n.id AS g, n.id AS h",
        },
    },
    FamilySpec {
        name: "create",
        scenario: Scenario {
            // Stream graph-owned seed rows. A giant `UNWIND range(...)`
            // materializes input that is not create scratch and would make
            // this family measure an unrelated accounting gap.
            rows_per_quantum: 32,
            floor: 2_000,
            seed: Seed::Rows,
            grows_graph: true,
            statement:
                "MATCH (s:Cal) CREATE (:CalCreate {id: s.id, payload: s.payload}) RETURN count(*)",
        },
    },
];

/// Resolve a `--calibration-family` name against the table: the one loud
/// unknown-family rejection, run where the config is built so a bad name
/// fails at startup regardless of which case runs.
pub fn family_spec(name: &str) -> Result<&'static FamilySpec, String> {
    FAMILIES
        .iter()
        .find(|spec| spec.name == name)
        .ok_or_else(|| {
            format!(
                "unknown calibration family {name:?}; expected one of {}",
                FAMILIES
                    .iter()
                    .map(|spec| spec.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}
/// Logical bytes per query, then holder count. The last three keep the same
/// 16,777,216-byte total and change the query count.
const SHAPES: &[(u64, usize)] = &[
    (1_048_576, 1),
    (4_194_304, 1),
    (16_777_216, 1),
    (8_388_608, 2),
    (4_194_304, 4),
];
/// A read can intern a few strings into the logical meter. Growth past
/// this is graph data landing in the RSS delta.
const LOGICAL_READ_SLACK: u64 = 4 * 1024;

pub(crate) fn case_workspace_memory(
    cfg: &Config,
) -> Result<(CaseReport, PeakCalibrationReport), String> {
    let mut observations = Vec::new();
    let mut display_case = None;
    // The config's family is already typed (parsed where it is built); the
    // table is the one filter over it.
    let families = FAMILIES
        .iter()
        .filter(|spec| {
            cfg.calibration_family
                .is_none_or(|only| only.name == spec.name)
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
    for spec in families.iter().copied() {
        for &(target, holders) in SHAPES {
            for repetition in 0..cfg.calibration_repeats {
                let index = observations.len();
                // Progress on stderr: the report on stdout stays clean, and a
                // long matrix shows it is moving.
                eprintln!(
                    "  [{}/{total}] {}  {} x {holders} holder(s)  repeat {}/{}  elapsed {}s",
                    index + 1,
                    spec.name,
                    crate::memory_controls::format_mem(target),
                    repetition + 1,
                    cfg.calibration_repeats,
                    started.elapsed().as_secs()
                );
                let (observation, case) =
                    run_observation(cfg, spec, target, holders, repetition, index == 0)?;
                if case.is_some() {
                    display_case = case;
                }
                observations.push(observation);
            }
        }
    }
    let mut summaries = families
        .iter()
        .copied()
        .map(|spec| summarize_family(Scope::Family(spec), &observations))
        .collect::<Result<Vec<_>, _>>()?;
    summaries.push(summarize_family(Scope::Overall, &observations)?);
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
    spec: &'static FamilySpec,
    target: u64,
    holders: usize,
    repetition: u32,
    make_case: bool,
) -> Result<(PeakCalibrationObservation, Option<CaseReport>), String> {
    // The wire spelling for every prose, log, and report use; the spec's
    // own fields stay for the decisions below.
    let family_name = spec.name;
    let store = cfg.store.join(format!(
        "workspace-memory-{family_name}-b{target}-h{holders}-r{repetition}"
    ));
    let server = Server::start(
        &cfg.serve_bin,
        &store,
        ServeLimits {
            concurrent_logical_workspace_limit: Some(u64::MAX),
            peak_hold_ms: Some(cfg.calibration_hold_ms),
            // The engine compares this threshold to reserved logical bytes.
            peak_hold_logical_bytes: Some(target),
            peak_hold_family: Some(family_name),
            ..ServeLimits::default()
        },
    )?;
    let baseline = server.sample()?;
    let mut loader = connect_addr(server.bolt())?;
    let query = load_scenario(&mut loader, spec, target)?;
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

    // The statement is static, so every holder thread captures it without
    // a per-holder clone.
    let mut joins = Vec::with_capacity(holders);
    for mut session in parked_sessions {
        joins.push(thread::spawn(move || session.run(query)));
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
            .map_err(|_| format!("{family_name} query thread panicked"))?
            .map_err(|err| format!("{err}\n{}", server.log_text()))?;
    }
    let held_logical_bytes = verify_holds(&server, spec, target, holders, logical)?;
    let retained = held_rss.saturating_sub(parked);
    if held_logical_bytes == 0 || retained == 0 {
        return Err(format!(
            "logical-byte RSS calibration saw family={family_name} logical_bytes={held_logical_bytes} retained={retained} parked={parked} held={held_rss}\n{}",
            server.log_text()
        ));
    }
    let cgroup_delta = held_cgroup
        .zip(parked_cgroup)
        .map(|(held, parked)| held.saturating_sub(parked));
    let observation = PeakCalibrationObservation {
        family: family_name.to_string(),
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

/// The hold policy: the graph did not grow during the hold (unless the
/// family grows it), every holder logged a peak hold of this family that
/// reached `target`, and the reservations are real. Returns the logical
/// bytes the holds reserved, summed over holders; `logical` is the graph's
/// logical size before the hold.
fn verify_holds(
    server: &Server,
    spec: &'static FamilySpec,
    target: u64,
    holders: usize,
    logical: u64,
) -> Result<u64, String> {
    let family_name = spec.name;
    let holds = peak_holds(&server.log_text());
    let after = crate::processing_logical::logical_bytes(server)?;
    let growth = after.saturating_sub(logical);
    if !spec.scenario.grows_graph && growth > LOGICAL_READ_SLACK {
        return Err(format!(
            "graph grew during the {family_name} hold: {logical} -> {after} ({growth} bytes)"
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
        .any(|hold| hold.family != family_name || hold.held < target || hold.peak_bytes == 0)
    {
        return Err(format!(
            "{family_name} did not reach its peak reservation target\n{}",
            server.log_text()
        ));
    }
    // `held` in the engine log is logical bytes.
    Ok(holds.iter().map(|hold| hold.held).sum())
}

/// The one row-count policy: rows per charge quantum, floored, converted
/// once. Every family's load goes through it.
fn rows_for(quanta: u64, per_quanta: u64, floor: i64) -> Result<i64, String> {
    i64::try_from(quanta.saturating_mul(per_quanta))
        .map_err(|_| "calibration row count overflow".to_string())
        .map(|rows| rows.max(floor))
}

/// Seed the spec's graph and return its statement. The charge quantum is
/// the engine's: shapes are multiples of it so a hold threshold lines up
/// with a reserved logical-byte total.
fn load_scenario(
    session: &mut Session,
    spec: &'static FamilySpec,
    target_logical_bytes: u64,
) -> Result<&'static str, String> {
    let scenario = &spec.scenario;
    let quanta = target_logical_bytes / relate_core::tokens::CHARGE_QUANTUM_BYTES;
    let rows = rows_for(quanta, scenario.rows_per_quantum, scenario.floor)?;
    match scenario.seed {
        Seed::Rows => load_rows(session, rows)?,
        Seed::Expand => load_expand(session, rows)?,
    }
    Ok(scenario.statement)
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
}
