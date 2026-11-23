//! The one serve-child ceremony for harness spawns of `relate serve`.
//!
//! Two harnesses in this crate spawn the product binary as a serve child —
//! `memory_shape`'s bolt case (bolt only, `--logical-graph-size-limit`) and
//! the `processing_memory` suites (bolt plus optional http, connection and
//! admission limits). The ceremony they
//! all walk — free loopback port, base serve argv, the harness child's env
//! policy, stderr drain, readiness wait — lives here once, and [`ServeChild`]
//! owns the child from spawn to the one kill-and-reap; each caller adds only
//! its real extras.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use relate_test_support::harness_child_env;

/// A free loopback port, handed to the spawned `relate serve` child
/// (LABS-424): the server binds the exact port the readiness wait and the
/// sessions then connect to. A child that binds more than one port uses
/// [`free_ports`], which keeps the sockets until every port is known.
pub(crate) fn free_port() -> Result<u16, String> {
    free_ports(1)?
        .pop()
        .ok_or_else(|| "port reservation was empty".to_string())
}

/// Reserve `count` distinct loopback ports for one serve child.
///
/// Every listener stays bound until all `count` ports are known. Probing
/// one port at a time lets Linux hand the just-released port out again, so
/// `--http` and `--bolt` can name the same address. Serve binds HTTP
/// first and then fails the Bolt bind with "address already in use".
pub(crate) fn free_ports(count: usize) -> Result<Vec<u16>, String> {
    let mut listeners = Vec::with_capacity(count);
    for _ in 0..count {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        listeners.push(listener);
    }
    let mut ports = Vec::with_capacity(count);
    for listener in &listeners {
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        ports.push(port);
    }
    Ok(ports)
}

/// The base serve argv every harness child starts from, with the shared
/// harness child env policy (`harness_child_env`: the rule-based
/// `RELATE_*` scrub and the ADR-0079 §6 telemetry opt-out) already
/// applied — a caller extends the argv with its own flags and sets its
/// deliberate env on top, and the scrub has already run.
pub(crate) fn serve_command(bin: &Path, store: &Path, bolt: u16) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("serve")
        .arg("--database")
        .arg(store)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--bolt")
        .arg(bolt.to_string())
        .arg("--auth")
        .arg("none");
    harness_child_env(&mut cmd);
    cmd
}

/// The readiness deadline every spawn waits by default: the synthetic shapes
/// and the loadtests open their stores in well under it.
pub(crate) const READY_DEADLINE: Duration = Duration::from_secs(30);

/// How many trailing log lines [`ServeChild::log_tail`] keeps.
const LOG_TAIL_LINES: usize = 20;

/// The drained stderr of a serve child: every line the reader thread saw, in
/// arrival order.
type ServeLog = Arc<Mutex<Vec<String>>>;

/// A spawned `relate serve` child: the process, the loopback bolt address
/// it serves, its drained stderr log and the caller's readiness window.
/// The kill-and-reap `Drop` ends it.
pub(crate) struct ServeChild {
    child: Mutex<Child>,
    /// The bolt address the child serves — what sessions connect to.
    pub(crate) addr: String,
    /// The caller's readiness window: the spawn's own wait for [`Self::addr`]
    /// and every later [`ServeChild::wait_for`] address.
    readiness: Duration,
    log: ServeLog,
}

impl ServeChild {
    /// Spawn a bolt-only serve child: the base argv under
    /// `logical_graph_size_limit`, which is what makes a memory-shape fill stop
    /// at the measured capacity, waiting by [`READY_DEADLINE`]. The
    /// memory-shape case and the live Bolt and import tests start their serve
    /// child here. The store is opened as it stands — a caller that wants the
    /// fill to start from an empty store resets the directory first.
    pub(crate) fn spawn_bolt(
        bin: &Path,
        store: &Path,
        logical_graph_size_limit: u64,
    ) -> Result<Self, String> {
        let port = free_port()?;
        let mut cmd = serve_command(bin, store, port);
        cmd.arg("--logical-graph-size-limit")
            .arg(logical_graph_size_limit.to_string());
        Self::spawn(&mut cmd, port, READY_DEADLINE)
    }

    /// Spawn `cmd` — a [`serve_command`] plus the caller's extras — on its
    /// `bolt` port, drain its stderr into the child's log, and wait until the
    /// port accepts a connection within `readiness`. The drained log becomes
    /// the error text when the child dies before binding or the wait times
    /// out.
    pub(crate) fn spawn(cmd: &mut Command, bolt: u16, readiness: Duration) -> Result<Self, String> {
        let (mut child, log) = spawn_draining_stderr(cmd)?;
        let addr = format!("127.0.0.1:{bolt}");
        wait_for_addr(&mut child, &addr, &log, readiness)?;
        Ok(Self {
            child: Mutex::new(child),
            addr,
            readiness,
            log,
        })
    }

    /// The child's pid, for the platform RSS and CPU samplers.
    pub(crate) fn pid(&self) -> u32 {
        self.lock().id()
    }

    /// Wait until a further address the child serves (its `--http` port)
    /// accepts a TCP connect, within the caller's readiness window — the same
    /// evidence path as the spawn's own wait.
    pub(crate) fn wait_for(&self, addr: &str) -> Result<(), String> {
        let mut child = self.lock();
        wait_for_addr(&mut child, addr, &self.log, self.readiness)
    }

    /// Fail unless the child is still running; the failure carries the status
    /// and the drained log.
    pub(crate) fn alive(&self) -> Result<(), String> {
        let mut child = self.lock();
        if let Some(status) = child
            .try_wait()
            .map_err(|err| format!("wait failed: {err}"))?
        {
            return Err(format!(
                "relate serve exited ({status})\n{}",
                self.log_text()
            ));
        }
        Ok(())
    }

    /// The whole drained stderr as one text block.
    pub(crate) fn log_text(&self) -> String {
        drained_text(&self.log)
    }

    /// The log's last [`LOG_TAIL_LINES`] lines — the failure evidence a harness
    /// prints beside its own error without dumping a long serve log.
    pub(crate) fn log_tail(&self) -> String {
        let log = lock_recovered(&self.log);
        let start = log.len().saturating_sub(LOG_TAIL_LINES);
        log[start..].join("\n")
    }

    /// The child's lock — the process state is shared with the `&self` readers
    /// (`pid`, `alive`, `wait_for`) and the `Drop` reap.
    fn lock(&self) -> std::sync::MutexGuard<'_, Child> {
        lock_recovered(&self.child)
    }
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        // `Drop` holds `&mut self`, so this is the guarded state's `get_mut`
        // rather than [`lock_recovered`]'s lock — the same recovery: a
        // poisoned lock only means another holder panicked, and the child is
        // still ours to kill and reap.
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Spawn `cmd` with piped stderr and drain the pipe into a shared log on a
/// reader thread. Read errors are rendered visibly in the log, because the
/// drained log is exactly the failure evidence the readiness wait prints.
fn spawn_draining_stderr(cmd: &mut Command) -> Result<(Child, ServeLog), String> {
    let mut child = cmd
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", cmd.get_program().to_string_lossy()))?;
    let pipe = child.stderr.take().ok_or("stderr pipe missing")?;
    let log = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&log);
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let line = line.unwrap_or_else(|err| format!("<stderr read error: {err}>"));
            lock_recovered(&writer).push(line);
        }
    });
    Ok((child, log))
}

/// Take `m`'s lock, recovering a poisoned one — the one spelling of the
/// poison-recovery policy: poisoning only means a holder panicked, the guarded
/// state is intact, and the drained log is exactly the evidence a failure path
/// prints.
fn lock_recovered<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The drained log as one text block, recovering a poisoned lock the same way
/// the drain thread does — the evidence must survive into whatever error text
/// carries it.
fn drained_text(log: &Mutex<Vec<String>>) -> String {
    lock_recovered(log).join("\n")
}

/// Wait until `addr` accepts a TCP connect while `child` stays alive. If the
/// child exits first, fail carrying the drained log; if the deadline passes,
/// kill the child and fail carrying the same log — one evidence path for both
/// failure modes.
fn wait_for_addr(
    child: &mut Child,
    addr: &str,
    log: &Mutex<Vec<String>>,
    limit: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "relate serve exited before bind ({status})\n{}",
                drained_text(log)
            ));
        }
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!(
        "relate serve did not accept connections on {addr}\n{}",
        drained_text(log)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live session against a fresh store's serve child under `limit`, or
    /// `None` (with the skip printed) when no `relate` binary is configured.
    ///
    /// The store directory must outlive the server, so the fixture owns the
    /// ordering instead of leaving it to a tuple's or a pattern's order: the
    /// fields drop in declaration order, and `_server` is declared before
    /// `_dir` so the child is killed and reaped while its store directory is
    /// still there. Both are held for their lifetimes and never read — that is
    /// the whole fixture.
    struct LiveServe {
        _server: ServeChild,
        session: relate_test_support::bolt::client::Session,
        _dir: tempfile::TempDir,
    }

    fn live_serve(limit: u64) -> Option<LiveServe> {
        let Some(bin) = relate_test_support::relate_bin() else {
            eprintln!(
                "skipping the live Bolt session test: {}",
                relate_test_support::relate_bin_missing_message()
            );
            return None;
        };
        let dir = tempfile::tempdir().unwrap();
        let server = ServeChild::spawn_bolt(&bin, &dir.path().join("store"), limit).unwrap();
        let session = relate_test_support::bolt::client::Session::connect(&server.addr).unwrap();
        Some(LiveServe {
            _server: server,
            session,
            _dir: dir,
        })
    }

    /// A live session against a spawned `relate serve`: the 5.4
    /// negotiation, one parameterized statement, one pipelined batch, and a
    /// batch whose failure the drain absorbs as IGNORED pairs before its
    /// RESET recovers the session.
    #[test]
    fn live_session_runs_parameterized_and_pipelined_statements() {
        let Some(live) = live_serve(1 << 30) else {
            return;
        };
        // The session is moved out; `live` keeps the child and its store
        // directory and drops whole, in declaration order (`server` before
        // `dir`), so the child is reaped while the directory still exists.
        let mut session = live.session;
        assert_eq!(
            session.version(),
            relate_test_support::bolt::client::BoltVersion::V5_4,
            "the server speaks both versions and the client proposes 5.4 first"
        );

        session
            .run_with_params("CREATE (:ParamProbe {id: $id})", &[("id", 7)])
            .unwrap();
        assert_eq!(
            session
                .run_records("MATCH (n:ParamProbe) RETURN n")
                .unwrap(),
            1
        );
        // The count probe: one row, one integer, read as the value itself.
        assert_eq!(
            session
                .run_count("MATCH (n:ParamProbe) RETURN count(n)")
                .unwrap(),
            1
        );
        assert_eq!(
            session
                .run_count("MATCH ()-[r:Missing]->() RETURN count(r)")
                .unwrap(),
            0
        );

        // One batch: every pair queued, one flush, one drain.
        for id in 0..3 {
            session
                .enqueue_run_pull("CREATE (:BatchProbe {id: $id})", &[("id", id)])
                .unwrap();
        }
        session.flush().unwrap();
        let batch = session.drain_batch().unwrap();
        assert_eq!(batch.errors, 0);
        assert!(batch.failure.is_none());
        assert_eq!(batch.applied(), 3);
        // ADR-0642: the batch commits once, at its end, so no statement
        // carries a bookmark of its own.
        assert_eq!(batch.bookmarks(), &[None, None, None]);
        assert_eq!(
            session
                .run_records("MATCH (n:BatchProbe) RETURN n")
                .unwrap(),
            3
        );

        // The middle pair's RUN fails: its PULL and the pair queued behind it
        // answer IGNORED, and the drain's RESET leaves the session usable.
        session
            .enqueue_run_pull("CREATE (:BatchProbe {id: $id})", &[("id", 99)])
            .unwrap();
        session.enqueue_run_pull("NOT A STATEMENT", &[]).unwrap();
        session
            .enqueue_run_pull("CREATE (:BatchProbe {id: $id})", &[("id", 100)])
            .unwrap();
        session.flush().unwrap();
        let drained = session.drain_batch().unwrap();
        assert_eq!(drained.applied(), 1);
        assert_eq!(drained.errors, 2);
        assert!(!drained.failure.unwrap().is_empty());
        assert_eq!(
            session
                .run_records("MATCH (n:BatchProbe) RETURN n")
                .unwrap(),
            4
        );
        session.goodbye().unwrap();
    }

    /// ADR-0642: one framed message batch's pipelined CREATE-only RUN+PULL
    /// pairs commit once, at the batch's end. The wire proves it by
    /// bookmarks: the three batched statements answer no bookmark of their
    /// own, and the MERGE that follows — which cannot join the batch and so
    /// commits on its own — advances the commit sequence by exactly two (the
    /// batch's one commit plus its own).
    #[test]
    fn a_pipelined_create_batch_commits_once() {
        let Some(live) = live_serve(1 << 30) else {
            return;
        };
        // The session is moved out; `live` keeps the child and its store
        // directory and drops whole, in declaration order (`server` before
        // `dir`), so the child is reaped while the directory still exists.
        let mut session = live.session;
        // A MERGE is not CREATE-only: it commits alone, and its bookmark is
        // the reference point.
        session
            .enqueue_run_pull("MERGE (:Before {id: 0})", &[])
            .unwrap();
        session.flush().unwrap();
        let probe = session.drain_batch().unwrap();
        let first = relate_test_support::bolt::client::bookmark_sequence(
            probe.bookmarks()[0]
                .as_deref()
                .expect("the probe statements commit writes"),
        );

        for id in 0..3 {
            session
                .enqueue_run_pull("CREATE (:BatchCommit {id: $id})", &[("id", id)])
                .unwrap();
        }
        session.flush().unwrap();
        let batch = session.drain_batch().unwrap();
        assert_eq!(batch.applied(), 3);
        assert_eq!(batch.bookmarks(), &[None, None, None]);

        session
            .enqueue_run_pull("MERGE (:After {id: 0})", &[])
            .unwrap();
        session.flush().unwrap();
        let after = session.drain_batch().unwrap();
        let last = relate_test_support::bolt::client::bookmark_sequence(
            after.bookmarks()[0]
                .as_deref()
                .expect("the probe statements commit writes"),
        );
        assert_eq!(
            last - first,
            2,
            "the batch is one commit: {first:?} -> {last:?}"
        );
        assert_eq!(
            session
                .run_count("MATCH (n:BatchCommit) RETURN count(n)")
                .unwrap(),
            3,
            "the batch's statements all landed"
        );
        session.goodbye().unwrap();
    }

    /// The pipelined-import rate (ADR-0642): 429 UNWIND-CREATE statements of
    /// 200 rows each — the rebaseline-load identity load's statement shape —
    /// flushed as one pipeline, timed from the first write to the last drained
    /// summary. Set `RELATE_BIN` to compare builds; run with
    /// `--ignored --nocapture`.
    #[test]
    #[ignore = "measurement, not a gate"]
    fn measure_pipelined_import_rate() {
        measure_pipelined(
            "UNWIND $ids AS id CREATE (:Load {id: id})",
            429,
            200,
            |base| {
                let ids: Vec<relate_test_support::bolt::Pack> = (0..200)
                    .map(|r| relate_test_support::bolt::Pack::Int(base + r))
                    .collect();
                vec![(
                    relate_test_support::bolt::Pack::String("ids".into()),
                    relate_test_support::bolt::Pack::List(ids),
                )]
            },
        );
    }

    /// The commit-dominated twin (ADR-0642): 20 000 single-row CREATEs,
    /// where a statement's own work is far below its commit. Same knobs as
    /// [`measure_pipelined_import_rate`].
    #[test]
    #[ignore = "measurement, not a gate"]
    fn measure_pipelined_small_statement_rate() {
        measure_pipelined("CREATE (:Row {id: $id})", 20_000, 1, |base| {
            relate_test_support::bolt::client::int_params(&[("id", base)])
        });
    }

    /// Flush `statements` RUN+PULL pairs of `rows` rows each as one pipeline
    /// and print the rate over the whole exchange. `params` builds each
    /// statement's own parameter map from its first row id.
    fn measure_pipelined(
        cypher: &str,
        statements: usize,
        rows: i64,
        params: impl Fn(
            i64,
        ) -> Vec<(
            relate_test_support::bolt::Pack,
            relate_test_support::bolt::Pack,
        )>,
    ) {
        let Some(live) = live_serve(1 << 34) else {
            panic!("set RELATE_BIN to compare builds");
        };
        let mut session = live.session;
        let started = std::time::Instant::now();
        for s in 0..statements {
            let base = s as i64 * rows;
            session.enqueue_run_pull_pack(cypher, params(base)).unwrap();
        }
        session.flush().unwrap();
        let drained = session.drain_batch().unwrap();
        let elapsed = started.elapsed();
        assert_eq!(drained.applied(), statements, "{:?}", drained.failure);
        let statements_per_s = statements as f64 / elapsed.as_secs_f64();
        println!(
            "pipelined import: {statements} statements ({} rows) in {:.1} ms = {:.0} statements/s, {:.0} rows/s",
            statements as i64 * rows,
            elapsed.as_secs_f64() * 1e3,
            statements_per_s,
            statements_per_s * rows as f64
        );
    }

    /// Two ports reserved together stay distinct. Reserved one after
    /// another, Linux can reissue the port the first probe just released.
    #[test]
    fn free_ports_stay_distinct() {
        for _ in 0..32 {
            let ports = free_ports(2).expect("two ports");
            assert_ne!(ports[0], ports[1], "both probes got {}", ports[0]);
        }
    }

    /// The base argv is pinned once: every harness serve child starts from
    /// exactly this shape, with the bolt port in argv form.
    #[test]
    fn serve_command_carries_the_base_argv() {
        let cmd = serve_command(Path::new("/bin/relate"), Path::new("/store"), 7687);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "serve",
                "--database",
                "/store",
                "--host",
                "127.0.0.1",
                "--bolt",
                "7687",
                "--auth",
                "none"
            ]
        );
    }

    /// The base construction applies the shared env policy
    /// (`relate_test_support::harness_child_env`), so no serve spawn
    /// can forget the ADR-0079 §6 opt-out or the knob scrub.
    #[test]
    fn serve_command_applies_the_shared_env_policy() {
        let cmd = serve_command(Path::new("/bin/relate"), Path::new("/store"), 7687);
        let opt_out = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("DO_NOT_TRACK"))
            .and_then(|(_, v)| v);
        assert_eq!(opt_out, Some(std::ffi::OsStr::new("1")));
    }
}
