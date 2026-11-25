//! The differential run (ADR-0144). Opt-in via `RELATE_GRAPH_EXAMPLES=1`
//! (the nightly workflow sets it); without it the test self-skips so
//! regular `cargo test` never boots docker or downloads dumps.
//!
//! Requires: docker (reference container), `RELATE_BIN` (the built CLI),
//! network (dumps/adocs on first run, docker image). Dumps are cached and
//! sha-pinned in `target/graph-examples/` (override with
//! `RELATE_GRAPH_EXAMPLES_CACHE`). `RELATE_GRAPH_EXAMPLES_DATASETS`
//! (comma-separated manifest names) runs a subset, in manifest order; an
//! unknown name fails before any docker command. Opt-in datasets (twitch —
//! 465 MB dump) are excluded from the default run and run only when their
//! flag is `1` (`RELATE_GRAPH_EXAMPLES_TWITCH=1`) or when named in the
//! filter: naming them is an explicit opt-in.
//!
//! The run is report-only (ADR-0176): DIFF, order-only, skip and
//! inconclusive reference-timebox outcomes are reported, never gated on.
//! Only infrastructure failures fail the run — a dataset setup failure
//! (kind 1, and kind 4's no-runnable-statements error), a pull-verification
//! count mismatch (kind 3), and a reference-boot failure (kind 2, the panic
//! in [`boot_reference`]). Kinds 1, 3 and 4 all surface as the
//! [`record::Outcome::SetupFailed`] variant, which [`verdict`] gates on.

use std::path::{Path, PathBuf};

use relate_graph_examples::{cache, config, docker, manifest, record, report, sandbox};

fn main_gate() -> bool {
    config::env_flag("RELATE_GRAPH_EXAMPLES")
}

fn apoc_enabled() -> bool {
    config::env_flag("RELATE_GRAPH_EXAMPLES_APOC")
}

/// Preflight gates. The suite self-skips unless `RELATE_GRAPH_EXAMPLES=1`;
/// a gated run without docker or the CLI binary fails loudly. Returns the
/// selected datasets: `RELATE_GRAPH_EXAMPLES_DATASETS` (comma-separated
/// manifest names) restricts the run to a subset, resolved before any
/// docker command — an unknown name panics naming the value and listing
/// the valid names.
fn preflight() -> Option<Vec<&'static manifest::Dataset>> {
    if !main_gate() {
        eprintln!(
            "skipping: set RELATE_GRAPH_EXAMPLES=1, RELATE_BIN=<built cli>, \
             and ensure docker to run the graph-examples differential suite"
        );
        return None;
    }
    let datasets =
        config::dataset_filter("RELATE_GRAPH_EXAMPLES_DATASETS").unwrap_or_else(|e| panic!("{e}"));
    for &dataset in &datasets {
        if let Some(flag) = dataset.opt_in_env {
            eprintln!(
                "{}: opted in ({flag}=1 or named in the dataset filter)",
                dataset.name
            );
        }
    }
    for dataset in manifest::DATASETS
        .iter()
        .filter(|d| d.opt_in_env.is_some() && !datasets.iter().any(|&s| s.name == d.name))
    {
        eprintln!(
            "{}: skipped by default; set {}=1 to include it",
            dataset.name,
            dataset.opt_in_env.unwrap_or_default()
        );
    }
    if !docker::docker_available() {
        panic!("RELATE_GRAPH_EXAMPLES=1 but docker is not available");
    }
    if relate_test_support::relate_bin().is_none() {
        panic!("{}", relate_test_support::relate_bin_missing_message());
    }
    Some(datasets)
}

/// Write the run's report artifacts for the nightly upload — the full
/// report and, when the apoc capture renderer has apoc-classified records
/// to render (F-1-11: the renderer owns that decision), the apoc capture —
/// and return the full report's path.
fn write_reports(
    workdir: &Path,
    all_results: &[record::QueryResult],
    summary: &record::Summary,
    apoc_enabled: bool,
) -> PathBuf {
    let report_path = workdir.join("report.txt");
    std::fs::write(&report_path, report::render_run(all_results, summary)).expect("writing report");
    eprintln!("{} (report: {})", summary.line(), report_path.display());
    append_step_summary(all_results, summary);

    if let Some(capture) = report::render_apoc_capture(all_results, apoc_enabled) {
        let apoc_path = workdir.join("apoc-report.txt");
        std::fs::write(&apoc_path, capture).expect("writing apoc report");
        eprintln!("apoc capture written (report: {})", apoc_path.display());
    }
    report_path
}

/// Append the markdown run summary (summary line + per-dataset rollup) to
/// GitHub's step summary file. Silent no-op when `GITHUB_STEP_SUMMARY` is
/// unset (local runs); an unwritable file warns, never fails the run.
fn append_step_summary(all_results: &[record::QueryResult], summary: &record::Summary) {
    let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") else {
        return;
    };
    let markdown = report::render_summary_markdown(all_results, summary);
    let result = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, markdown.as_bytes()));
    if let Err(e) = result {
        eprintln!("warning: could not append to GITHUB_STEP_SUMMARY {path}: {e}");
    }
}

/// The verdict (ADR-0176): the run is report-only, so it fails if and only
/// if a record carries an infrastructure failure — the setup-failed variant
/// (kinds 1, 3 and 4; kind 2 is the boot panic above). DIFF, order-only,
/// skip and inconclusive records are reported, never fatal. The panic names
/// each failed dataset and its error.
fn verdict(all_results: &[record::QueryResult], report_path: &Path) {
    let infra_failures: Vec<(&str, &str)> = all_results
        .iter()
        .filter_map(|r| match &r.outcome {
            record::Outcome::SetupFailed(why) => Some((r.dataset.as_str(), why.as_str())),
            _ => None,
        })
        .collect();
    if infra_failures.is_empty() {
        return;
    }
    // ADR-0204: capture the reference container's logs BEFORE the panic —
    // the unwind drops the DatasetTest, which removes the container, and
    // the workflow's own capture step then finds nothing. The files land
    // next to report.txt, inside the workflow's existing `reference-*.log`
    // upload glob.
    docker::capture_reference_logs(
        report_path
            .parent()
            .expect("the report always lives in the workdir"),
    );
    panic!(
        "{} infrastructure failures (ADR-0176):\n  {}\nreport: {}",
        infra_failures.len(),
        infra_failures
            .iter()
            .map(|(dataset, why)| format!("{dataset}: {why}"))
            .collect::<Vec<_>>()
            .join("\n  "),
        report_path.display()
    );
}

#[test]
fn graph_examples_differential_suite() {
    let Some(datasets) = preflight() else {
        return;
    };
    let names: Vec<&str> = datasets.iter().map(|d| d.name).collect();
    // A dump or reference-boot failure is the run's error (ADR-0176 kind
    // 2): with no reference, no result is interpretable.
    let run = sandbox::run(&names, None).unwrap_or_else(|e| panic!("{e}"));
    let workdir = cache::cache_root();
    let apoc_enabled = apoc_enabled();
    let report_path = write_reports(&workdir, &run.records, &run.summary, apoc_enabled);

    verdict(&run.records, &report_path);
}
