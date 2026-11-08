//! Configured processing-memory workload.
//!
//! The invariant cases keep their own caps. This workload uses the operator
//! controls: graph scale, client workers, read/write ratio, and the serve
//! limits. The graph is loaded with limits off, then reopened with the
//! requested limits before the measured operations start.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::Serialize;

use crate::bolt::Session;
use crate::memory_controls::harness::{
    self, connect_addr, CaseReport, Config, ServeLimits, Server,
};
use crate::processing_graph::{self, Ledger};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndexMode {
    Indexed,
    Scan,
    Both,
}

/// Which read statement the workload runs. `Mix` is the six-way rotation.
/// `DeepPaths` and `SizeSort` fill every read slot with one heavy query.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ReadQuery {
    #[default]
    Mix,
    DeepPaths,
    SizeSort,
}

#[derive(Debug, Clone, Serialize)]
pub struct Controls {
    pub graph_scale: u64,
    pub workers: u64,
    pub read_parts: u64,
    pub write_parts: u64,
    pub max_connections: u64,
    pub concurrent_logical_workspace_limit: u64,
    pub ops: u64,
    pub warmup_ops: u64,
    pub seed: u64,
    pub index_mode: IndexMode,
    pub read_query: ReadQuery,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            graph_scale: 1,
            workers: 4,
            read_parts: 80,
            write_parts: 20,
            max_connections: 0,
            concurrent_logical_workspace_limit: 0,
            ops: 40,
            warmup_ops: 8,
            seed: 1,
            index_mode: IndexMode::Both,
            read_query: ReadQuery::Mix,
        }
    }
}

impl Controls {
    pub fn validate(&self) -> Result<(), String> {
        if self.graph_scale == 0 {
            return Err("graph scale must be at least 1".into());
        }
        if self.workers == 0 {
            return Err("workers must be at least 1".into());
        }
        if self
            .read_parts
            .checked_add(self.write_parts)
            .ok_or("read/write ratio parts overflow")?
            == 0
        {
            return Err("read/write ratio needs a positive read or write part".into());
        }
        if self.ops.checked_mul(self.read_parts).is_none()
            || self.warmup_ops.checked_mul(self.read_parts).is_none()
        {
            return Err("ops times the read-ratio part overflows".into());
        }
        Ok(())
    }

    pub fn ledger(&self) -> Ledger {
        Ledger::from_scale(self.graph_scale)
    }

    pub fn read_write_ratio(&self) -> String {
        format!("{}:{}", self.read_parts, self.write_parts)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkloadReport {
    pub scenario: crate::memory_controls::harness::ScenarioText,
    pub case: CaseReport,
    pub index_mode: IndexMode,
    pub reads_succeeded: u64,
    pub reads_limited: u64,
    pub reads_failed: u64,
    pub writes_succeeded: u64,
    pub writes_limited: u64,
    pub writes_failed: u64,
    pub connections_refused: u64,
    pub active_query_refusals: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Write,
}

struct Planned {
    kind: Kind,
    cypher: String,
}

#[derive(Default)]
struct Tally {
    reads_succeeded: u64,
    reads_limited: u64,
    reads_failed: u64,
    writes_succeeded: u64,
    writes_limited: u64,
    writes_failed: u64,
    active_query_refusals: u64,
}

pub fn parse_ratio(raw: &str) -> Result<(u64, u64), String> {
    let Some((reads, writes)) = raw.split_once(':') else {
        return Err(format!(
            "read/write ratio must look like 80:20, got {raw:?}"
        ));
    };
    let read_parts = reads
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("read/write ratio has a bad read part: {raw:?}"))?;
    let write_parts = writes
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("read/write ratio has a bad write part: {raw:?}"))?;
    if read_parts == 0 && write_parts == 0 {
        return Err(format!("read/write ratio must not be 0:0 ({raw:?})"));
    }
    Ok((read_parts, write_parts))
}

pub fn parse_read_query(raw: &str) -> Result<ReadQuery, String> {
    match raw {
        "mix" => Ok(ReadQuery::Mix),
        "deep-paths" => Ok(ReadQuery::DeepPaths),
        "size-sort" => Ok(ReadQuery::SizeSort),
        other => Err(format!(
            "read query must be mix, deep-paths, or size-sort, got {other:?}"
        )),
    }
}

pub fn parse_index_mode(raw: &str) -> Result<IndexMode, String> {
    match raw {
        "indexed" => Ok(IndexMode::Indexed),
        "scan" => Ok(IndexMode::Scan),
        "both" => Ok(IndexMode::Both),
        other => Err(format!(
            "index mode must be indexed, scan, or both, got {other:?}"
        )),
    }
}

pub struct WorkloadOutcome {
    pub reports: Vec<WorkloadReport>,
    pub gate_error: Option<String>,
}

pub fn run(
    cfg: &Config,
    mut on_workload: impl FnMut(&WorkloadReport),
) -> Result<WorkloadOutcome, String> {
    cfg.controls.validate()?;
    let modes = match cfg.controls.index_mode {
        IndexMode::Indexed => vec![true],
        IndexMode::Scan => vec![false],
        IndexMode::Both => vec![true, false],
    };
    let mut reports = Vec::new();
    for indexes in modes {
        let report = one(cfg, indexes)?;
        on_workload(&report);
        reports.push(report);
    }
    Ok(WorkloadOutcome {
        reports,
        gate_error: None,
    })
}

fn one(cfg: &Config, indexes: bool) -> Result<WorkloadReport, String> {
    let label = if indexes { "indexed" } else { "scan" };
    let ledger = cfg.controls.ledger();
    let loaded = cfg.store.join(format!("workload-{label}"));
    {
        let server = Server::start(&cfg.serve_bin, &loaded, ServeLimits::default())?;
        let mut session = connect_addr(server.bolt())?;
        // 8192 scratch entities stays under the 4 MiB query-text cap.
        processing_graph::load(&mut session, ledger, indexes, 8192)?;
        drop(session);
    }
    let empty_dir = cfg.store.join(format!("workload-{label}-empty"));
    let baseline = {
        let server = Server::start(&cfg.serve_bin, &empty_dir, server_limits(&cfg.controls))?;
        let sample = server.sample()?;
        drop(server);
        sample
    };
    let server = Server::open_existing(&cfg.serve_bin, &loaded, server_limits(&cfg.controls))?;
    let cold = server.sample()?;
    {
        let mut session = connect_addr(server.bolt())?;
        session.run("RETURN 1")?;
    }
    let logical = crate::processing_logical::logical_bytes(&server)?;
    let session_cost = server.session_cost(cold.rss)?;
    if cfg.controls.warmup_ops > 0 {
        let warmup = plan(
            cfg.controls.seed.wrapping_add(1),
            cfg.controls.warmup_ops,
            cfg.controls.read_parts,
            cfg.controls.write_parts,
            ledger.account_count(),
            cfg.controls.read_query,
        )?;
        let mut session = connect_addr(server.bolt())?;
        for op in warmup {
            session.run(&op.cypher)?;
        }
    }
    let ops = plan(
        cfg.controls.seed,
        cfg.controls.ops,
        cfg.controls.read_parts,
        cfg.controls.write_parts,
        ledger.account_count(),
        cfg.controls.read_query,
    )?;
    let (tally, refused) = run_workers(server.bolt(), &cfg.controls, ops)?;
    thread::sleep(Duration::from_millis(50));
    let idle = server.sample()?.rss;
    let case = harness::finish(
        &format!("workload-{label}"),
        &server,
        baseline,
        cold,
        logical,
        session_cost,
        cfg.controls.max_connections,
        vec![idle],
        "configured workload",
    )?;
    let mut report = WorkloadReport {
        scenario: crate::memory_controls::harness::ScenarioText::empty(),
        case,
        index_mode: if indexes {
            IndexMode::Indexed
        } else {
            IndexMode::Scan
        },
        reads_succeeded: tally.reads_succeeded,
        reads_limited: tally.reads_limited,
        reads_failed: tally.reads_failed,
        writes_succeeded: tally.writes_succeeded,
        writes_limited: tally.writes_limited,
        writes_failed: tally.writes_failed,
        connections_refused: refused,
        active_query_refusals: tally.active_query_refusals,
    };
    super::scenario::fill_workload(cfg, &mut report);
    Ok(report)
}

fn server_limits(controls: &Controls) -> ServeLimits {
    ServeLimits {
        // Pass 0 through. It is `--max-connections 0`, the explicit unlimited
        // spelling. An absent flag is the server default of 128 (ADR-0451).
        max_connections: Some(controls.max_connections),
        concurrent_logical_workspace_limit: positive(controls.concurrent_logical_workspace_limit),
        ..ServeLimits::default()
    }
}

fn positive(value: u64) -> Option<u64> {
    (value > 0).then_some(value)
}

fn run_workers(addr: &str, controls: &Controls, ops: Vec<Planned>) -> Result<(Tally, u64), String> {
    let queue = Arc::new(Mutex::new(VecDeque::from(ops)));
    let mut sessions = Vec::new();
    let mut refused = 0u64;
    // Hold every accepted session before any worker runs, so a full cap
    // stays full while the extra handshakes are attempted.
    for _ in 0..controls.workers {
        match connect_addr(addr) {
            Ok(session) => sessions.push(session),
            Err(err) if connection_refused(&err) => refused += 1,
            Err(err) => return Err(err),
        }
    }
    let expected = if controls.max_connections == 0 {
        0
    } else {
        controls.workers.saturating_sub(controls.max_connections)
    };
    if refused != expected {
        return Err(format!(
            "connections refused {refused}, expected {expected} with --max-connections {} and {} workers",
            controls.max_connections, controls.workers
        ));
    }
    let mut handles = Vec::new();
    for session in sessions {
        let queue = Arc::clone(&queue);
        handles.push(thread::spawn(move || worker(session, queue)));
    }
    let mut tally = Tally::default();
    for handle in handles {
        let worker_tally = handle
            .join()
            .map_err(|_| "workload worker panicked".to_string())??;
        tally.add(worker_tally);
    }
    Ok((tally, refused))
}

fn worker(mut session: Session, queue: Arc<Mutex<VecDeque<Planned>>>) -> Result<Tally, String> {
    let mut tally = Tally::default();
    loop {
        let next = queue.lock().map_err(|err| err.to_string())?.pop_front();
        let Some(op) = next else {
            break;
        };
        match session.run(&op.cypher) {
            Ok(()) => tally.succeed(op.kind),
            Err(err) => tally.limit(op.kind, &err)?,
        }
    }
    Ok(tally)
}

fn connection_refused(err: &str) -> bool {
    err.contains("handshake") || err.starts_with("connect ")
}

impl Tally {
    fn add(&mut self, other: Self) {
        self.reads_succeeded += other.reads_succeeded;
        self.reads_limited += other.reads_limited;
        self.reads_failed += other.reads_failed;
        self.writes_succeeded += other.writes_succeeded;
        self.writes_limited += other.writes_limited;
        self.writes_failed += other.writes_failed;
        self.active_query_refusals += other.active_query_refusals;
    }

    fn mark_limited(&mut self, kind: Kind) {
        match kind {
            Kind::Read => self.reads_limited += 1,
            Kind::Write => self.writes_limited += 1,
        }
    }

    fn succeed(&mut self, kind: Kind) {
        match kind {
            Kind::Read => self.reads_succeeded += 1,
            Kind::Write => self.writes_succeeded += 1,
        }
    }

    fn limit(&mut self, kind: Kind, err: &str) -> Result<(), String> {
        if err.contains("concurrent-logical-workspace-limit") {
            self.active_query_refusals += 1;
            self.mark_limited(kind);
            return Ok(());
        }
        // Overlapping workers updating one account is a normal outcome.
        if err.contains("write-write conflict") {
            match kind {
                Kind::Read => self.reads_failed += 1,
                Kind::Write => self.writes_failed += 1,
            }
            return Ok(());
        }
        Err(format!(
            "workload query failed outside a resource limit: {err}"
        ))
    }
}

fn plan(
    seed: u64,
    ops: u64,
    read_parts: u64,
    write_parts: u64,
    accounts: i64,
    read_query: ReadQuery,
) -> Result<Vec<Planned>, String> {
    let parts = read_parts
        .checked_add(write_parts)
        .ok_or("read/write ratio parts overflow")?;
    if ops == 0 || parts == 0 {
        return Ok(Vec::new());
    }
    let reads = ops
        .checked_mul(read_parts)
        .ok_or("ops times the read-ratio part overflows")?
        / parts;
    let writes = ops
        .checked_sub(reads)
        .ok_or("read/write split underflows")?;
    let mut planned = Vec::with_capacity(ops as usize);
    for index in 0..reads {
        planned.push(Planned {
            kind: Kind::Read,
            cypher: read_cypher(index, read_query),
        });
    }
    for index in 0..writes {
        planned.push(Planned {
            kind: Kind::Write,
            cypher: write_cypher(index, accounts),
        });
    }
    let mut rng = StdRng::seed_from_u64(seed);
    planned.shuffle(&mut rng);
    Ok(planned)
}

fn read_cypher(index: u64, read_query: ReadQuery) -> String {
    match read_query {
        ReadQuery::DeepPaths => return processing_graph::deep_paths().to_string(),
        ReadQuery::SizeSort => return processing_graph::size_sort().to_string(),
        ReadQuery::Mix => {}
    }
    match index % 6 {
        0 => processing_graph::point_read().to_string(),
        1 => processing_graph::page_read(),
        2 => "MATCH (a:Account) WHERE a.balance >= 1000 AND a.balance < 2000 RETURN a.id, a.balance ORDER BY a.balance LIMIT 8".to_string(),
        3 => processing_graph::size_sort().to_string(),
        4 => processing_graph::deep_paths().to_string(),
        _ => "MATCH (a:Account) RETURN count(a) AS accounts, sum(a.balance) AS balance".to_string(),
    }
}

fn write_cypher(index: u64, accounts: i64) -> String {
    let accounts = accounts.max(1) as u64;
    let id = index % accounts;
    let other = (id + 1) % accounts;
    match index % 4 {
        0 => format!(
            "MATCH (a:Account) WHERE a.id = {id} SET a.status = 'review' CREATE (:Review {{account: {id}, kind: 'manual', note: 'worker {index}'}})"
        ),
        1 => format!("MATCH (a:Account) WHERE a.id = {id} SET a.risk = a.risk + 1"),
        2 => format!(
            "MATCH (a:Account {{id: {id}}}) MATCH (b:Account {{id: {other}}}) CREATE (a)-[:REVIEWED {{note: 'worker {index}'}}]->(b)"
        ),
        _ => processing_graph::bulk_reviews(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_splits_an_operation_window_exactly() {
        let planned = plan(7, 40, 80, 20, 96, ReadQuery::Mix).unwrap();
        let reads = planned.iter().filter(|op| op.kind == Kind::Read).count();
        let writes = planned.iter().filter(|op| op.kind == Kind::Write).count();
        assert_eq!(reads, 32);
        assert_eq!(writes, 8);
        assert_eq!(
            plan(7, 40, 80, 20, 96, ReadQuery::Mix).unwrap().len(),
            planned.len()
        );
        assert_eq!(
            plan(7, 40, 80, 20, 96, ReadQuery::Mix).unwrap()[0].cypher,
            planned[0].cypher
        );
        let deep = plan(7, 4, 100, 0, 96, ReadQuery::DeepPaths).unwrap();
        assert!(deep.iter().all(|op| op.cypher.contains("PARENT_OF")));
        let sorted = plan(7, 4, 100, 0, 96, ReadQuery::SizeSort).unwrap();
        assert!(sorted
            .iter()
            .all(|op| op.cypher.contains("ORDER BY a.balance")));
    }

    #[test]
    fn ratio_parser_accepts_the_make_form() {
        assert_eq!(parse_ratio("80:20").unwrap(), (80, 20));
        assert!(parse_ratio("80").is_err());
        assert!(parse_ratio("0:0").is_err());
        assert_eq!(parse_index_mode("both").unwrap(), IndexMode::Both);
        assert_eq!(
            parse_read_query("deep-paths").unwrap(),
            ReadQuery::DeepPaths
        );
        assert!(parse_read_query("walk").is_err());
    }
}
