//! The shared `serve` e2e harness (ADR-0232, ADR-0306): the credential
//! every serve suite pins — its Basic header derived from it, never
//! pinned beside it — plus spawn `serve` on ephemeral loopback ports
//! with piped stderr (so tests assert on the banner lines), wait for the
//! ports, and stop the child on SIGINT — or wait for a child that exits by
//! itself (a startup failure) and drain its stderr — one copy of the
//! spawn/port/SIGINT/wait policy, shared by `e2e_serve.rs`,
//! `e2e_serve_bolt.rs`, `e2e_serve_read_only.rs`, and
//! `e2e_serve_durability.rs`.
//!
//! Compiled only when both served surfaces are in the build — the same
//! gate as the suites that use it (a lean bundle's `serve` has no
//! `--http`/`--mcp` to spawn with).

#[cfg(all(unix, any(feature = "bolt", feature = "http")))]
use std::ffi::OsStr;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;

use super::{cli_command, tempdir};
use tempfile::TempDir;

/// The pinned credential every serve suite uses (`neo4j:s3cret`, the
/// same credential the `relate-http` surface tests pin).
pub const AUTH: &str = "neo4j:s3cret";

/// The RFC 7617 Basic header for [`AUTH`]: derived from the credential,
/// never pinned beside it, so the credential stays one fact — rotating it
/// cannot leave a stale blob silently failing every authed request as a
/// confusing 401.
pub fn auth_header() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(AUTH)
    )
}

/// Reserve an ephemeral loopback port for the child to bind. The listener
/// drops before the child starts; the tiny race is the accepted test idiom.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The serve store path under a tempdir (`<dir>/graph`) — one
/// `.expect` for the store path's UTF-8 (the `binary_path_str` policy
/// in `support/mod.rs`, for the store path instead of the binary
/// path). The bolt suite routes every store path through it; the older
/// serve suites still spell the conversion inline, migrating here
/// under REL8-1882.
pub fn graph_db(dir: &TempDir) -> String {
    dir.path()
        .join("graph")
        .to_str()
        .expect("the store path is UTF-8")
        .to_string()
}

/// Wait until both ports accept connections (or fail the test).
pub fn wait_for_ports(ports: &[u16]) {
    let deadline = Instant::now() + Duration::from_secs(30);
    for &port in ports {
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "port {port} never came up");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A spawned `relate serve` plus the tempdir that is its `HOME`: the
/// guard owns the child and the directory for the child's lifetime, so
/// nothing is leaked and a failing assert still reaps the server via
/// [`ServeChild::stop`]. The `Child` is private to the guard — every
/// SIGINT/wait/drain goes through the guard's methods, so no consumer can
/// split the child from the tempdir that must outlive it.
pub struct ServeChild {
    child: Child,
    _home: TempDir,
}

/// The one spawn-policy core both spawners call: create the tempdir that
/// is the child's `HOME`, apply it plus the Stdio triple (the caller's
/// `stdin` — null for every serve that exits on signals or its own
/// lifecycle, held-open piped for the mixed stdio run the disposition
/// pin keeps alive — null stdout, piped stderr), spawn, and wrap the
/// child in the [`ServeChild`] guard. Callers prepare args and any
/// extra env on `cmd` first.
fn spawn_with_home(cmd: &mut Command, stdin: Stdio) -> ServeChild {
    let home = tempdir();
    cmd.env("HOME", home.path())
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    ServeChild {
        child: cmd.spawn().unwrap(),
        _home: home,
    }
}

/// Spawn `relate serve` with the auth env scrubbed unless the caller
/// sets it (via `auth`: `Some("user:password")` sets `RELATE_AUTH`,
/// `None` removes it so the generated-token path runs).
pub fn spawn_serve(db: &str, http_port: u16, mcp_port: u16, auth: Option<&str>) -> ServeChild {
    spawn_serve_with(db, http_port, mcp_port, auth, &[])
}

/// The both-surface serve command every spawner starts from — the one
/// copy of the argument assembly, so a second spawner cannot drift on the
/// base shape.
fn serve_command(db: &str, http_port: u16, mcp_port: u16, extra: &[&str]) -> Command {
    let mut cmd = cli_command();
    cmd.args([
        "serve",
        "--database",
        db,
        "--http",
        &http_port.to_string(),
        "--mcp",
        &mcp_port.to_string(),
    ])
    .args(extra);
    cmd
}

/// The auth env policy every spawner applies — the one copy of the
/// `Some` sets / `None` scrubbed fold.
fn apply_auth(cmd: &mut Command, auth: Option<&str>) {
    match auth {
        Some(cred) => {
            cmd.env("RELATE_AUTH", cred);
        }
        None => {
            cmd.env_remove("RELATE_AUTH");
        }
    }
}

/// [`spawn_serve`] plus extra trailing arguments (e.g. `--bolt PORT`,
/// `--bolt-identity STRING`, `--read-only[=snapshot|live]`,
/// `--durability-mode MODE`).
pub fn spawn_serve_with(
    db: &str,
    http_port: u16,
    mcp_port: u16,
    auth: Option<&str>,
    extra: &[&str],
) -> ServeChild {
    spawn_serve_with_env(db, http_port, mcp_port, auth, extra, &[])
}

/// [`spawn_serve_with`] plus extra child-only environment entries — the
/// serve suites' one way to set a variable the spawn policy does not own
/// (a `RELATE_DURABILITY_MODE` value, ADR-0470) without each test
/// hand-rolling its own spawn.
pub fn spawn_serve_with_env(
    db: &str,
    http_port: u16,
    mcp_port: u16,
    auth: Option<&str>,
    extra: &[&str],
    envs: &[(&str, &str)],
) -> ServeChild {
    let mut cmd = serve_command(db, http_port, mcp_port, extra);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    apply_auth(&mut cmd, auth);
    spawn_with_home(&mut cmd, Stdio::null())
}

/// Spawn the mixed run — the HTTP listener beside the foreground stdio
/// transport (`--mcp stdio`) — with its stdin held OPEN: the stdio
/// transport blocks reading it, so the run stays alive until a signal
/// lands, which is exactly what the mixed-run disposition pin asserts
/// (SIGINT must still terminate it by default disposition; the guard's
/// child owns the open pipe, so the run lives as long as the guard).
#[cfg(all(unix, feature = "mcp"))]
pub fn spawn_mixed_stdio_serve(db: &str, http_port: u16) -> ServeChild {
    let mut cmd = cli_command();
    cmd.args([
        "serve",
        "--database",
        db,
        "--http",
        &http_port.to_string(),
        "--mcp",
        "stdio",
    ]);
    apply_auth(&mut cmd, None);
    spawn_with_home(&mut cmd, Stdio::piped())
}

/// Spawn `serve` with one surface invocation (`surface`, e.g.
/// `["--bolt", "0"]` or `["--mcp", "stdio"]`) and one
/// deliberately malformed env var: `key` set to `value` on the child's
/// environment only, never the test process, so the pin stays race-free
/// under the parallel harness. The shared core of the startup-ordering
/// regression pins: the malformed read must fail before auth's
/// generated-token path can persist `<store>.auth-token`, so the child
/// exits 1 leaving no token file. `RELATE_AUTH` is removed so the
/// generated-token path is the one auth would take.
#[cfg(all(unix, any(feature = "bolt", feature = "http", feature = "mcp")))]
fn spawn_malformed_env(
    db: &str,
    surface: &[&str],
    key: &str,
    value: impl AsRef<OsStr>,
) -> ServeChild {
    let mut cmd = cli_command();
    cmd.args(["serve", "--database", db])
        .args(surface)
        .env(key, value.as_ref())
        .env_remove("RELATE_AUTH");
    spawn_with_home(&mut cmd, Stdio::null())
}

/// Spawn `serve --bolt 0` with a deliberately malformed
/// `RELATE_IDENTITY` — a non-UTF-8 value. The startup-ordering
/// regression pin: the identity read must fail before auth's
/// generated-token path can persist `<store>.auth-token`, so the child
/// exits 1 leaving no token file. The spawn policy (tempdir `HOME`,
/// scrubbed `RELATE_AUTH`, the Stdio triple) is
/// [`spawn_malformed_env`]'s.
#[cfg(all(unix, feature = "bolt"))]
pub fn spawn_serve_with_malformed_identity(db: &str) -> ServeChild {
    use std::os::unix::ffi::OsStrExt;

    let value = OsStr::from_bytes(b"\xff");
    spawn_malformed_env(db, &["--bolt", "0"], "RELATE_IDENTITY", value)
}

/// Spawn `serve --bolt 0` with a deliberately malformed
/// `RELATE_MAX_CONNECTIONS` — a non-numeric value. The
/// startup-ordering regression pin, the malformed-identity shape
/// mirrored: the connection-cap resolution must fail before auth's
/// generated-token path can persist `<store>.auth-token`, so the child
/// exits 1 leaving no token file. The spawn policy (tempdir `HOME`,
/// scrubbed `RELATE_AUTH`, the Stdio triple) is
/// [`spawn_malformed_env`]'s.
#[cfg(all(unix, feature = "bolt"))]
pub fn spawn_serve_with_malformed_max_connections(db: &str) -> ServeChild {
    spawn_malformed_env(
        db,
        &["--bolt", "0"],
        "RELATE_MAX_CONNECTIONS",
        "not-a-count",
    )
}

/// Spawn `serve --http 0` with a deliberately malformed
/// `RELATE_TX_IDLE_TIMEOUT_SECS` — a non-numeric value (ADR-0435 §4).
/// The startup-ordering regression pin: the managed-transaction limits
/// resolve before auth's generated-token path, so the child exits 1
/// leaving no token file.
#[cfg(all(unix, feature = "http"))]
pub fn spawn_serve_with_malformed_tx_idle_timeout(db: &str) -> ServeChild {
    spawn_malformed_env(
        db,
        &["--http", "0"],
        "RELATE_TX_IDLE_TIMEOUT_SECS",
        "not-a-timeout",
    )
}

/// Spawn `serve --database :memory: --mcp stdio` with one deliberately
/// malformed env var. The stdio-only startup-ordering pin: the admission
/// knobs resolve before the stdio early return, so a malformed
/// cap/deadline variable still fails startup with exit 1. The spawn policy
/// (tempdir `HOME`, scrubbed `RELATE_AUTH`, the Stdio triple, the
/// child-only environment) is [`spawn_malformed_env`]'s.
#[cfg(unix)]
pub fn spawn_stdio_with_malformed_env(key: &str) -> ServeChild {
    spawn_malformed_env(":memory:", &["--mcp", "stdio"], key, "soon")
}

/// The assert half of the malformed-env setup-ordering pins: the child
/// `spawn` starts fails startup with exit 1, the `needle` names the
/// malformed variable as the reported failure, and — because auth must
/// not have run yet — no token file is left behind. One copy of the
/// wait/exit/needle/token policy beside [`spawn_malformed_env`]'s spawn
/// half, so the per-variable tests (identity, max-connections, tx idle
/// timeout — across the serve suites) state only their spawn helper,
/// needle, and label.
#[cfg(unix)]
pub fn malformed_env_fails_before_auth(
    db: &str,
    spawn: fn(&str) -> ServeChild,
    needle: &str,
    label: &str,
) {
    let mut child = spawn(db);
    let (status, stderr) = child.wait_and_drain_stderr();
    assert_eq!(status.code(), Some(1), "serve must exit 1: {stderr}");
    assert!(
        stderr.contains(needle),
        "the malformed {label} must be the reported failure: {stderr}"
    );
    let token = relate_auth::token_path(db)
        .expect("the temp store path is a local URI")
        .expect("a persistent store has a token path");
    assert!(
        !token.exists(),
        "a malformed {label} must fail before auth persists {}",
        token.display()
    );
}

impl ServeChild {
    /// Wait for the child to exit — the harness's one kill-and-panic
    /// deadline (30 s), so a serve that never exits must not hang the
    /// suite. `what` names the wait in the panic ("did not exit on
    /// SIGINT", "did not stop").
    fn wait_for_exit(&mut self, what: &str) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.child.try_wait().unwrap() {
                Some(status) => return status,
                None => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        panic!("serve {what}");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// Send the child SIGINT and wait for the exit — the one unsafe kill
    /// plus the bounded wait, shared by [`Self::sigint_and_drain_stderr`]
    /// and [`Self::stop`]'s unix arm. `what` names the wait in the
    /// deadline panic ("did not exit on SIGINT", "did not stop").
    #[cfg(unix)]
    fn sigint_and_wait(&mut self, what: &str) -> std::process::ExitStatus {
        // SAFETY: signaling our own spawned child process by pid.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGINT) };
        self.wait_for_exit(what)
    }

    /// Drain the piped stderr for assertions. Called once the child has
    /// exited (SIGINT'd or self-exited): the pipe hits EOF when the
    /// process is gone, so the read completes.
    fn drain_stderr(&mut self) -> String {
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .expect("serve stderr is piped")
            .read_to_string(&mut stderr)
            .expect("drain serve stderr");
        stderr
    }

    /// SIGINT the child, wait for the exit, and drain its stderr for
    /// assertions. A hang is a failure with the same kill-and-panic
    /// fallback [`Self::stop`] has — a server that misses SIGINT must not
    /// hang the suite. Returns the exit status for shutdown-path
    /// assertions.
    #[cfg(unix)]
    pub fn sigint_and_drain_stderr(&mut self) -> (std::process::ExitStatus, String) {
        let status = self.sigint_and_wait("did not exit on SIGINT");
        (status, self.drain_stderr())
    }

    /// Wait for a serve child that exits by itself — a startup failure
    /// (open, bind) never binds a port — and drain its stderr for
    /// assertions. The same kill-and-panic deadline: a serve that never
    /// exits must not hang the suite.
    pub fn wait_and_drain_stderr(&mut self) -> (std::process::ExitStatus, String) {
        let status = self.wait_for_exit("did not exit");
        (status, self.drain_stderr())
    }

    /// Kill the child if it is somehow still up (a failing assert above
    /// must not leak a server), and reap it. The `HOME` tempdir drops with
    /// the consumed guard, so nothing is leaked per spawned server.
    pub fn stop(mut self) {
        #[cfg(unix)]
        {
            let _ = self.sigint_and_wait("did not stop");
        }
        // Windows has no SIGINT for a console process we did not spawn
        // into a process group; stop() is cleanup for tests that assert
        // nothing about the exit path, so hard-kill and reap instead of
        // polling to a panic.
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
