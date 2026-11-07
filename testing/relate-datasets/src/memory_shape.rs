//! Out-of-process RSS of each graph shape.
//!
//! Each filled shape is loaded until the database reports the logical memory
//! limit. Logical bytes come from an in-process database on a filesystem
//! directory that is dropped before the server starts. Disk bytes are the
//! full size of that directory after checkpoint, including the WAL. RSS is
//! read from the `relate serve` child's `/proc` status, never from this
//! process. The Bolt child uses its own directory next to the measured one.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use relate_test_support::sampling::process_status;
use serde::{Deserialize, Serialize};

use crate::bolt::Session;
use crate::harness::default_bolt_bin;
use crate::report::mib;
use crate::serve_child::ServeChild;

mod progress;
use crate::shapes::{
    census, rss_matrix, FillCursor, GraphShape, IndexCounts, ShapeScale,
    DEFAULT_LOGICAL_TARGET_BYTES, DEFAULT_NODE_ATTRIBUTES,
};
use progress::{abbreviate, clear_active, format_elapsed, mark, progress, Heartbeat, Live};

pub const REPORT_SCHEMA_VERSION: u32 = 1;
const RSS_SAMPLES: usize = 5;
const SAMPLE_PAUSE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct MemoryShapeConfig {
    pub bolt_bin: PathBuf,
    /// Run directory. Each shape is stored at `<store>/<shape>`.
    pub store: PathBuf,
    pub out: Option<PathBuf>,
    /// Memory limit for each filled shape. MB in the CLI is 1024×1024 bytes.
    /// Loading stops when the database reports this limit.
    pub logical_target_bytes: u64,
    /// Integer properties on every node. The first is `id`.
    pub attributes: u32,
    /// Text, point, and range indexes applied to every filled shape.
    pub indexes: IndexCounts,
}

impl Default for MemoryShapeConfig {
    fn default() -> Self {
        Self {
            bolt_bin: default_bolt_bin(),
            store: default_store_uri(),
            out: None,
            logical_target_bytes: DEFAULT_LOGICAL_TARGET_BYTES,
            attributes: DEFAULT_NODE_ATTRIBUTES,
            indexes: IndexCounts::NONE,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShapeSample {
    pub shape: String,
    pub index: String,
    pub nodes: u64,
    pub relationships: u64,
    pub logical_bytes: u64,
    /// Full size of this shape's database directory after checkpoint.
    /// Every file is counted, including the WAL.
    pub disk_bytes: u64,
    pub rss_bytes: u64,
    pub peak_rss_bytes: u64,
    pub baseline_rss_bytes: u64,
    pub attributed_rss_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadKind {
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportConfig {
    pub scale: String,
    pub logical_target_bytes: u64,
    /// Integer properties created on every node.
    pub attributes: u32,
    /// String attributes per node, each with a TEXT index.
    pub text_indexes: u32,
    /// Point attributes per node, each with a POINT index.
    pub point_indexes: u32,
    /// Integer attributes per node that carry a RANGE index.
    pub range_indexes: u32,
    /// Run directory. Shape stores are `<uri>/<shape>`.
    pub uri: String,
    /// cgroup memory max of this process, when the run is inside the baseline
    /// container. Absent on a host run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryShapeReport {
    pub schema_version: u32,
    pub name: String,
    pub started_at: String,
    pub finished_at: String,
    pub workload: WorkloadKind,
    pub config: ReportConfig,
    pub shapes: Vec<ShapeSample>,
}

/// `/tmp/memory-shape-loadtest-<unix seconds>`. One directory per invocation.
pub fn default_store_uri() -> PathBuf {
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/memory-shape-loadtest-{run}"))
}

pub fn run(cfg: &MemoryShapeConfig) -> Result<MemoryShapeReport, String> {
    if process_status(std::process::id()).is_none() {
        return Err(
            "memory-shape RSS sampling needs Linux /proc/<pid>/status (VmRSS and VmHWM)".into(),
        );
    }
    let store_root = store_root(&cfg.store)?;
    if !cfg.bolt_bin.is_file() {
        return Err(format!(
            "relate serve binary not found at {} — build it before this loadtest",
            cfg.bolt_bin.display()
        ));
    }
    let scale = ShapeScale::Large {
        logical_bytes: cfg.logical_target_bytes,
    };
    crate::shapes::validate(cfg.attributes, cfg.indexes, scale)?;
    let matrix = rss_matrix();
    let total_shapes = matrix.len();
    let container = container_cgroup_memory_max();
    let container_label = container
        .map(|bytes| format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0)))
        .unwrap_or_else(|| "unset".into());
    progress(&format!(
        "memory-shape   {total_shapes} shapes   memory limit {:.0} MB   attributes {}   text {}   point {}   range {}   container {container_label}   uri {}",
        mib(cfg.logical_target_bytes),
        cfg.attributes,
        cfg.indexes.text,
        cfg.indexes.point,
        cfg.indexes.range,
        store_root.display()
    ));
    progress(
        "filling each shape until the database reports the limit; rss is the relate serve child",
    );
    let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let live = Arc::new(Mutex::new(Live::default()));
    let heartbeat = Heartbeat::start(Arc::clone(&live));
    let mut shapes = Vec::with_capacity(total_shapes);
    for (nth, shape) in matrix.into_iter().enumerate() {
        shapes.push(measure_shape(
            cfg,
            &store_root,
            shape,
            nth + 1,
            total_shapes,
            &live,
        )?);
    }
    drop(heartbeat);
    let baseline_rss = shapes
        .iter()
        .find(|sample| sample.shape == GraphShape::Base.as_str())
        .map(|sample| sample.rss_bytes)
        .ok_or_else(|| "base shape missing from the RSS matrix".to_string())?;
    for sample in &mut shapes {
        sample.baseline_rss_bytes = baseline_rss;
        sample.attributed_rss_bytes = sample.rss_bytes.saturating_sub(baseline_rss);
        sample.ratio = if sample.logical_bytes == 0 {
            None
        } else {
            Some(sample.attributed_rss_bytes as f64 / sample.logical_bytes as f64)
        };
    }
    let report = MemoryShapeReport {
        schema_version: REPORT_SCHEMA_VERSION,
        name: "memory-shape".into(),
        started_at,
        finished_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        workload: WorkloadKind {
            kind: "memory-shape".into(),
        },
        config: ReportConfig {
            scale: scale.as_str().into(),
            logical_target_bytes: cfg.logical_target_bytes,
            attributes: cfg.attributes,
            text_indexes: cfg.indexes.text,
            point_indexes: cfg.indexes.point,
            range_indexes: cfg.indexes.range,
            uri: store_root.display().to_string(),
            container_memory_bytes: container,
        },
        shapes,
    };
    if let Some(path) = &cfg.out {
        crate::report::write_report(path, &report)?;
    }
    Ok(report)
}

fn measure_shape(
    cfg: &MemoryShapeConfig,
    store_root: &Path,
    shape: GraphShape,
    nth: usize,
    total_shapes: usize,
    live: &Mutex<Live>,
) -> Result<ShapeSample, String> {
    let store = store_root.join(shape.to_string());
    let bolt_store = store_root.join(format!("{shape}-bolt"));
    let indexes = cfg.indexes;
    let attributes = cfg.attributes;
    let limit = cfg.logical_target_bytes;
    progress("");
    progress(&format!(
        "[{nth}/{total_shapes}] {} {}",
        shape.as_str(),
        indexes.token()
    ));
    progress("      filling until the memory limit");

    let probe_started = Instant::now();
    let (logical_bytes, disk_bytes, nodes, relationships) =
        probe_store(shape, attributes, indexes, limit, &store, live)?;
    progress(&format!(
        "      probe done   {nodes:>8} nodes   {relationships:>8} rels   logical {:>7.1} MB   disk {:>7.1} MB   {}",
        mib(logical_bytes),
        mib(disk_bytes),
        format_elapsed(probe_started.elapsed())
    ));

    mark(live, "bolt", "starting relate serve");
    progress("      bolt   starting relate serve");
    reset_store(&bolt_store)?;
    let server = ServeChild::spawn_bolt(&cfg.bolt_bin, &bolt_store, limit)?;
    clear_active(live);
    progress(&format!("      bolt   listening {}", server.addr));
    let bolt_started = Instant::now();
    let mut session = Session::connect(&server.addr)?;
    let bolted = fill_until_capacity(
        shape,
        attributes,
        indexes,
        limit,
        live,
        "bolt",
        |statement| {
            session
                .run(statement)
                .map(|_| ())
                .map_err(|err| format!("{err}\n{}", server.log_tail()))
        },
    )?;
    if bolted.nodes != nodes || bolted.rels != relationships {
        return Err(format!(
            "{} bolt load stopped at {} nodes and {} rels; the in-process load stopped at {nodes} nodes and {relationships} rels",
            shape.as_str(),
            bolted.nodes,
            bolted.rels
        ));
    }
    session
        .goodbye()
        .map_err(|err| format!("{err}\n{}", server.log_tail()))?;
    progress("      bolt   sampling rss");
    let (rss_bytes, peak_rss_bytes) = sample_child_rss(server.pid())?;
    progress(&format!(
        "      bolt  done                         rss     {:>7.1} MB   peak {:>7.1} MB   {}",
        mib(rss_bytes),
        mib(peak_rss_bytes),
        format_elapsed(bolt_started.elapsed())
    ));
    drop(server);
    reset_store(&bolt_store)?;
    Ok(ShapeSample {
        shape: shape.as_str().into(),
        index: indexes.token(),
        nodes,
        relationships,
        logical_bytes,
        disk_bytes,
        rss_bytes,
        peak_rss_bytes,
        baseline_rss_bytes: 0,
        attributed_rss_bytes: 0,
        ratio: None,
    })
}

fn probe_store(
    shape: GraphShape,
    attributes: u32,
    indexes: IndexCounts,
    limit: u64,
    store: &Path,
    live: &Mutex<Live>,
) -> Result<(u64, u64, u64, u64), String> {
    reset_store(store)?;
    let db = relate::Database::open_with(
        &store.display().to_string(),
        relate::OpenOptions {
            logical_graph_size_limit: Some(limit),
            ..relate::OpenOptions::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let filled = {
        let mut conn = db.connect().map_err(|e| e.to_string())?;
        let filled = fill_until_capacity(
            shape,
            attributes,
            indexes,
            limit,
            live,
            "probe",
            |statement| {
                conn.query(statement)
                    .map(|_| ())
                    .map_err(|e| format!("{statement}: {e}"))
            },
        )?;
        let nodes = scalar(&mut conn, "MATCH (n) RETURN count(n) AS n")?;
        let rels = scalar(&mut conn, "MATCH ()-[r]->() RETURN count(r) AS n")?;
        if nodes != filled.nodes || rels != filled.rels {
            return Err(format!(
                "{} fill counted {} nodes and {} rels; the database has {nodes} nodes and {rels} rels",
                shape.as_str(),
                filled.nodes,
                filled.rels
            ));
        }
        filled
    };
    mark(live, "probe", "checkpoint snapshot");
    progress("      probe  checkpoint snapshot");
    db.checkpoint().map_err(|e| e.to_string())?;
    clear_active(live);
    let (used, _) = db.memory_usage().map_err(|e| e.to_string())?;
    drop(db);
    let disk = store_directory_bytes(store)?;
    Ok((used, disk, filled.nodes, filled.rels))
}

/// A filesystem directory for this run. `:memory:` and a remote scheme have
/// nothing on disk to measure. `file://` is the same prefix `parse_uri` strips.
fn store_root(store: &Path) -> Result<PathBuf, String> {
    let uri = store.to_str().ok_or("store uri is not valid UTF-8")?;
    if relate_storage::is_memory_uri(uri) {
        return Err(
            "memory-shape needs a filesystem directory (--uri); :memory: has no directory to measure"
                .into(),
        );
    }
    if let Some((scheme, _)) = uri.split_once("://") {
        if !scheme.eq_ignore_ascii_case("file") {
            return Err(format!("remote store URIs are not supported: {uri}"));
        }
        let (_, path) = uri.split_once("://").expect("scheme split checked above");
        return Ok(PathBuf::from(path));
    }
    if uri.is_empty() {
        return Err(
            "memory-shape needs a filesystem directory (--uri); :memory: has no directory to measure"
                .into(),
        );
    }
    Ok(store.to_path_buf())
}

/// Remove an existing store so the next open starts empty. A missing path is
/// an empty store.
fn reset_store(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    relate_storage::reset_uri(&path.display().to_string(), None).map_err(|e| e.to_string())
}

/// Full size of a checkpointed directory store, including the WAL.
fn store_directory_bytes(root: &Path) -> Result<u64, String> {
    relate_storage::store_info(root)
        .map(|info| info.total_bytes)
        .map_err(|e| e.to_string())
}

struct Filled {
    nodes: u64,
    rels: u64,
}

/// Write batches until the database returns `capacity exceeded`.
///
/// The rejected batch is not committed. A ring closes with one relationship
/// when that still fits. `base` writes nothing and does not hit the limit.
fn fill_until_capacity(
    shape: GraphShape,
    attributes: u32,
    indexes: IndexCounts,
    limit: u64,
    live: &Mutex<Live>,
    phase: &'static str,
    mut run_one: impl FnMut(&str) -> Result<(), String>,
) -> Result<Filled, String> {
    let max_nodes = if shape == GraphShape::Base {
        0
    } else {
        census(
            shape,
            ShapeScale::Large {
                logical_bytes: limit,
            },
            IndexCounts::NONE,
            attributes,
        )?
        .nodes
        .saturating_mul(8)
        .max(1)
    };
    if let Ok(mut guard) = live.lock() {
        guard.phase = phase;
        guard.index = 0;
        guard.total = 0;
        guard.phase_started = Instant::now();
        guard.active = false;
    }
    let mut cursor = FillCursor::new(shape, attributes, indexes);
    let mut n = 0usize;
    loop {
        let Some(statement) = cursor.next_statement()? else {
            break;
        };
        n += 1;
        let label = abbreviate(&statement);
        {
            let Ok(mut guard) = live.lock() else {
                return Err("progress lock poisoned".into());
            };
            guard.phase = phase;
            guard.index = n;
            guard.total = 0;
            guard.label.clone_from(&label);
            guard.statement_started = Instant::now();
            guard.active = true;
        }
        match run_one(&statement) {
            Ok(()) => {
                cursor.ack()?;
                let Ok(mut guard) = live.lock() else {
                    return Err("progress lock poisoned".into());
                };
                guard.active = false;
                let elapsed = guard.statement_started.elapsed();
                let phase_elapsed = guard.phase_started.elapsed();
                let due = n == 1
                    || elapsed >= Duration::from_secs(2)
                    || n.is_multiple_of(25)
                    || guard.last_print.elapsed() >= Duration::from_secs(10);
                if due {
                    let line = format!(
                        "      {phase:<5} {n:>4}      {label:<52} {:>8}  total {}",
                        format_elapsed(elapsed),
                        format_elapsed(phase_elapsed)
                    );
                    guard.last_print = Instant::now();
                    drop(guard);
                    progress(&line);
                }
                if max_nodes > 0 && cursor.nodes > max_nodes {
                    return Err(format!(
                        "{} reached {} nodes without the {limit} byte memory limit",
                        shape.as_str(),
                        cursor.nodes
                    ));
                }
            }
            Err(err) if is_capacity(&err) => {
                cursor.nack()?;
                clear_active(live);
                progress(&format!(
                    "      {phase:<5} stopped at the memory limit after {n} statements"
                ));
                if let Some(wrap) = cursor.ring_wrap_statement() {
                    match run_one(&wrap) {
                        Ok(()) => cursor.note_wrap(),
                        Err(err) if is_capacity(&err) => {}
                        Err(err) => return Err(err),
                    }
                }
                return Ok(Filled {
                    nodes: cursor.nodes,
                    rels: cursor.rels,
                });
            }
            Err(err) => return Err(err),
        }
    }
    clear_active(live);
    if n == 0 {
        progress(&format!("      {phase:<5} no statements"));
    }
    if shape != GraphShape::Base {
        return Err(format!(
            "{} produced no further statements before the {limit} byte memory limit",
            shape.as_str()
        ));
    }
    Ok(Filled {
        nodes: cursor.nodes,
        rels: cursor.rels,
    })
}

fn is_capacity(err: &str) -> bool {
    err.contains("capacity exceeded")
}

fn scalar(conn: &mut relate::Connection, cypher: &str) -> Result<u64, String> {
    let result = conn.query(cypher).map_err(|e| e.to_string())?;
    let value = result
        .rows
        .first()
        .and_then(|row| row.first())
        .ok_or_else(|| format!("no row from {cypher}"))?;
    let n = value
        .as_int()
        .ok_or_else(|| format!("expected an integer from {cypher}"))?;
    u64::try_from(n).map_err(|_| format!("negative count from {cypher}"))
}

/// Docker's memory cap, from the cgroup this process is in.
fn container_cgroup_memory_max() -> Option<u64> {
    for path in [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ] {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let text = text.trim();
        if text == "max" {
            return None;
        }
        if let Ok(bytes) = text.parse::<u64>() {
            // cgroup v1 reports a huge number when the limit is unset.
            if bytes < 1 << 60 {
                return Some(bytes);
            }
        }
    }
    None
}

pub fn format_report(report: &MemoryShapeReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<12} {:<22} {:>12} {:>12} {:>12} {:>12} {:>12} {:>8}\n",
        "shape", "index", "nodes", "rels", "logical MB", "disk MB", "rss MB", "ratio"
    ));
    for sample in &report.shapes {
        let ratio = sample
            .ratio
            .map(|ratio| format!("{ratio:8.3}"))
            .unwrap_or_else(|| format!("{:>8}", "-"));
        out.push_str(&format!(
            "{:<12} {:<22} {:>12} {:>12} {:>12.1} {:>12.1} {:>12.1} {ratio}\n",
            sample.shape,
            sample.index,
            sample.nodes,
            sample.relationships,
            mib(sample.logical_bytes),
            mib(sample.disk_bytes),
            mib(sample.rss_bytes),
        ));
    }
    out.push_str(
        "MB is 1024×1024 bytes. logical MB is memory_usage after the batch that crossed the limit was rejected. disk MB is the full size of that shape's database directory after checkpoint, including the WAL.\n",
    );
    out
}

/// Sample the `relate serve` child's RSS: the minimum `VmRSS` and maximum
/// `VmHWM` over [`RSS_SAMPLES`] `/proc/<pid>/status` reads. This is child
/// sampling, unlike the loadtest harness's own-process `sample_rss`.
fn sample_child_rss(pid: u32) -> Result<(u64, u64), String> {
    let mut min_rss = u64::MAX;
    let mut peak = 0u64;
    for i in 0..RSS_SAMPLES {
        if i > 0 {
            thread::sleep(SAMPLE_PAUSE);
        }
        let status = process_status(pid)
            .ok_or_else(|| format!("failed to read /proc/{pid}/status while sampling RSS"))?;
        min_rss = min_rss.min(status.rss_bytes);
        peak = peak.max(status.peak_rss_bytes);
    }
    Ok((min_rss, peak))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Disk tier (ADR-0251): `probe_store` checkpoints and measures the
    // on-disk store directory (`store_directory_bytes` →
    // `relate_storage::store_info`, which rejects `:memory:` — an
    // in-memory store has no on-disk layout), so these tests keep a real
    // filesystem directory.
    #[test]
    fn flat_fill_stops_when_the_limit_is_hit() {
        let limit = 1024 * 1024;
        let live = Mutex::new(Live::default());
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("flat");
        let (used, disk, nodes, rels) =
            probe_store(GraphShape::Flat, 1, IndexCounts::NONE, limit, &store, &live).unwrap();
        assert!(
            used <= limit,
            "committed {used} bytes above the {limit} limit"
        );
        assert!(
            used > limit / 2,
            "stopped at {used} bytes, far under the {limit} limit"
        );
        assert!(nodes >= 2_000 && nodes.is_multiple_of(2_000));
        assert_eq!(rels, 0);
        assert!(disk > 0, "checkpointed store directory is empty");
        assert_eq!(disk, store_directory_bytes(&store).unwrap());
        assert!(
            store.join("wal").is_dir(),
            "WAL is part of the directory size"
        );
    }

    #[test]
    fn ring_fill_closes_when_the_next_batch_does_not_fit() {
        // One closed ring of 2,000 nodes. The next node batch has to miss,
        // and the ring has to be closed when it does. A relationship batch
        // can miss while another node batch still fits, which leaves the
        // ring open; 1.25 MB sits under that second node batch and over
        // the closed ring.
        let limit = 1_250_000;
        let live = Mutex::new(Live::default());
        let dir = tempfile::tempdir().unwrap();
        let (used, _, nodes, rels) = probe_store(
            GraphShape::Ring,
            1,
            IndexCounts::NONE,
            limit,
            &dir.path().join("ring"),
            &live,
        )
        .unwrap();
        assert!(
            used <= limit,
            "committed {used} bytes above the {limit} limit"
        );
        assert_eq!(
            nodes, rels,
            "ring did not close ({nodes} nodes, {rels} rels, {used} bytes)"
        );
        assert!(nodes >= 2_000, "used {used} bytes");
    }

    #[test]
    fn dense_ring_fill_links_each_window_before_the_next() {
        let limit = 4 * 1024 * 1024;
        let live = Mutex::new(Live::default());
        let dir = tempfile::tempdir().unwrap();
        let (used, _, nodes, rels) = probe_store(
            GraphShape::DenseRing,
            1,
            IndexCounts::NONE,
            limit,
            &dir.path().join("dense-ring"),
            &live,
        )
        .unwrap();
        assert!(
            used <= limit,
            "committed {used} bytes above the {limit} limit"
        );
        assert!(nodes >= 100, "used {used} bytes");
        assert_eq!(
            rels,
            nodes * 20,
            "last window was not linked ({nodes} nodes, {rels} rels, {used} bytes)"
        );
    }
}
