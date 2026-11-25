//! One whole sandbox differential run (ADR-0144): resolve the dataset
//! selection, fetch the dumps, boot the Neo4j reference with them
//! pre-loaded, run the selected datasets under bounded concurrency, and
//! fold every outcome into one record list (F-1-6). The docker-gated
//! integration test and the A/B driver bin both call [`run`], so the
//! orchestration exists once.

use std::path::{Path, PathBuf};

use crate::config;
use crate::docker;
use crate::manifest::Dataset;
use crate::record::{Outcome, QueryResult, Summary};
use crate::{cache, suite};

/// One sandbox run's outcome: every per-statement record (datasets in
/// manifest order, statements in run order) and the one [`Summary`] fold
/// over them. The integration test renders report artifacts and the
/// verdict from it; the A/B driver prints its per-statement diff lines
/// from [`Self::records`].
pub struct SandboxSummary {
    pub records: Vec<QueryResult>,
    pub summary: Summary,
    /// The reference container, still running: carried OUT of [`run`] so
    /// the caller's ADR-0204 log capture happens before the drop that
    /// removes the container. The A/B driver ignores it.
    pub reference: docker::DatasetTest,
}

impl SandboxSummary {
    /// The number of records — one per statement, plus one per
    /// dataset-level setup failure.
    pub fn total(&self) -> usize {
        self.records.len()
    }

    /// Whether any record carries an infrastructure failure (ADR-0176) —
    /// the only outcome that fails a run.
    pub fn infra_failed(&self) -> bool {
        self.summary.setup_failed > 0
    }
}

/// Run the sandbox differential suite over `names` — manifest dataset
/// names in any order; an empty list selects every non-opt-in dataset, and
/// naming an opt-in dataset runs it ([`config::parse_dataset_filter`], the
/// one selection policy). `attach` is applied to every pulled copy
/// (ADR-0665). Requires docker and `RELATE_BIN`.
pub fn run(
    names: &[&str],
    attach: Option<relate_test_support::AttachHook>,
) -> Result<SandboxSummary, String> {
    if !docker::docker_available() {
        return Err("docker is not available (the sandbox suite boots a Neo4j reference)".into());
    }
    if relate_test_support::relate_bin().is_none() {
        return Err(relate_test_support::relate_bin_missing_message());
    }
    let filter = names.join(",");
    let datasets = config::parse_dataset_filter(
        "sandbox datasets",
        (!names.is_empty()).then_some(filter.as_str()),
        |_| false,
    )?;
    let workdir = cache::cache_root();
    std::fs::create_dir_all(&workdir).map_err(|e| format!("workdir {}: {e}", workdir.display()))?;
    let reference = boot_reference(&datasets)?;
    // The apoc capture is a report knob (ADR-0144): read from the same env
    // the report renderer reads, so the run and its report agree.
    let apoc_enabled = config::env_flag("RELATE_GRAPH_EXAMPLES_APOC");
    let outcomes = run_datasets_bounded(&workdir, &reference, &datasets, apoc_enabled, attach);
    let records = fold_outcomes(outcomes);
    let summary = Summary::of(&records);
    Ok(SandboxSummary {
        records,
        summary,
        reference,
    })
}

/// Fetch (or verify from cache) every selected dump, then boot the reference
/// container with all of them pre-loaded: the reference volume is loaded
/// before the server boots (loading under a running server quarantines the
/// store). A boot failure panics at the caller (ADR-0176 kind 2: with no
/// reference, no result is interpretable); the reference log is captured
/// first, because the cleanup guard removes the container on unwind.
fn boot_reference(datasets: &[&'static Dataset]) -> Result<docker::DatasetTest, String> {
    let mut dumps: Vec<(String, PathBuf)> = Vec::new();
    for &dataset in datasets {
        let path =
            cache::ensure_dump(dataset, true).map_err(|e| format!("{} dump: {e}", dataset.name))?;
        dumps.push((dataset.name.to_string(), path));
    }
    docker::start_with_dumps(&dumps).map_err(|e| {
        // ADR-0204: capture whatever the half-booted reference logged
        // before the error's unwind removes the container (the armed
        // CleanupGuard's Drop), or the workflow's capture step finds
        // nothing.
        docker::capture_reference_logs(&cache::cache_root());
        format!("reference container: {e}")
    })
}

/// Run every selected dataset with bounded concurrency. The datasets are
/// independent (each has its own reference database, pulled store, and
/// destination), but every one drives the same reference container —
/// running all of them at once starves it (and its heap) on the larger
/// sandbox sets. One dataset's infrastructure error becomes that dataset's
/// `Err` entry; the others' results are kept.
fn run_datasets_bounded(
    workdir: &Path,
    reference: &docker::DatasetTest,
    datasets: &[&'static Dataset],
    apoc_enabled: bool,
    attach: Option<relate_test_support::AttachHook>,
) -> Vec<(String, Result<Vec<QueryResult>, String>)> {
    // Default 4 assumes a release build (the nightly workflow builds
    // --release): a debug build's slower ingest under 4-way dataset
    // concurrency can stall a pull's read loop past the server's
    // read-stall window (~400-500 s, measured). Docs: docs/datasets.md
    // env table.
    let width = config::env_or("RELATE_GRAPH_EXAMPLES_CONCURRENCY", 4) as usize;
    work_queue(datasets, width, |&dataset| {
        eprintln!("{}: running", dataset.name);
        let out = suite::run_dataset(
            reference,
            dataset,
            workdir,
            // Named fields (F-1-13): the two flags cannot be swapped
            // silently the way adjacent bare bools can.
            suite::RunOptions {
                fetch: true,
                apoc_enabled,
                attach,
            },
        );
        eprintln!(
            "{}: {}",
            dataset.name,
            match &out {
                Ok(r) => format!("{} statements done", r.len()),
                Err(e) => format!("setup failed: {e}"),
            }
        );
        (dataset.name.to_string(), out)
    })
}

/// Shared-index work queue: `width` scoped workers pull the next item index
/// from one atomic counter, so a worker that finishes picks up the next item
/// immediately — one straggler never idles the other slots behind a wave
/// barrier the way `chunks(width)` did (F-1-3). Results come back in input
/// order (each carries its index and the sorted merge restores it), so a
/// consumer that diffs two runs sees the same line order in both.
fn work_queue<T: Sync, R: Send>(
    items: &[T],
    width: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..width.min(items.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut mine: Vec<(usize, R)> = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        mine.push((index, work(item)));
                    }
                    mine
                })
            })
            .collect();
        let mut done: Vec<(usize, R)> = workers
            .into_iter()
            .flat_map(|h| h.join().expect("worker panicked"))
            .collect();
        done.sort_unstable_by_key(|(index, _)| *index);
        done.into_iter().map(|(_, result)| result).collect()
    })
}

/// Fold per-dataset outcomes into the run's record list, in the input
/// order [`work_queue`] preserves (which is manifest order). A dataset-level
/// infrastructure error (ADR-0176 kinds 1 and 4) becomes one setup-failed
/// record, so one bad dataset loses none of the others' results (and the
/// report still gets written). The setup-failed record carries only real
/// data (F-1-4): no adoc line and no statement exist for it, so both are
/// `None` — the error text is the outcome's.
fn fold_outcomes(outcomes: Vec<(String, Result<Vec<QueryResult>, String>)>) -> Vec<QueryResult> {
    let mut all = Vec::new();
    for (name, result) in outcomes {
        match result {
            Ok(results) => all.extend(results),
            // A dataset-level failure is one record built through the
            // record model's statement-less constructor: no line, no
            // statement, no engine run (F-2-2).
            Err(e) => all.push(QueryResult::dataset_level(&name, Outcome::SetupFailed(e))),
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The queue runs every item exactly once, in input order, however the
    /// workers interleave.
    #[test]
    fn work_queue_processes_every_item_exactly_once_in_input_order() {
        let items: Vec<usize> = (0..100).collect();
        let out = work_queue(&items, 4, |&i| i * 2);
        let expected: Vec<usize> = items.iter().map(|i| i * 2).collect();
        assert_eq!(out, expected);
    }

    /// Degenerate shapes: more workers than items, and no items at all.
    #[test]
    fn work_queue_handles_degenerate_shapes() {
        assert_eq!(work_queue(&[1, 2], 8, |&i| i), [1, 2]);
        let empty: Vec<usize> = Vec::new();
        assert!(work_queue(&empty, 4, |&i| i).is_empty());
    }

    /// F-1-3: no wave barrier. Item 0 is a straggler that completes only once
    /// every other item has been processed by the other worker; under
    /// `chunks(width)` waves those items would sit behind the barrier and this
    /// test would hit its timeout instead.
    #[test]
    fn work_queue_has_no_wave_barrier() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        let others_done = AtomicUsize::new(0);
        let items: Vec<usize> = (0..5).collect();
        let out = work_queue(&items, 2, |&i| {
            if i == 0 {
                let start = Instant::now();
                while others_done.load(Ordering::SeqCst) < 4 {
                    assert!(
                        start.elapsed() < Duration::from_secs(30),
                        "wave barrier: the straggler's wave never releases the other items"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            } else {
                others_done.fetch_add(1, Ordering::SeqCst);
            }
            i
        });
        assert_eq!(out, items);
    }

    /// F-1-4: a dataset setup failure becomes one setup-failed record carrying
    /// only real data — no fabricated adoc line, no fabricated statement; the
    /// error text is the outcome's. Successful datasets keep their records.
    #[test]
    fn fold_outcomes_records_setup_failures_without_fake_fields() {
        let ok = QueryResult {
            dataset: "movies".to_string(),
            line: Some(3),
            statement: Some("MATCH (n) RETURN n".to_string()),
            outcome: Outcome::Pass,
            engine_ms: None,
            apoc: false,
        };
        let all = fold_outcomes(vec![
            ("movies".to_string(), Ok(vec![ok])),
            ("pole".to_string(), Err("pull failed".to_string())),
        ]);
        assert_eq!(all.len(), 2);
        assert!(matches!(all[0].outcome, Outcome::Pass));
        let setup = &all[1];
        assert_eq!(setup.dataset, "pole");
        assert_eq!(setup.line, None);
        assert_eq!(setup.statement, None);
        assert!(
            matches!(&setup.outcome, Outcome::SetupFailed(e) if e == "pull failed"),
            "{:?}",
            setup.outcome
        );
    }
}
