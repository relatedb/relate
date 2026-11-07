use std::io::{self, Write};
use std::path::PathBuf;

use clap::Parser;
use relate_datasets::harness::default_bolt_bin;
use relate_datasets::memory_controls::harness::{default_store, run, CaseReport, Config};
use relate_datasets::memory_controls::workload::{
    parse_index_mode, parse_ratio, parse_read_query, Controls, IndexMode, ReadQuery, WorkloadReport,
};

#[derive(Parser, Debug)]
#[command(
    name = "memory-controls",
    about = "Verify Relate memory controls and calibrate workspace memory"
)]
struct Args {
    /// Directory for this run. Each case uses a subdirectory.
    #[arg(long)]
    uri: Option<PathBuf>,
    /// Path to the relate CLI (`serve`). Default: sibling of this
    /// executable, then target/release/relate.
    #[arg(long)]
    serve_bin: Option<PathBuf>,
    /// Report JSON path
    #[arg(long, default_value = "target/memory-controls.json")]
    out: PathBuf,
    /// Multiplier for ledger width. Depth stays at 8 until scale 5.
    #[arg(long, default_value_t = 1)]
    graph_scale: u64,
    /// Concurrent Bolt clients. Each keeps one connection.
    #[arg(long, default_value_t = 4)]
    workers: u64,
    /// Read and write parts, for example 80:20.
    #[arg(long, default_value = "80:20")]
    read_write_ratio: String,
    /// Shared connection cap. 0 is unlimited.
    #[arg(long, default_value_t = 0)]
    max_connections: u64,
    /// Concurrent logical workspace bytes. 0 leaves admission off.
    #[arg(long, default_value_t = 0)]
    concurrent_logical_workspace_limit: u64,
    /// Measured operations after warmup.
    #[arg(long, default_value_t = 40)]
    ops: u64,
    /// Operations run before measurement. Not counted.
    #[arg(long, default_value_t = 8)]
    warmup_ops: u64,
    /// Seed for the operation shuffle.
    #[arg(long, default_value_t = 1)]
    workload_seed: u64,
    /// indexed, scan, or both.
    #[arg(long, default_value = "both")]
    index_mode: String,
    /// mix, deep-paths, or size-sort. mix rotates the six read statements.
    #[arg(long, default_value = "mix")]
    read_query: String,
    /// Run the configured workload only.
    #[arg(long, default_value_t = false)]
    skip_controls: bool,
    /// One case: connection-cap, unlimited-connections,
    /// concurrent-logical-workspace-limit, workspace-memory-calibration, or workload.
    #[arg(long)]
    case: Option<String>,
    /// Repetitions for each logical-byte calibration shape.
    #[arg(long, default_value_t = 3)]
    calibration_repeats: u32,
    /// Milliseconds to hold each calibrated workspace at its live peak.
    #[arg(long, default_value_t = 2500)]
    calibration_hold_ms: u64,
    /// Restrict logical-byte calibration to one workspace family.
    #[arg(long)]
    calibration_family: Option<String>,
}

fn format_mem(bytes: u64) -> String {
    relate_datasets::memory_controls::format_mem(bytes)
}

/// Split prose into one sentence per entry, keeping each full stop.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while let Some(index) = rest.find(". ") {
        out.push(rest[..=index].to_string());
        rest = rest[index + 2..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// One labelled block, one sentence per line. Continuation lines are indented
/// under the text, not under the label.
fn print_block(label: &str, text: &str) {
    let indent = " ".repeat(9);
    for (index, sentence) in sentences(text).iter().enumerate() {
        if index == 0 {
            println!("{label:<8} {sentence}");
        } else {
            println!("{indent}{sentence}");
        }
    }
}

fn print_gwt(scenario: &relate_datasets::memory_controls::harness::ScenarioText) {
    print_block("Given", &scenario.given);
    print_block("When", &scenario.when);
    print_block("Then", &scenario.then);
    print_block("Outcome", &scenario.outcome);
    println!();
}

fn print_rss(case: &CaseReport) {
    let graph = case.baseline_rss_bytes.saturating_add(case.floor_rss_bytes);
    let operating_min = graph.saturating_add(case.processing_rss_min_bytes);
    let operating_max = graph.saturating_add(case.processing_rss_max_bytes);
    line("Operating RSS", &format_range(operating_min, operating_max));
    line("├── Graph Logical", &format_mem(case.graph_logical_bytes));
    line("├── Graph RSS", &format_mem(graph));
    line(
        "└── Processing Memory (RSS)",
        &format_range(case.processing_rss_min_bytes, case.processing_rss_max_bytes),
    );
    line("    ├── Min", &format_mem(case.processing_rss_min_bytes));
    line("    └── Max", &format_mem(case.processing_rss_max_bytes));
    if let (Some(logical_bytes), Some(retained), Some(per)) = (
        case.peak_logical_bytes_held,
        case.retained_rss_bytes,
        case.rss_bytes_per_logical_byte,
    ) {
        line("Peak logical bytes held", &format_mem(logical_bytes));
        line("Retained RSS", &format_mem(retained));
        line("RSS bytes per logical byte", &format!("{per} B"));
    }
}

fn print_case(case: &CaseReport) {
    println!("{}", case.name);
    print_gwt(&case.scenario);
    println!("Memory");
    print_rss(case);
}

/// The observation whose ratio is the one the family gate stores.
fn peak_ratio<'a>(
    observations: &'a [relate_datasets::memory_controls::calibration::PeakCalibrationObservation],
    family: &str,
    cgroup: bool,
) -> Option<&'a relate_datasets::memory_controls::calibration::PeakCalibrationObservation> {
    observations
        .iter()
        .filter(|row| family == "overall" || row.family == family)
        .filter(|row| !cgroup || row.cgroup_to_logical_ratio.is_some())
        .max_by(|left, right| {
            let left_ratio = if cgroup {
                left.cgroup_to_logical_ratio.unwrap_or(0.0)
            } else {
                left.rss_to_logical_ratio
            };
            let right_ratio = if cgroup {
                right.cgroup_to_logical_ratio.unwrap_or(0.0)
            } else {
                right.rss_to_logical_ratio
            };
            left_ratio.total_cmp(&right_ratio)
        })
}

fn line(label: &str, value: &str) {
    println!("{label:<36}{value:>22}");
}

fn format_range(min: u64, max: u64) -> String {
    if min == max {
        format_mem(min)
    } else {
        format!("{} – {}", format_mem(min).trim(), format_mem(max).trim())
    }
}

fn print_workload(workload: &WorkloadReport) {
    println!("{}", workload.case.name);
    print_gwt(&workload.scenario);
    println!("Memory");
    print_rss(&workload.case);
    println!(
        "reads  succeeded {}  limited {}  failed {}",
        workload.reads_succeeded, workload.reads_limited, workload.reads_failed
    );
    println!(
        "writes  succeeded {}  limited {}  failed {}",
        workload.writes_succeeded, workload.writes_limited, workload.writes_failed
    );
    println!(
        "refusals  connections {}  active-queries {}",
        workload.connections_refused, workload.active_query_refusals,
    );
}

fn emit_case(_cfg: &Config, case: &CaseReport) {
    print_case(case);
    println!();
    let _ = io::stdout().flush();
}

fn emit_workload(_cfg: &Config, workload: &WorkloadReport) {
    print_workload(workload);
    println!();
    let _ = io::stdout().flush();
}

fn controls_from(args: &Args) -> Result<Controls, String> {
    let (read_parts, write_parts) = parse_ratio(&args.read_write_ratio)?;
    let index_mode = parse_index_mode(&args.index_mode)?;
    let read_query = parse_read_query(&args.read_query)?;
    let controls = Controls {
        graph_scale: args.graph_scale,
        workers: args.workers,
        read_parts,
        write_parts,
        max_connections: args.max_connections,
        concurrent_logical_workspace_limit: args.concurrent_logical_workspace_limit,
        ops: args.ops,
        warmup_ops: args.warmup_ops,
        seed: args.workload_seed,
        index_mode,
        read_query,
    };
    controls.validate()?;
    Ok(controls)
}

fn main() {
    let args = Args::parse();
    let controls = match controls_from(&args) {
        Ok(controls) => controls,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    let cfg = Config {
        serve_bin: args.serve_bin.clone().unwrap_or_else(default_bolt_bin),
        store: args.uri.clone().unwrap_or_else(default_store),
        out: args.out.clone(),
        controls: controls.clone(),
        skip_controls: args.skip_controls,
        case: args.case.clone(),
        calibration_repeats: args.calibration_repeats,
        calibration_hold_ms: args.calibration_hold_ms,
        calibration_family: args.calibration_family.clone(),
    };
    println!(
        "scale {}  workers {}  ratio {}  index {:?}  ops {}  warmup {}",
        controls.graph_scale,
        controls.workers,
        controls.read_write_ratio(),
        match controls.index_mode {
            IndexMode::Indexed => "indexed",
            IndexMode::Scan => "scan",
            IndexMode::Both => "both",
        },
        controls.ops,
        controls.warmup_ops,
    );
    println!(
        "reads  {}",
        match controls.read_query {
            ReadQuery::Mix => "mix",
            ReadQuery::DeepPaths => "deep-paths",
            ReadQuery::SizeSort => "size-sort",
        }
    );
    println!(
        "limits  connections {}  logical bytes {}",
        if controls.max_connections == 0 {
            "unlimited".to_string()
        } else {
            controls.max_connections.to_string()
        },
        if controls.concurrent_logical_workspace_limit == 0 {
            "unlimited".to_string()
        } else {
            format_mem(controls.concurrent_logical_workspace_limit)
        },
    );
    println!();
    let _ = io::stdout().flush();
    match run(
        &cfg,
        |case| emit_case(&cfg, case),
        |workload| emit_workload(&cfg, workload),
    ) {
        Ok(report) => {
            if let Some(calibration) = &report.workspace_memory_calibration {
                println!("workspace memory calibration");
                println!("Outcome  {} observations.", calibration.observations.len());
                for summary in &calibration.summaries {
                    let rss = peak_ratio(&calibration.observations, &summary.family, false);
                    let retained = rss
                        .map(|row| format_mem(row.rss_delta_bytes))
                        .unwrap_or_else(|| "-".into());
                    let mut outcome = format!(
                        "  {} retained {retained}, ratio {:.3}x.",
                        summary.family, summary.max_rss_to_logical_ratio
                    );
                    if let Some(row) = peak_ratio(&calibration.observations, &summary.family, true)
                    {
                        if let (Some(bytes), Some(ratio)) =
                            (row.cgroup_delta_bytes, row.cgroup_to_logical_ratio)
                        {
                            outcome.push_str(&format!(
                                " Cgroup retained {}, ratio {ratio:.3}x.",
                                format_mem(bytes)
                            ));
                        }
                    }
                    println!("{outcome}");
                }
            }
            if let Some(peak) = report.cgroup_memory_peak_bytes {
                println!("cgroup memory.peak  {}", format_mem(peak));
            }
            println!("wrote {}", args.out.display());
            let _ = io::stdout().flush();
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
