//! Time each query (ADR-0243): one untimed warmup whose rows are checked
//! against the expected answer, then timed rounds until both `min_rounds`
//! and `min_time` are reached. Every execution runs under a per-query
//! timeout enforced with a [`CancelToken`], so a runaway query is a
//! recorded `timeout`, not a hung run.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use relate::{CancelToken, Connection, QueryResult};
use serde::Serialize;

use crate::answers::{self, Expected};
use crate::workload::Query;

/// How each query is timed.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Timed rounds at least (upstream's `--benchmark-min-rounds=5`).
    pub min_rounds: usize,
    /// Keep timing until this much round time has passed
    /// (upstream's `--benchmark-max-time=1.0` budget).
    pub min_time: Duration,
    /// Per-execution timeout.
    pub timeout: Duration,
    /// Hard cap on timed rounds, so a microsecond query stays bounded.
    pub max_rounds: usize,
}

impl RunConfig {
    pub fn new(timeout: Duration) -> Self {
        Self {
            min_rounds: 5,
            min_time: Duration::from_secs(1),
            timeout,
            max_rounds: 100_000,
        }
    }
}

/// How a query ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Answered as expected.
    Ok,
    /// Answered, but not as expected (still timed).
    Wrong,
    /// The engine returned an error.
    Error,
    /// Cancelled at the timeout.
    Timeout,
}

impl Status {
    pub const ALL: [Status; 4] = [Self::Ok, Self::Wrong, Self::Error, Self::Timeout];

    /// The status's one text form: the report's JSON value (the serde
    /// rename above), the console table and the progress line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Wrong => "wrong",
            Self::Error => "error",
            Self::Timeout => "timeout",
        }
    }
}

impl std::fmt::Display for Status {
    /// Honours width and alignment (`{:<8}`), so tables can pad it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// Summary statistics of a set of durations, in nanoseconds.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub min_ns: u64,
    pub p50_ns: u64,
    pub mean_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

impl Stats {
    /// Nearest-rank percentiles over `samples` (empty gives zeros).
    pub fn of(samples: &[Duration]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut ns: Vec<u64> = samples.iter().map(|d| d.as_nanos() as u64).collect();
        ns.sort_unstable();
        let rank = |p: f64| ns[relate_test_support::nearest_rank(ns.len(), p)];
        Self {
            min_ns: ns[0],
            p50_ns: rank(0.50),
            mean_ns: (ns.iter().map(|&x| x as u128).sum::<u128>() / ns.len() as u128) as u64,
            p99_ns: rank(0.99),
            max_ns: ns[ns.len() - 1],
        }
    }
}

/// One query's result.
#[derive(Debug, Clone, Serialize)]
pub struct QueryOutcome {
    pub qid: u32,
    pub query_sha256: String,
    pub status: Status,
    /// Rows of the checked (warmup) execution.
    pub rows: usize,
    pub rounds: usize,
    /// Store reads of the checked (PROFILE) execution, the plan tree's
    /// `db_hits` summed: deterministic for a fixed graph and plan, so the
    /// gate compares it in every environment class. Zero when the query
    /// failed before its checked execution returned; such a query is not
    /// `ok`, which the gate fails on first.
    pub db_hits: u64,
    /// Engine-reported statement elapsed (ADR-0221) per round.
    pub engine: Stats,
    /// Wall time around `query_with_params_cancellable` per round.
    pub wall: Stats,
    /// The answer difference (`wrong`) or the error (`error`, `timeout`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Run one query: the checked warmup (a `PROFILE` execution, so the answer
/// check and the store-read count come from the one run), then the timed
/// plain rounds.
pub fn run_query(
    conn: &mut Connection,
    q: &Query,
    expected: Option<&Expected>,
    cfg: &RunConfig,
    watchdog: &Watchdog,
) -> QueryOutcome {
    let mut out = QueryOutcome {
        qid: q.qid,
        query_sha256: q.sha256.clone(),
        status: Status::Ok,
        rows: 0,
        rounds: 0,
        db_hits: 0,
        engine: Stats::default(),
        wall: Stats::default(),
        detail: None,
    };
    let first = match execute(conn, &format!("PROFILE {}", q.text), cfg.timeout, watchdog) {
        Ok((r, _)) => r,
        Err((status, msg)) => {
            out.status = status;
            out.detail = Some(msg);
            return out;
        }
    };
    out.db_hits = match profile_db_hits(&first) {
        Ok(hits) => hits,
        Err(msg) => {
            out.status = Status::Error;
            out.detail = Some(msg);
            return out;
        }
    };
    out.rows = first.rows.len();
    if let Some(exp) = expected {
        if let Some(diff) = answers::diff(&first, exp) {
            out.status = Status::Wrong;
            out.detail = Some(diff);
        }
    }
    drop(first);
    let (mut engine, mut wall) = (Vec::new(), Vec::new());
    let mut spent = Duration::ZERO;
    while engine.len() < cfg.max_rounds && (engine.len() < cfg.min_rounds || spent < cfg.min_time) {
        match execute(conn, &q.text, cfg.timeout, watchdog) {
            Ok((r, w)) => {
                engine.push(r.header.elapsed);
                wall.push(w);
                spent += w;
            }
            Err((status, msg)) => {
                out.status = status;
                out.detail = Some(format!("round {}: {msg}", engine.len() + 1));
                break;
            }
        }
    }
    out.rounds = engine.len();
    out.engine = Stats::of(&engine);
    out.wall = Stats::of(&wall);
    out
}

/// The store reads of one PROFILE result: every executed plan node's
/// `db_hits`, summed (the plan's one executed-children walk skips the arms
/// that never ran). A result with no plan is a PROFILE the engine did not
/// stamp: an error naming that, never a zero.
fn profile_db_hits(result: &QueryResult) -> Result<u64, String> {
    fn sum(node: &relate::PlanNode) -> u64 {
        node.metrics.as_ref().map_or(0, |m| m.db_hits)
            + node.executed_children().map(sum).sum::<u64>()
    }
    result
        .plan_root()
        .map(sum)
        .ok_or_else(|| "PROFILE returned no plan".to_string())
}

/// Execute `text` once under the timeout: the result and its wall time, or
/// the failure's status and message.
pub fn execute(
    conn: &mut Connection,
    text: &str,
    timeout: Duration,
    watchdog: &Watchdog,
) -> Result<(QueryResult, Duration), (Status, String)> {
    execute_with_params(conn, text, &BTreeMap::new(), timeout, watchdog)
}

/// [`execute`] with query parameters — the corpus lane's statements are
/// natively parameterized (`$seed`), unlike the graph-benchmark-ldbc lane's substituted
/// literals (ADR-0247).
pub fn execute_with_params(
    conn: &mut Connection,
    text: &str,
    params: &BTreeMap<String, relate::Value>,
    timeout: Duration,
    watchdog: &Watchdog,
) -> Result<(QueryResult, Duration), (Status, String)> {
    let token = CancelToken::new();
    watchdog.arm(token.clone(), timeout);
    let t = Instant::now();
    let r = conn.query_with_params_cancellable(text, params.clone(), token.clone());
    let wall = t.elapsed();
    watchdog.disarm();
    match r {
        Ok(r) => Ok((r, wall)),
        Err(_) if token.is_cancelled() => Err((
            Status::Timeout,
            format!("cancelled after {:.1} s", timeout.as_secs_f64()),
        )),
        Err(e) => Err((Status::Error, e.to_string())),
    }
}

#[derive(Default)]
struct Armed {
    job: Option<(Instant, CancelToken)>,
    shutdown: bool,
}

/// One background thread that cancels the armed token when its deadline
/// passes. Arming and disarming are a lock and a notify, so the watchdog
/// costs no thread spawn per execution.
pub struct Watchdog {
    state: Arc<(Mutex<Armed>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    pub fn new() -> Self {
        let state = Arc::new((Mutex::new(Armed::default()), Condvar::new()));
        let shared = state.clone();
        let thread = std::thread::Builder::new()
            .name("query-latency-watchdog".into())
            .spawn(move || {
                let (lock, cv) = &*shared;
                let mut armed = lock.lock().unwrap();
                loop {
                    if armed.shutdown {
                        return;
                    }
                    match &armed.job {
                        None => armed = cv.wait(armed).unwrap(),
                        Some((deadline, token)) => {
                            let now = Instant::now();
                            if now >= *deadline {
                                token.cancel();
                                armed.job = None;
                            } else {
                                let wait = *deadline - now;
                                armed = cv.wait_timeout(armed, wait).unwrap().0;
                            }
                        }
                    }
                }
            })
            .expect("spawning the watchdog thread");
        Self {
            state,
            thread: Some(thread),
        }
    }

    fn set(&self, f: impl FnOnce(&mut Armed)) {
        let (lock, cv) = &*self.state;
        f(&mut lock.lock().unwrap());
        cv.notify_one();
    }

    pub fn arm(&self, token: CancelToken, timeout: Duration) {
        self.set(|a| a.job = Some((Instant::now() + timeout, token)));
    }

    pub fn disarm(&self) {
        self.set(|a| a.job = None);
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.set(|a| a.shutdown = true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: &[u64]) -> Vec<Duration> {
        v.iter().map(|&x| Duration::from_millis(x)).collect()
    }

    #[test]
    fn status_text_is_its_json_value() {
        for st in Status::ALL {
            assert_eq!(serde_json::to_value(st).unwrap(), st.as_str());
            assert_eq!(st.to_string(), st.as_str());
        }
        assert_eq!(format!("[{:<7}]", Status::Ok), "[ok     ]");
    }

    #[test]
    fn stats_use_nearest_rank_percentiles() {
        let s = Stats::of(&ms(&[5, 1, 3, 2, 4]));
        assert_eq!(s.min_ns, 1_000_000);
        assert_eq!(s.p50_ns, 3_000_000);
        assert_eq!(s.mean_ns, 3_000_000);
        assert_eq!(s.p99_ns, 5_000_000);
        assert_eq!(s.max_ns, 5_000_000);
        let hundred: Vec<u64> = (1..=100).collect();
        assert_eq!(Stats::of(&ms(&hundred)).p99_ns, 99_000_000);
        assert_eq!(Stats::of(&[]).p50_ns, 0);
    }

    #[test]
    fn watchdog_cancels_only_past_the_deadline() {
        let w = Watchdog::new();
        let quick = CancelToken::new();
        w.arm(quick.clone(), Duration::from_secs(60));
        w.disarm();
        let slow = CancelToken::new();
        w.arm(slow.clone(), Duration::from_millis(20));
        let t = Instant::now();
        while !slow.is_cancelled() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(slow.is_cancelled());
        assert!(!quick.is_cancelled());
    }

    fn graph() -> (relate::Database, Connection) {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("UNWIND range(1, 3) AS i CREATE (:N {i: i})")
            .unwrap();
        (db, conn)
    }

    fn query(text: &str) -> Query {
        Query {
            qid: 1,
            header: String::new(),
            text: text.into(),
            sha256: String::new(),
        }
    }

    fn quick() -> RunConfig {
        RunConfig {
            min_rounds: 3,
            min_time: Duration::ZERO,
            timeout: Duration::from_secs(30),
            max_rounds: 10,
        }
    }

    #[test]
    fn a_right_answer_is_ok_and_timed() {
        let (_db, mut conn) = graph();
        let exp = crate::workload::parse_expected(
            r#"{"q1": {"rows": [{"c": 3}], "order_sensitive": true}}"#,
        )
        .unwrap()
        .remove(&1);
        let o = run_query(
            &mut conn,
            &query("MATCH (n:N) RETURN count(n) AS c"),
            exp.as_ref(),
            &quick(),
            &Watchdog::new(),
        );
        assert_eq!(o.status, Status::Ok, "{:?}", o.detail);
        assert_eq!((o.rows, o.rounds), (1, 3));
        assert!(o.wall.max_ns >= o.wall.min_ns);
    }

    #[test]
    fn a_label_count_reads_the_store_deterministically() {
        let (_db, mut conn) = graph();
        let run = |conn: &mut Connection| {
            run_query(
                conn,
                &query("MATCH (n:N) RETURN count(n) AS c"),
                None,
                &quick(),
                &Watchdog::new(),
            )
        };
        let first = run(&mut conn);
        assert_eq!(first.status, Status::Ok, "{:?}", first.detail);
        assert!(first.db_hits > 0, "{first:?}");
        assert_eq!(first.db_hits, run(&mut conn).db_hits);
    }

    #[test]
    fn a_wrong_answer_is_reported_and_still_timed() {
        let (_db, mut conn) = graph();
        let exp = crate::workload::parse_expected(
            r#"{"q1": {"rows": [{"c": 4}], "order_sensitive": true}}"#,
        )
        .unwrap()
        .remove(&1);
        let o = run_query(
            &mut conn,
            &query("MATCH (n:N) RETURN count(n) AS c"),
            exp.as_ref(),
            &quick(),
            &Watchdog::new(),
        );
        assert_eq!(o.status, Status::Wrong);
        assert_eq!(o.rounds, 3);
        assert!(o.detail.unwrap().contains("expected"));
    }

    #[test]
    fn an_engine_error_stops_the_query() {
        let (_db, mut conn) = graph();
        let o = run_query(
            &mut conn,
            &query("RETURN 1 +"),
            None,
            &quick(),
            &Watchdog::new(),
        );
        assert_eq!(o.status, Status::Error);
        assert_eq!(o.rounds, 0);
    }

    #[test]
    fn a_query_past_its_timeout_is_cancelled() {
        let (_db, mut conn) = graph();
        let cfg = RunConfig {
            timeout: Duration::from_millis(50),
            ..quick()
        };
        // A cross product is slow per row without building one large value
        // — a `range(1, 100000000)` list is refused by the expression budget
        // (ADR-0463) before the timeout can fire (the list is one value).
        let slow = "UNWIND range(1, 1000) AS a UNWIND range(1, 1000) AS b \
                    UNWIND range(1, 100) AS c RETURN count(c) AS c";
        let o = run_query(&mut conn, &query(slow), None, &cfg, &Watchdog::new());
        assert_eq!(o.status, Status::Timeout, "{:?}", o.detail);
        // The connection stays usable after a cancel.
        let r = conn.query("MATCH (n:N) RETURN count(n) AS c").unwrap();
        assert_eq!(r.rows.len(), 1);
    }
}
