//! One whole run: data set, graph, queries, report. The `query-latency run`
//! command and the gated integration test both drive this; `query-latency
//! profile` shares [`open_graph`] and [`connect`], and the corpus lanes'
//! graph handle routes its connect step through [`connect`] too.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use relate::{Connection, Database, DurabilityMode, OpenMode, OpenOptions};
use relate_test_support::sampling::process_peak_rss_bytes;
use serde::{Deserialize, Serialize};

use crate::fetch::ensure_dataset;
use crate::load::{load, LoadReport};
use crate::report::{
    build_kind, git_state, started_unix, ConfigReport, Report, StoreOrigin, SCHEMA,
};
use crate::run::{run_query, RunConfig, Watchdog};
use crate::workload::{expected, query, IndexProfile, ScaleFactor};
use crate::{remove_if_present, Result};

/// Which graph a run queries and where it comes from.
#[derive(Debug, Clone)]
pub struct GraphOptions {
    pub sf: ScaleFactor,
    pub profile: IndexProfile,
    /// `None`: load into `:memory:`. `Some(path)`: reuse the store at
    /// `path` (opened read-only) or, when it does not exist, load into it
    /// and checkpoint it for the next run.
    pub store: Option<PathBuf>,
    /// The data cache (see [`crate::fetch::data_root`]).
    pub data_root: PathBuf,
    /// Download the data set when it is not cached.
    pub download: bool,
    /// An experimental property-store attach hook (ADR-0665), run once after
    /// the graph is loaded. `None` runs the default store; the Vortex A/B
    /// driver in `testing/relate-vortex-lab` passes its own attach.
    pub attach: Option<crate::AttachHook>,
}

impl Default for GraphOptions {
    /// The suite's ordinary graph: SF0.1, upstream indexes, the in-memory
    /// store and the default data cache, downloading when uncached, with no
    /// attach hook — so a caller overrides only what its run changes and
    /// never restates the hook.
    fn default() -> Self {
        Self {
            sf: ScaleFactor::Sf0_1,
            profile: IndexProfile::Upstream,
            store: None,
            data_root: crate::fetch::data_root(),
            download: true,
            attach: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SuiteOptions {
    pub graph: GraphOptions,
    /// Queries to run, in order.
    pub qids: Vec<u32>,
    pub run: RunConfig,
}

/// The run suite's default report file name,
/// `report-graph-benchmark-ldbc-sf<SF>.json` — derived from the one
/// spelling of the workload's name ([`crate::workload::NAME`]), mirroring
/// `Lane::report_name` beside `run_corpus`.
pub fn report_name(sf: ScaleFactor) -> String {
    format!("report-{}-sf{sf}.json", crate::workload::NAME)
}

/// Run the suite and return its report. Query failures are recorded in the
/// report, not returned as errors; an `Err` means the run itself could not
/// happen (data, load, store).
pub fn run_suite(opts: &SuiteOptions) -> Result<Report> {
    let started_unix = started_unix();
    let expected = expected(opts.graph.sf)?;
    let queries = opts
        .qids
        .iter()
        .map(|&q| query(q, opts.graph.sf))
        .collect::<Result<Vec<_>>>()?;

    let (db, graph) = open_graph(&opts.graph)?;
    let mut conn = connect("the run's store", &db)?;

    let watchdog = Watchdog::new();
    let mut outcomes = Vec::with_capacity(queries.len());
    for q in &queries {
        let o = run_query(&mut conn, q, expected.get(&q.qid), &opts.run, &watchdog);
        eprintln!(
            "query-latency: q{:<2} {:<7} {:>6} rounds  p50 {:>10.3} ms",
            q.qid,
            o.status,
            o.rounds,
            o.engine.p50_ns as f64 / 1e6
        );
        outcomes.push(o);
    }
    drop(conn);
    drop(db);

    Ok(Report {
        schema: SCHEMA,
        suite: "query-latency",
        workload: crate::workload::NAME,
        sf: opts.graph.sf.as_str().into(),
        index_profile: opts.graph.profile.as_str().into(),
        started_unix,
        engine_version: env!("CARGO_PKG_VERSION"),
        build: build_kind(),
        git: git_state(),
        config: ConfigReport {
            min_rounds: opts.run.min_rounds,
            min_time_s: opts.run.min_time.as_secs_f64(),
            timeout_s: opts.run.timeout.as_secs_f64(),
            max_rounds: opts.run.max_rounds,
        },
        graph,
        peak_rss_bytes: process_peak_rss_bytes(),
        queries: outcomes,
    })
}

/// Open the graph `opts.store` asks for: an existing, completed store is
/// reused read-only; otherwise the data set is loaded into `:memory:` or
/// into a new store at the path, which is checkpointed and marked complete
/// for the next run.
pub fn open_graph(opts: &GraphOptions) -> Result<(Database, StoreOrigin)> {
    let opened = if let Some(path) = opts.store.as_ref().filter(|p| p.exists()) {
        reuse_store(path, opts)?
    } else {
        let dataset = ensure_dataset(&opts.data_root, opts.sf, opts.download)?;
        match &opts.store {
            Some(path) => build_store(path, &dataset, opts)?,
            None => {
                let db = Database::open(":memory:").map_err(|e| e.to_string())?;
                let load = load_into(&db, &dataset, opts)?;
                let store = ":memory:".to_string();
                (db, StoreOrigin::Loaded { store, load })
            }
        }
    };
    if let Some(attach) = opts.attach {
        attach(&opened.0)?;
    }
    Ok(opened)
}

/// Connect `db` for statements — the one home of the connect step and
/// its error text: every caller names its step (the run's store, the
/// load into a new store, the profile loop's store, the corpus lanes'
/// graph handle), so a failed connect names what was being connected.
pub(crate) fn connect(step: &str, db: &Database) -> Result<Connection> {
    db.connect().map_err(|e| format!("{step}: connecting: {e}"))
}

/// What a completed store holds, written beside it once its checkpoint
/// succeeded: the store's existence alone does not mean it was built (the
/// path exists from the first write of a load that may still fail or be
/// killed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoreMarker {
    sf: String,
    index_profile: String,
    nodes: u64,
    relationships: u64,
}

/// `<store>.query-latency.json`.
fn store_marker(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.query-latency.json", path.display()))
}

fn reuse_store(path: &Path, opts: &GraphOptions) -> Result<(Database, StoreOrigin)> {
    let marker_path = store_marker(path);
    let text = std::fs::read_to_string(&marker_path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!(
            "{} exists but {} does not: it is not a completed query-latency store (a build \
             may have failed or been killed); delete it to rebuild",
            path.display(),
            marker_path.display()
        ),
        _ => format!("reading {}: {e}", marker_path.display()),
    })?;
    let marker: StoreMarker = serde_json::from_str(&text)
        .map_err(|e| format!("reading {}: {e}", marker_path.display()))?;
    if (marker.sf.as_str(), marker.index_profile.as_str())
        != (opts.sf.as_str(), opts.profile.as_str())
    {
        return Err(format!(
            "{} holds SF{} with {} indexes, not SF{} with {} indexes",
            path.display(),
            marker.sf,
            marker.index_profile,
            opts.sf,
            opts.profile.as_str()
        ));
    }
    eprintln!("query-latency: opening {} read-only", path.display());
    let t = Instant::now();
    let db = Database::open_with(
        utf8(path)?,
        OpenOptions {
            mode: OpenMode::ReadOnly,
            ..OpenOptions::default()
        },
    )
    .map_err(|e| format!("opening {}: {e}", path.display()))?;
    let origin = StoreOrigin::Reused {
        store: path.display().to_string(),
        open_seconds: t.elapsed().as_secs_f64(),
    };
    Ok((db, origin))
}

/// Load into a new store at `path`, checkpoint it, then mark it complete.
/// A failed build removes the store, which this run created.
fn build_store(
    path: &Path,
    dataset: &Path,
    opts: &GraphOptions,
) -> Result<(Database, StoreOrigin)> {
    let marker_path = store_marker(path);
    // A stale completed marker would certify the store this run is about
    // to rebuild mid-rebuild, so every removal failure except NotFound
    // (nothing to remove on a fresh build) is a loud build error — before
    // the store is touched.
    remove_if_present(&marker_path).map_err(|e| format!("the stale completed marker: {e}"))?;
    // A bulk build: commits are acknowledged after apply and only the
    // checkpoint after the load is made durable.
    let db = Database::open_with(
        utf8(path)?,
        OpenOptions {
            durability_mode: Some(DurabilityMode::Snapshot),
            checkpoint_interval: Some(Duration::ZERO),
            ..OpenOptions::default()
        },
    )
    .map_err(|e| format!("creating {}: {e}", path.display()))?;
    // The checkpoint and the marker are one commit: both succeed, or the
    // store this run created is removed (with any marker written).
    let built = load_into(&db, dataset, opts).and_then(|load| {
        db.checkpoint()
            .map_err(|e| format!("checkpointing {}: {e}", path.display()))?;
        let marker = StoreMarker {
            sf: opts.sf.as_str().into(),
            index_profile: opts.profile.as_str().into(),
            nodes: load.nodes,
            relationships: load.relationships,
        };
        let json = serde_json::to_string_pretty(&marker).map_err(|e| e.to_string())?;
        std::fs::write(&marker_path, json + "\n")
            .map_err(|e| format!("writing {}: {e}", marker_path.display()))?;
        Ok(load)
    });
    let load = match built {
        Ok(load) => load,
        Err(e) => {
            drop(db);
            // A surviving marker would certify the partial store (or a
            // later one at this path), so its removal failure is
            // reported beside the store note, never discarded.
            let marker_note = match remove_if_present(&marker_path) {
                Ok(()) => String::new(),
                Err(me) => format!(
                    "; the completed marker was not removed ({me}); delete it before the next run"
                ),
            };
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            let note = match removed {
                Ok(()) => "removed".to_string(),
                Err(re) => format!("not removed ({re}); delete it before the next run"),
            };
            return Err(format!(
                "{e} (the partial store at {} was {note}{marker_note})",
                path.display()
            ));
        }
    };
    let store = path.display().to_string();
    Ok((db, StoreOrigin::Loaded { store, load }))
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("the store path {} is not UTF-8", path.display()))
}

fn load_into(db: &Database, dataset: &Path, opts: &GraphOptions) -> Result<LoadReport> {
    eprintln!(
        "query-latency: loading {} ({} indexes)",
        dataset.display(),
        opts.profile.as_str()
    );
    let mut conn = connect("loading the data set", db)?;
    let report = load(
        &mut conn,
        dataset,
        opts.profile,
        Some(opts.sf.published_counts()),
    )?;
    eprintln!(
        "query-latency: loaded {} nodes, {} relationships in {:.1} s",
        report.nodes, report.relationships, report.seconds
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::pinned_sha256;

    fn options(root: &Path, store: PathBuf) -> GraphOptions {
        GraphOptions {
            sf: ScaleFactor::Sf0_1,
            profile: IndexProfile::Upstream,
            store: Some(store),
            data_root: root.to_path_buf(),
            download: false,
            ..GraphOptions::default()
        }
    }

    fn err(r: Result<(Database, StoreOrigin)>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    #[test]
    fn a_store_without_its_marker_is_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("graph-benchmark-ldbc.rl");
        std::fs::write(&store, b"half-built").unwrap();
        let e = err(open_graph(&options(dir.path(), store.clone())));
        assert!(e.contains("not a completed query-latency store"), "{e}");
        assert!(
            store.exists(),
            "a store this run did not create is left alone"
        );
    }

    #[test]
    fn an_unreadable_marker_is_reported_as_a_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("graph-benchmark-ldbc.rl");
        std::fs::write(&store, b"").unwrap();
        // A directory where the marker file should be: it exists, but
        // reading it fails with something other than NotFound.
        std::fs::create_dir(store_marker(&store)).unwrap();
        let e = err(open_graph(&options(dir.path(), store)));
        assert!(e.starts_with("reading "), "{e}");
        assert!(!e.contains("not a completed"), "{e}");
    }

    #[test]
    fn a_marker_for_another_scale_factor_or_profile_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("graph-benchmark-ldbc.rl");
        std::fs::write(&store, b"").unwrap();
        let marker = StoreMarker {
            sf: "1".into(),
            index_profile: "upstream".into(),
            nodes: 1,
            relationships: 1,
        };
        std::fs::write(
            store_marker(&store),
            serde_json::to_string(&marker).unwrap(),
        )
        .unwrap();
        let e = err(open_graph(&options(dir.path(), store)));
        assert!(
            e.contains("holds SF1 with upstream indexes, not SF0.1"),
            "{e}"
        );
    }

    #[test]
    fn a_failed_build_removes_the_store_it_created() {
        // A stamped but empty data set: the load fails at its first file.
        let dir = tempfile::tempdir().unwrap();
        let sf = ScaleFactor::Sf0_1;
        std::fs::create_dir_all(dir.path().join(sf.dataset_name())).unwrap();
        std::fs::write(
            dir.path().join(format!(".{}.ok", sf.dataset_name())),
            pinned_sha256(sf).unwrap(),
        )
        .unwrap();
        let store = dir.path().join("graph-benchmark-ldbc.rl");
        let e = err(open_graph(&options(dir.path(), store.clone())));
        assert!(e.contains("was removed"), "{e}");
        assert!(!store.exists());
        assert!(!store_marker(&store).exists());
    }
}
