//! The extension mount's process half (ADR-0447): spawning the child over
//! std pipes, the one-line request envelope, the read deadline, the exit
//! classification, and the mount's caps.
//!
//! A command-kind extension is a child process the host runs to walk an
//! external source while the host alone owns the `Database` (LABS-466).
//! [`spawn_piped`] runs the extension's executable over std pipes — no
//! shell, no argv — and [`write_envelope_line`] hands it exactly one request
//! envelope: its resolved flag values as a parsed JSON object, never raw
//! argv and never a store path (ADR-0430 §6). All store writes flow through
//! the host-owned `Database`; the child's only power is the MCP requests it
//! writes (`extensions::mcp`, ADR-0442).
//!
//! The manifest's `protocol` names the MCP revision this mount serves
//! ([`relate_wire::mcp::MCP_PROTOCOL_VERSION`]) — one protocol fact, not
//! two: the value the manifest declares is the `initialize` handshake's
//! `protocolVersion`. [`Caps`] states the mount's caps once for both
//! directions of the pipe.
//!
//! Every [`ProtocolError`] renders as one `Error:` line and exits 1
//! (ADR-0430 §5): a spawn failure, a broken envelope write, a non-zero child
//! exit, a killed child — runtime failures, rendered through `print_error`'s
//! sanitization so a child can never forge a second line, ANSI, or control
//! characters into the host's output. The exit-2 usage class (manifest and
//! flag problems, ADR-0429 §5) is the registry's and the host parser's, not
//! this module's.

use std::io::BufReader;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Map, Value};

use relate_wire::mcp::CapsFacts;
use relate_wire::read_cap::{drain_stderr, write_line};

// The mount's caps, stated once for both sides of the pipe (ADR-0430 §4):
// the wire kit (`relate-wire`) owns the numbers — the host enforces
// them, the first-party children chunk and refuse against them — and this
// module re-exports them as the host's admission vocabulary.
pub(crate) use relate_wire::{
    MAX_ROWS_PER_STATEMENT, MAX_STATEMENTS, MAX_STATEMENT_BYTES, MAX_STREAM_BYTES,
};

/// How long the mount's handshake window waits for the child's first request
/// line — the read bound before `initialize` arrives (ADR-0442's deferred
/// read deadline, closed here). A child that takes its envelope and then
/// holds its pipes without writing its first request is stalled, not slow:
/// the read fails the run with its own refusal instead of hanging the host.
/// Once the first request has established the session, the mount reads
/// without a bound, because a working child may do arbitrarily expensive
/// work before its first tool call. The `check` verb uses the same value as
/// its per-stage bound (its tests substitute a short one); as a diagnostic
/// it bounds every stage, where the mount bounds only the handshake — the
/// two differ deliberately.
pub(crate) const READ_DEADLINE: Duration = Duration::from_secs(10);

/// The host-side resource caps one extension run admits (ADR-0430 §4,
/// ADR-0377's semantics): every bound a named constant, every refusal a
/// typed error with pinned text a test asserts.
///
/// [`Caps::MOUNT`] is the shipped set; tests construct tight values so one
/// scripted call crosses each bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    /// The per-statement byte cap: one statement's bytes, and — in the
    /// response direction — the most bytes one result line may carry. The
    /// request line is read under the framing bound derived from it
    /// ([`Self::mcp_request_bytes`]), so a statement at this cap always fits
    /// one request.
    pub(crate) statement_bytes: u64,
    /// The rows-per-statement cap: the most parameter rows one
    /// `write_cypher` call may carry.
    pub(crate) rows_per_statement: usize,
    /// The statements-per-call cap: the most statements one `batch_cypher`
    /// call admits.
    pub(crate) statements: usize,
    /// The whole-run stream cap: the most JSON-RPC bytes a `whole-import`
    /// session's request stream may carry, counted line by line before
    /// decode (ADR-0598). A `per-batch` session runs uncapped across the
    /// session — this number does not apply to it.
    pub(crate) stream_bytes: u64,
}

impl Caps {
    /// The shipped caps (ADR-0430 §4) — the wire kit's stated numbers,
    /// enforced here (the one home both sides read).
    pub(crate) const MOUNT: Self = Self {
        statement_bytes: MAX_STATEMENT_BYTES,
        rows_per_statement: MAX_ROWS_PER_STATEMENT,
        statements: MAX_STATEMENTS,
        stream_bytes: MAX_STREAM_BYTES,
    };

    /// The mount's per-request framing cap: the statement cap plus the wire
    /// kit's JSON envelope allowance, from the kit's one derivation. The
    /// session's capped line read and the published
    /// [`CapsFacts::max_request_bytes`] both use this, so a statement at the
    /// published statement cap always fits one request line.
    pub(crate) fn mcp_request_bytes(self) -> u64 {
        relate_wire::mcp::request_bytes_cap(self.statement_bytes)
    }

    /// The most bytes one MCP *response* line may carry: the statement cap
    /// itself — the same bound the child's capped response read runs under,
    /// so the host can never write a line its child would refuse as
    /// oversized. The request direction is the framing bound above, which
    /// leaves room for the statement's JSON escaping.
    pub(crate) fn mcp_response_bytes(self) -> u64 {
        self.statement_bytes
    }

    /// The MCP `initialize` cap facts: the caps under the names the wire
    /// publishes, plus the framing bound above.
    pub(crate) fn mcp_facts(self) -> CapsFacts {
        CapsFacts {
            max_statement_bytes: self.statement_bytes,
            max_rows_per_statement: self.rows_per_statement,
            max_statements: self.statements,
            max_stream_bytes: self.stream_bytes,
            max_request_bytes: self.mcp_request_bytes(),
        }
    }
}

/// A refused extension run (ADR-0430 §2, §4, §5).
///
/// The refusals are typed variants with pinned texts, the `HostError`
/// pattern's sibling on the wire: the CLI renders each as one `Error:` line
/// and exits 1, and tests assert the wording here in one place. The child's
/// own words (a non-zero exit's stderr) ride the variants that collected
/// them; they are rendered through `print_error`'s sanitization, never raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProtocolError {
    /// The extension's executable was not found or is not executable.
    NotExecutable {
        /// The executable path, as the manifest resolved it.
        exe: String,
    },
    /// Spawning the child failed for another reason.
    SpawnFailed(String),
    /// Writing the request envelope to the child's stdin failed — the
    /// child died before the handshake, or the pipe broke.
    EnvelopeWriteFailed(String),
    /// Waiting for the child's exit failed.
    WaitFailed(String),
    /// The child's stderr drain thread failed before it could hand over
    /// what it retained.
    StderrJoinFailed,
    /// The child exited non-zero — its stderr rides along, trimmed.
    ExitFailed {
        /// The child's exit code.
        code: i32,
        /// The child's trimmed stderr, empty when it said nothing.
        stderr: String,
    },
    /// The child died to a signal before finishing its session — the
    /// killed-child runtime failure (ADR-0430 §4–§5). On unix the signal
    /// is named; where the platform does not name it, `None`.
    KilledBySignal {
        /// The signal the child died to, where the platform names one.
        signal: Option<i32>,
        /// The child's trimmed stderr, empty when it said nothing.
        stderr: String,
    },
}

/// The child's stderr as a failure line's suffix — the one rendering
/// every variant that carries a child's stderr appends: nothing when the
/// child said nothing, `: {stderr}` when it spoke. The two variants that
/// render it ([`ProtocolError::ExitFailed`] and
/// [`ProtocolError::KilledBySignal`]) share this, so their tails cannot
/// drift apart.
fn write_stderr_suffix(f: &mut std::fmt::Formatter<'_>, stderr: &str) -> std::fmt::Result {
    match stderr {
        "" => Ok(()),
        stderr => write!(f, ": {stderr}"),
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotExecutable { exe } => {
                write!(
                    f,
                    "the extension executable {exe} was not found or is not executable"
                )
            }
            Self::SpawnFailed(detail) => write!(f, "failed to spawn the extension child: {detail}"),
            Self::EnvelopeWriteFailed(detail) => write!(
                f,
                "failed to send the request envelope to the extension child: {detail}"
            ),
            Self::WaitFailed(detail) => {
                write!(f, "failed to wait for the extension child: {detail}")
            }
            Self::StderrJoinFailed => f.write_str("failed to collect the extension child's stderr"),
            Self::ExitFailed { code, stderr } => {
                write!(f, "the extension child exited with {code}")?;
                write_stderr_suffix(f, stderr)
            }
            Self::KilledBySignal { signal, stderr } => {
                match signal {
                    Some(signal) => write!(
                        f,
                        "the extension child was killed by signal {signal} before finishing its MCP session"
                    )?,
                    None => f.write_str(
                        "the extension child was killed before finishing its MCP session",
                    )?,
                }
                write_stderr_suffix(f, stderr)
            }
        }
    }
}

/// Kill and reap a child — the one statement of the mount's reaping
/// policy (ADR-0377: nothing more is read, the child never outlives the
/// error it caused), consulted by the spawn failure path, the refusal
/// path, and `Drop`.
pub(crate) fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A child spawned over the mount's std pipes, before the envelope wrote
/// anything: the process, its three handles, and the stderr drain already
/// running. The one spawn the mount uses, so the pipe setup — no shell, no
/// argv, a drained stderr — has one home.
pub(crate) struct PipedChild {
    /// The spawned process.
    pub(crate) child: Child,
    /// The child's stdin, still open: the caller writes the envelope and
    /// keeps it open — the child's responses arrive there.
    pub(crate) stdin: std::process::ChildStdin,
    /// The child's stdout, capped-read ready.
    pub(crate) stdout: BufReader<ChildStdout>,
    /// The stderr drain, joined when the child is reaped.
    pub(crate) stderr_drain: Drain,
}

/// Spawn `exe` over the mount's three std pipes and start the stderr drain
/// (ADR-0430 §1, ADR-0377's drain rule). Existence is spawn-time, so a
/// missing or non-executable path is the typed
/// [`ProtocolError::NotExecutable`] the mount renders. `current_dir` is the
/// child's working directory: `None` inherits the host's — what the `run`
/// mount passes, so the child sees the user's invocation cwd — and a caller
/// that owns a stable directory (the tests) pins it there, so no transient
/// host-side cwd swap can reach the child.
pub(crate) fn spawn_piped(
    exe: &Path,
    current_dir: Option<&Path>,
) -> Result<PipedChild, ProtocolError> {
    let mut command = Command::new(exe);
    if let Some(dir) = current_dir {
        command.current_dir(dir);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            ProtocolError::NotExecutable {
                exe: exe.display().to_string(),
            }
        }
        _ => ProtocolError::SpawnFailed(e.to_string()),
    })?;
    let stdin = child
        .stdin
        .take()
        .expect("a child spawned with piped stdin has a stdin handle");
    let stdout = child
        .stdout
        .take()
        .expect("a child spawned with piped stdout has a stdout handle");
    let stderr = child
        .stderr
        .take()
        .expect("a child spawned with piped stderr has a stderr handle");
    // Drain stderr on its own thread before anything can block: the
    // child must never stall on a full stderr pipe while the host is
    // writing the envelope or waiting for requests (the `git log` reader's
    // rule, ADR-0377; the drain and its bound are `read_cap`'s one
    // shared helper).
    let stderr_drain = Drain::new(std::thread::spawn(move || {
        drain_stderr(stderr, "extension")
    }));
    Ok(PipedChild {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        stderr_drain,
    })
}

/// Write the request envelope as one compact JSON line (ADR-0430 §6): the
/// child's whole input before the MCP handshake. The caller owns the
/// handle's lifetime and keeps it open — the child writes its requests on
/// stdout and reads the host's responses from stdin.
pub(crate) fn write_envelope_line(
    stdin: &mut std::process::ChildStdin,
    envelope: &Map<String, Value>,
) -> Result<(), ProtocolError> {
    let line = serde_json::to_string(envelope)
        .map_err(|e| ProtocolError::EnvelopeWriteFailed(e.to_string()))?;
    match write_line(stdin, &line) {
        Ok(()) => Ok(()),
        // The child exited without reading. The pipe's reader is gone, so
        // the write races the exit: a fast child closes stdin before this
        // write is scheduled. The exit classification (or the next stage's
        // missing `initialize`) is the failure, not the closed pipe.
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(ProtocolError::EnvelopeWriteFailed(error.to_string())),
    }
}

/// The child's stderr drain, owned as one value: [`Self::join`] consumes it,
/// so a handle joins exactly once by construction — the mount holds one, and
/// neither the exit path nor the refusal path can join twice. Its owner
/// keeps the slot optional because either path may take it.
pub(crate) struct Drain(JoinHandle<std::io::Result<Vec<u8>>>);

impl Drain {
    /// Wrap a spawned drain thread.
    pub(crate) fn new(handle: JoinHandle<std::io::Result<Vec<u8>>>) -> Self {
        Self(handle)
    }

    /// Join the drain and render what it retained, trimmed: the child's own
    /// words for a failed exit, never a second `Error:` line of its own.
    pub(crate) fn join(self) -> Result<String, ProtocolError> {
        let retained = self
            .0
            .join()
            .map_err(|_| ProtocolError::StderrJoinFailed)?
            .map_err(|_| ProtocolError::StderrJoinFailed)?;
        Ok(String::from_utf8_lossy(&retained).trim().to_string())
    }
}

/// Wait for the child, join its stderr, and classify the exit: `Ok` for a
/// clean zero exit (carrying the trimmed stderr, unused by the caller),
/// the typed [`ProtocolError::ExitFailed`] or
/// [`ProtocolError::KilledBySignal`] otherwise. The one exit classification
/// the mount runs, consulted at the request stream's end inside the
/// session's framed import.
pub(crate) fn wait_and_reap(child: &mut Child, drain: Drain) -> Result<String, ProtocolError> {
    let status = child
        .wait()
        .map_err(|e| ProtocolError::WaitFailed(e.to_string()))?;
    let stderr = drain.join()?;
    if !status.success() {
        return Err(classify_bad_exit(status, stderr));
    }
    Ok(stderr)
}

/// Classify a failed child exit (ADR-0430 §5): a non-zero code carries the
/// child's trimmed stderr; a death by signal is the killed-child failure,
/// the signal named where the platform names one.
pub(crate) fn classify_bad_exit(status: std::process::ExitStatus, stderr: String) -> ProtocolError {
    match status.code() {
        Some(code) => ProtocolError::ExitFailed { code, stderr },
        None => ProtocolError::KilledBySignal {
            signal: killed_by_signal(&status),
            stderr,
        },
    }
}

/// The signal a unix child died to; `None` where the platform does not
/// name one.
#[cfg(unix)]
fn killed_by_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// The signal a child died to; `None` where the platform does not name
/// one.
#[cfg(not(unix))]
fn killed_by_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

// The protocol's behavior tests live in `tests.rs` (the dir-module pattern
// the extension tree's modules use, LABS-421) so the mount's spawn, envelope
// and exit refusals stay scannable in this file, beside their tests, under
// the repo's 1k-line budget.
#[cfg(test)]
mod tests;
