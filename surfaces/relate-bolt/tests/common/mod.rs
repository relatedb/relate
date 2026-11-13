//! Shared harness for the `relate-bolt` end-to-end tests: run the real
//! `serve(&Args)` listener in-process on a free port, wait for it to bind
//! and finish serving the readiness probe, and keep its serve-thread result
//! for a fast startup-crash report.
//!
//! The standalone `relate-bolt` binary is retired (LABS-381, ADR-0368): `relate
//! serve` is the one product binary, and this crate's e2e roots exercise the
//! crate's own listener — including the flags the serve CLI does not carry
//! (TLS, `--no-websocket`, the connection timeouts) — by driving
//! `relate_bolt::serve` on a thread of the test process instead of
//! spawning the removed binary. The wire bytes come from the canonical
//! `src/packstream.rs` through the crate's library target, so integration
//! tests link the lib directly and no source vendoring is needed. This
//! module re-exports the library's `packstream` module as `common::packstream`
//! for the e2e roots.
//!
//! Two coverage notes the in-process shape carries: the serve thread cannot
//! be stopped from the outside (a `Server` dropped by a test keeps serving
//! on its own free port until the test process exits — each server owns its
//! own store, so nothing a later server needs is held back), and the
//! server's stderr is the test process's stderr, so no banner or log-line
//! assertions are possible here; the auth root reads the generated token
//! from the persisted `<store>.auth-token` file instead of the banner.
//!
//! # Helper convention
//!
//! Helpers shared by every e2e root: `Server` / `start_server` (with the
//! private `boot` and `wait_for_listen` internals), the
//! `packstream` re-export, and `send`, `read_message` and `is_success`.
//! `handshake`, `MAGIC` and `VERSION_4_4` serve the roots that drive the
//! handshake themselves (`e2e_auth`, `e2e_websocket`). The `bolt_compat`
//! script replayer (ADR-0304) takes `start_server`, the `packstream`
//! re-export and `MAGIC` only: it reports every wire failure at the script
//! line that caused it, so its `compat` runner drives the fallible
//! primitives instead of the panicking `send` / `read_message`.
//!
//! A single-consumer transport helper is permitted in this harness: when
//! exactly one e2e root drives an exchange, the helper lives here rather
//! than in that root's file, because transport helpers belong in the
//! shared harness. Since the other roots compile this module too, each
//! such helper carries an `#[allow(dead_code)]` explained by a comment
//! naming its consumer — a sanctioned exception, not an oversight. Each
//! e2e root compiles this module on its own, so `handshake`,
//! `connect_and_hello` / `try_connect_and_hello` and `run_and_pull` carry
//! the annotation for the roots that do not drive them (their consumers
//! are `e2e_auth`, `e2e_websocket`, `e2e_browser_sidebar` and
//! `concurrency`).

pub(crate) use relate_bolt::packstream;

use clap::Parser;
use relate_bolt::cli_args::Args;
use relate_bolt::packstream::{decode, try_struct_msg, Pack};
use relate_bolt::serve_on;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17];
pub const VERSION_4_4: [u8; 16] = [0, 0, 4, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// A running in-process server: its serve thread plus its bound address.
/// The serve result channel that reports a startup crash is boot-local —
/// once the listener answers, a later crash surfaces as a failed connection,
/// exactly as a dead child process did.
pub struct Server {
    pub addr: String,
}

/// Start the server with the given extra args on a free port.
///
/// The harness binds the port itself and hands the listener to the server,
/// so no other process can take it between a probe and the server's bind
/// (that race let the readiness probe reach a foreign listener, and a
/// wrong-password check then succeeded against it). One retry on a fresh
/// boot covers a loaded machine stalling one boot past the wait, a harness
/// flake rather than a server bug. A genuinely broken startup still fails,
/// with the serve thread's own error from both attempts in the panic message.
pub fn start_server(extra: &[&str]) -> Server {
    match boot(extra, 1) {
        Ok(server) => server,
        Err(first) => match boot(extra, 2) {
            Ok(server) => server,
            Err(second) => panic!("server never bound\n{first}\n{second}"),
        },
    }
}

/// One boot attempt; `Err` carries the attempt's diagnostics on failure.
fn boot(extra: &[&str], attempt: u32) -> Result<Server, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("attempt {attempt}: binding a free port: {e}"))?;
    let bind = listener
        .local_addr()
        .map_err(|e| format!("attempt {attempt}: reading the bound port: {e}"))?
        .to_string();
    // The same flag vocabulary the retired binary parsed, through the same
    // clap definition — a harness argv that fails to parse is a harness bug
    // and panics here rather than silently skipping the server.
    let mut argv = vec![
        "relate-bolt".to_string(),
        "--bind".to_string(),
        bind.clone(),
    ];
    argv.extend(extra.iter().map(|s| s.to_string()));
    let args = Args::try_parse_from(&argv).unwrap_or_else(|e| panic!("harness argv {argv:?}: {e}"));
    let (tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        // A panic in the serve thread must reach the boot failure instead of
        // leaving the harness polling a dead thread for the full timeout
        // (catch_unwind turns it into an error event; the default hook has
        // already printed it).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            serve_on(&args, Some(listener))
        }))
        .unwrap_or_else(|_| Err("the serve thread panicked (payload logged above)".into()));
        // `Ok(())` is a clean return; `Err` is the error the boot report
        // carries.
        let _ = tx.send(result.map_err(|error| error.to_string()));
    });
    if let Err(why) = wait_for_listen(&done, &bind) {
        return Err(format!("attempt {attempt} on {bind}: {why}"));
    }
    Ok(Server { addr: bind })
}

/// The boot error for a serve thread whose verdict arrived before the
/// harness called the port ready (it returned, or its listener was
/// dropped on the way out).
fn early_exit(outcome: Result<(), String>) -> String {
    match outcome {
        Ok(()) => "serve returned before binding".to_string(),
        Err(error) => format!("serve failed before binding: {error}"),
    }
}

/// The outcome of one readiness probe against a bound address.
enum Probe {
    /// The server accepted and closed the probe: ready.
    Ready,
    /// The connection was reset: the serve thread has failed or is about
    /// to report why, so its verdict decides. A thread still live five
    /// seconds later means the reset was not the listener dying, so the
    /// port counts as ready.
    Reset,
}

/// Half-close and drain a connected probe until the peer closes.
/// Half-closing instead of merely dropping the socket matters: reading to
/// peer EOF proves the server-side handler consumed that EOF and closed
/// its stream, so its connection-cap permit is released as the handler
/// thread exits.
fn probe_once(mut probe: TcpStream, deadline: Instant) -> Result<Probe, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    probe
        .set_read_timeout(Some(remaining))
        .map_err(|e| format!("configuring readiness probe: {e}"))?;
    probe
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("closing readiness probe input: {e}"))?;
    let mut discarded = [0u8; 256];
    loop {
        match probe.read(&mut discarded) {
            Ok(0) => return Ok(Probe::Ready),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::NotConnected
                ) =>
            {
                return Ok(Probe::Reset)
            }
            Err(e) => return Err(format!("draining readiness probe: {e}")),
        }
    }
}

/// Poll until the server accepts connections and has closed the readiness
/// probe. Waiting for peer closure matters for capped servers: a successful
/// TCP connect occupies a connection slot until its handler observes EOF and
/// exits, so returning at connect time can make the first real client race
/// that probe (LABS-144).
///
/// A serve thread that returns first is a startup crash — fail fast with its
/// error rather than polling a dead port for the full timeout. The two
/// verdict reads differ on purpose: the poll loop only peeks (`try_recv`,
/// the thread usually has not reported), while a reset probe is evidence
/// the thread is failing and so waits up to 5s for its verdict.
fn wait_for_listen(done: &mpsc::Receiver<Result<(), String>>, addr: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(outcome) = done.try_recv() {
            return Err(early_exit(outcome));
        }
        if let Ok(stream) = TcpStream::connect(addr) {
            match probe_once(stream, deadline)? {
                Probe::Ready => return Ok(()),
                // The harness holds the listener, and `serve` drops it on
                // the way out, so wait for the verdict rather than calling
                // the port ready.
                Probe::Reset => {
                    return match done.recv_timeout(Duration::from_secs(5)) {
                        Ok(outcome) => Err(early_exit(outcome)),
                        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            Err("serve thread vanished without a verdict".to_string())
                        }
                    };
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("still starting after 10s".to_string())
}

pub fn is_success(msg: &[u8]) -> bool {
    matches!(decode(msg), Ok(Pack::Struct { tag: 0x70, .. }))
}

/// Send one chunk-framed Bolt message over any Bolt byte stream — raw TCP,
/// or the `BoltWs` writer whose each write becomes one WebSocket binary
/// message (`try_struct_msg` chunk-frames already: length prefix +
/// terminator).
pub fn send<W: Write>(client: &mut W, tag: u8, fields: Vec<Pack>) {
    client
        .write_all(&try_struct_msg(tag, fields).unwrap())
        .unwrap();
}

/// Negotiate Bolt 4.4 over raw TCP; the server must select it.
// Sanctioned per-root dead code (module convention): only `e2e_auth` and
// `e2e_websocket` drive the handshake themselves; the other roots go
// through `connect_and_hello`.
#[allow(dead_code)]
pub fn handshake(client: &mut TcpStream) {
    client.write_all(&MAGIC).unwrap();
    client.write_all(&VERSION_4_4).unwrap();
    let mut selected = [0u8; 4];
    client.read_exact(&mut selected).unwrap();
    assert_eq!(selected, [0, 0, 4, 4], "server must offer Bolt 4.4");
}

/// Connect to `addr`, set a read timeout, negotiate Bolt 4.4, and complete
/// HELLO (which must succeed); the ready session is returned.
// Sanctioned per-root dead code (module convention): consumed by
// `e2e_browser_sidebar` and `concurrency`, not by the auth/websocket roots.
#[allow(dead_code)]
pub fn connect_and_hello(addr: &str) -> TcpStream {
    try_connect_and_hello(addr).unwrap_or_else(|e| panic!("HELLO exchange on {addr}: {e}"))
}

/// Fallible [`connect_and_hello`]: callers that expect rejection (over-cap
/// probes, slot-recovery retries after a mid-query disconnect) observe the
/// failure instead of panicking.
// Sanctioned per-root dead code (module convention): consumed by
// `concurrency` only.
#[allow(dead_code)]
pub fn try_connect_and_hello(addr: &str) -> std::io::Result<TcpStream> {
    let mut client = TcpStream::connect(addr)?;
    client.set_read_timeout(Some(Duration::from_secs(10)))?;
    client.write_all(&MAGIC)?;
    client.write_all(&VERSION_4_4)?;
    let mut selected = [0u8; 4];
    client.read_exact(&mut selected)?;
    if selected != [0, 0, 4, 4] {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("server selected {selected:?}"),
        ));
    }
    // Bolt 4.4 requires a `user_agent` in the HELLO map (ADR-0313).
    let user_agent = (
        Pack::String("user_agent".into()),
        Pack::String("relate-bolt-e2e/1.0".into()),
    );
    let hello = try_struct_msg(0x01, vec![Pack::Map(vec![user_agent])])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    client.write_all(&hello)?;
    // This helper's contract is fallible (callers expecting rejection
    // observe it instead of panicking), so it calls the canonical fallible
    // reader directly rather than the panicking wrapper above.
    let payload = relate_bolt::protocol::read_message(&mut client)?;
    if !is_success(&payload) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("HELLO rejected: {payload:?}"),
        ));
    }
    Ok(client)
}

/// Read one chunk-framed Bolt message (length-prefixed chunks with a
/// 0x0000 terminator) from a byte stream — a raw TCP socket, or the `BoltWs`
/// reader that flattens WebSocket binary payloads into one stream.
pub fn read_message<R: Read>(client: &mut R) -> Vec<u8> {
    // The crate's canonical fallible chunk-reassembly reader.
    relate_bolt::protocol::read_message(client).unwrap()
}

/// RUN `cypher` with `params`, PULL all records, and return each RECORD's
/// values. Both the RUN reply and the PULL summary must be SUCCESS.
// Sanctioned per-root dead code (module convention): consumed by
// `e2e_browser_sidebar` and `concurrency`, not by the auth/websocket roots.
#[allow(dead_code)]
pub fn run_and_pull(
    client: &mut TcpStream,
    cypher: &str,
    params: Vec<(Pack, Pack)>,
) -> Vec<Vec<Pack>> {
    send(
        client,
        0x10,
        vec![Pack::String(cypher.into()), Pack::Map(params)],
    );
    let run = read_message(client);
    assert!(is_success(&run), "RUN must succeed for {cypher:?}: {run:?}");
    send(
        client,
        0x3F,
        vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
    );
    let mut records = Vec::new();
    loop {
        let msg = read_message(client);
        match decode(&msg).unwrap_or_else(|e| panic!("RECORD decodes: {e}")) {
            Pack::Struct { tag: 0x71, fields } => {
                let Some(Pack::List(values)) = fields.into_iter().next() else {
                    panic!("RECORD carries one list of values");
                };
                records.push(values);
            }
            Pack::Struct { tag: 0x70, .. } => return records,
            other => panic!("expected RECORD or SUCCESS for {cypher:?}, got {other:?}"),
        }
    }
}

/// Raw HTTP WebSocket upgrade request bytes: the one definition lives in
/// the shared test-support crate; re-exported so the e2e roots keep their
/// `common::upgrade_request` path.
// Only e2e_websocket drives the upgrade by hand.
#[allow(unused_imports)]
pub use relate_test_support::ws_upgrade_request as upgrade_request;
