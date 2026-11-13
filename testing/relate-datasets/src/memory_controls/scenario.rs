//! Given / When / Then text for each memory-controls case. The report stores
//! it so the comparator and the pull-request comment can quote the same words
//! the run printed.

use super::format_mem;
use super::harness::{CaseReport, Config, ScenarioText};
use super::workload::{IndexMode, ReadQuery, WorkloadReport};

pub fn fill_case(cfg: &Config, case: &mut CaseReport) {
    case.kind = kind_for(&case.name).into();
    case.scenario = for_case(cfg, case);
}

pub fn fill_workload(cfg: &Config, report: &mut WorkloadReport) {
    report.scenario = for_workload(cfg, report);
    report.case.kind = "control".into();
    report.case.scenario = report.scenario.clone();
}

fn kind_for(name: &str) -> &'static str {
    if name == "workspace-memory-calibration" {
        "calibration"
    } else {
        "control"
    }
}

fn for_case(cfg: &Config, case: &CaseReport) -> ScenarioText {
    let scale = cfg.controls.graph_scale;
    match case.name.as_str() {
        "connection-cap" => ScenarioText {
            given: format!(
                "a ledger at graph scale {scale}. The verification starts relate serve with --max-connections 2, shared by Bolt and HTTP."
            ),
            when: "a Bolt session writes a review while an HTTP query reads one account. A third connection opens. One session closes, and another connection opens. This repeats three times.".into(),
            then: "the third connection is closed and the log names bolt: --max-connections 2 reached. A later connection is accepted. The child stays alive, and idle RSS stays within 8 MiB of the first sample.".into(),
            outcome: "the third connection was closed and the log named the cap. A later connection was accepted.".into(),
        },
        "unlimited-connections" => ScenarioText {
            given: format!(
                "a ledger at graph scale {scale}. The verification starts relate serve with --max-connections 0."
            ),
            when: "129 Bolt sessions open. The first reads one account and writes a review. This repeats twice.".into(),
            then: "connection 129 is accepted. The child stays alive, and idle RSS stays within 8 MiB of the first sample.".into(),
            outcome: "connection 129 was accepted, and the first session read an account and wrote a review.".into(),
        },
        "concurrent-logical-workspace-limit" => ScenarioText {
            given: format!(
                "a ledger at graph scale {scale}, loaded with admission off. The verification reopens it with --max-connections 4 and a pool sized for one statement, its cached plan, and one later statement."
            ),
            when: "one session holds RETURN 1 and a second session writes a review. After the holder finishes, that session writes the review and reads the account. This repeats three times.".into(),
            then: "the overlapping write returns concurrent-logical-workspace-limit. The running query stays in place. The later write and read succeed. The child stays alive, and idle RSS stays within 8 MiB of the first sample.".into(),
            outcome: format!(
                "the overlapping write was refused ({}). The same session then wrote the review and read the account.",
                failure_summary(&case.failure)
            ),
        },
        "workspace-memory-calibration" => {
            // The families prose derives from the one table; no second
            // spelling of the vocabulary lives here.
            let families = match cfg.calibration_family {
                Some(spec) => spec.name.to_string(),
                None => super::calibration::FAMILIES
                    .iter()
                    .map(|spec| spec.name)
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            let outcome = match (
                case.peak_logical_bytes_held,
                case.retained_rss_bytes,
                case.rss_bytes_per_logical_byte,
            ) {
                (Some(logical), Some(retained), Some(per)) => format!(
                    "the displayed observation held {} and retained {} ({} RSS bytes per logical byte).",
                    format_mem(logical),
                    format_mem(retained),
                    per,
                ),
                _ => case.failure.clone(),
            };
            ScenarioText {
                given: format!(
                    "a fresh server per observation, admission on, a hold of {} ms, and {} repeats. Families: {families}.",
                    cfg.calibration_hold_ms, cfg.calibration_repeats
                ),
                when: format!(
                    "each observation parks its sessions on RETURN 1, then holds one workspace family at the target reservation. The shapes are one holder at {}, {}, and {}, then the same {} across two and four holders.",
                    format_mem(1_048_576),
                    format_mem(4_194_304),
                    format_mem(16_777_216),
                    format_mem(16_777_216),
                ),
                then: "each observation records VmRSS and cgroup deltas against the live held logical bytes. The child stays alive.".into(),
                outcome,
            }
        }
        _ => ScenarioText {
            given: format!("case {}.", case.name),
            when: "the case runs.".into(),
            then: "the child stays alive, and idle RSS stays within 8 MiB of the first sample.".into(),
            outcome: first_line(&case.failure).to_string(),
        },
    }
}

fn for_workload(cfg: &Config, report: &WorkloadReport) -> ScenarioText {
    let controls = &cfg.controls;
    let connections = if controls.max_connections == 0 {
        "--max-connections 0".to_string()
    } else {
        format!("--max-connections {}", controls.max_connections)
    };
    let workspace = if controls.concurrent_logical_workspace_limit == 0 {
        "admission off".to_string()
    } else {
        format!(
            "--concurrent-logical-workspace-limit {}",
            format_mem(controls.concurrent_logical_workspace_limit)
        )
    };
    let index = match report.index_mode {
        IndexMode::Indexed => "indexed",
        IndexMode::Scan => "scan",
        IndexMode::Both => "both",
    };
    let reads = match controls.read_query {
        ReadQuery::Mix => "mix",
        ReadQuery::DeepPaths => "deep-paths",
        ReadQuery::SizeSort => "size-sort",
    };
    let then = match (
        controls.max_connections == 0,
        controls.concurrent_logical_workspace_limit == 0,
    ) {
        (true, true) => "every connection is accepted and no query is limited by the workspace pool. A write-write conflict counts as failed. Any other error fails the run.".into(),
        (true, false) => "every connection is accepted. A query that does not fit the workspace pool counts as limited. A write-write conflict counts as failed. Any other error fails the run.".into(),
        (false, true) => format!(
            "connections past {} are refused and counted. No query is limited by the workspace pool. A write-write conflict counts as failed. Any other error fails the run.",
            controls.max_connections
        ),
        (false, false) => format!(
            "connections past {} are refused and counted. A query that does not fit the workspace pool counts as limited. A write-write conflict counts as failed. Any other error fails the run.",
            controls.max_connections
        ),
    };
    ScenarioText {
        given: format!(
            "a ledger at graph scale {} loaded with the limits off, then reopened with {connections} and {workspace}.",
            controls.graph_scale
        ),
        when: format!(
            "{} warmup operations run uncounted. Then {} Bolt clients share {} operations at {}, reads {reads}, indexes {index}.",
            controls.warmup_ops, controls.workers, controls.ops, controls.read_write_ratio()
        ),
        then,
        outcome: format!(
            "reads succeeded {}, limited {}, failed {}; writes succeeded {}, limited {}, failed {}; connection refusals {}; workspace refusals {}.",
            report.reads_succeeded,
            report.reads_limited,
            report.reads_failed,
            report.writes_succeeded,
            report.writes_limited,
            report.writes_failed,
            report.connections_refused,
            report.active_query_refusals,
        ),
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text).trim()
}

fn failure_summary(text: &str) -> String {
    if let Some(start) = text.find("resource limit exceeded:") {
        let rest = &text[start..];
        let end = rest.find('"').unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    let error = first_line(text).split(" — ").next().unwrap_or_default();
    let error = error.strip_prefix("RUN failed: ").unwrap_or(error);
    error.trim().to_string()
}
