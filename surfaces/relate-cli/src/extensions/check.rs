//! `relate extension check <dir>`: the six-stage package check
//! (ADR-0429 §6's load refusals, ADR-0430 §5's exit split, ADR-0447's mount
//! handshake) — the diagnostic an author runs before installing.
//!
//! The stages run in the mount's own order and each prints one line:
//!
//! 1. `manifest` — the package directory loads: `extension.json` present
//!    and strictly decoded, the executable inside the package directory,
//!    the mount name not a reserved command word, and the manifest a
//!    grammar the mount accepts (the protocol this host serves, no
//!    duplicate or store-shadowing flag names). Every refusal is the
//!    registry's or the `run` grammar's own text, rendered verbatim.
//! 2. `executable` — the declared file exists, is a regular file, and
//!    carries the exec bit where the platform has one.
//! 3. `spawn` — the process starts over the mount's std pipes and takes
//!    its request envelope (the flag table's defaults; a check passes no
//!    flags).
//! 4. `initialize` — the child's first request is `initialize`, names the
//!    MCP revision this host serves and carries its `clientInfo`; the
//!    check answers it with the mount's own result (the same
//!    [`initialize_facts`](crate::extensions::mcp::initialize_facts) the
//!    mount writes, over an empty unlimited store).
//! 5. `notifications/initialized` — the child confirms the handshake with
//!    MCP's id-less notification, the one message that may precede its
//!    first request.
//! 6. `tools/list` — the check serves the shared tool table (the wire
//!    kit's one table, [`extension_tools_list`]): a `tools/list` request
//!    is answered with it, and a `tools/call` must name one of its tools.
//!
//! A clean run then closes with one advisory status line about the
//! package's recorded GitHub pin (ADR-0449): `approved`, `drifted`, or
//! `not from a GitHub source`, read from the provenance marker and the
//! per-user trust store alone. The line is a report, never a stage: drift
//! does not stop the installed package from mounting (its bytes are the
//! ones that were approved, and the mount never consults the marker), so
//! it does not change the exit code — the install path is where a drifted
//! pin refuses. A marker or trust store that cannot be read is reported as
//! `unknown` with its own refusal text, never as one of the three states.
//!
//! The check opens no store and runs no tool: the handshake it answers
//! publishes an empty unlimited capacity ([`Capacity::new`]`(0, None)`) and
//! the session ends once the tool table is served — no tool executes, so a
//! check is safe against any store, writes nothing and runs offline. That
//! also means the child's own work (the walk an author is still writing)
//! never runs: the check proves the mount contract, not the import.
//!
//! Exit codes follow ADR-0430 §5's split: the package's declaration and
//! contents (stages 1-2) are the usage/manifest class, exit 2; what the
//! package does when run (stages 3-6) is the runtime class, exit 1; a clean
//! check exits 0.
//!
//! Reads are bounded: each expected request waits at most the caller's
//! deadline — the same value the mount's handshake window uses
//! ([`READ_DEADLINE`](crate::extensions::protocol::READ_DEADLINE)) — through
//! the shared deadline reader the mount's session loop also reads with
//! (ADR-0442's deferred read deadline), so a child that writes nothing (or
//! stops mid-session) fails its stage instead of hanging the check. The
//! check bounds every stage because it is a diagnostic; the mount bounds
//! only the handshake, because a working import may be slow between calls.

use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::time::Duration;

use relate_formatter::sanitize_error_text;
use relate_mcp::tools as tool_layer;
use relate_wire::mcp::{
    decode_request, extension_tools_list, Capacity, Response, MCP_PROTOCOL_VERSION,
    METHOD_INITIALIZE, METHOD_INITIALIZED,
};
use relate_wire::read_cap::{write_line, DeadlineReader, LineArrival};

use crate::extensions::host::{check_manifest, default_envelope};
use crate::extensions::mcp::{
    initialize_facts, require_protocol_version, server_info, McpError, McpRefusal, Route,
};
use crate::extensions::protocol::{self, Caps};
use crate::extensions::provenance::{Pin, Provenance, Tier};
use crate::extensions::registry::{self, ExtensionEntry};
use crate::extensions::trust::TrustStore;

/// The check's stages, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// `extension.json` present, decoded, and a grammar the mount accepts.
    Manifest,
    /// The declared executable exists and is executable.
    Executable,
    /// The child spawned and took its envelope.
    Spawn,
    /// `initialize` arrived and was answered.
    Initialize,
    /// `notifications/initialized` arrived.
    Initialized,
    /// The shared tool table was served.
    ToolTable,
}

impl Stage {
    /// The stage's line label — the word the report prints, so a failure
    /// always names the stage it happened at.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Executable => "executable",
            Self::Spawn => "spawn",
            Self::Initialize => "initialize",
            Self::Initialized => "notifications/initialized",
            Self::ToolTable => "tools/list",
        }
    }
}

/// The check's whole result: one report line per stage that ran, in order,
/// and the stage that failed (if any).
pub(crate) struct Report {
    /// The printed report: `"<stage>: ok …"` per completed stage, and the
    /// failing stage's `"<stage>: failed: <refusal>"` last.
    pub(crate) lines: Vec<String>,
    /// The stage that refused the package; `None` for a clean check.
    pub(crate) failure: Option<Stage>,
}

impl Report {
    /// The process exit code (ADR-0430 §5's split): 0 clean, 2 for the
    /// package's declaration and contents (stages 1-2), 1 for what it does
    /// when run.
    pub(crate) fn exit_code(&self) -> i32 {
        match self.failure {
            None => 0,
            Some(Stage::Manifest | Stage::Executable) => 2,
            Some(_) => 1,
        }
    }
}

/// Run the check over the package directory `dir`, waiting at most
/// `deadline` for each expected request. Never opens a store, never runs a
/// tool, and always returns: every read is bounded and every child is
/// killed before the report is returned. The trust store the closing pin
/// line consults is the real environment's
/// ([`crate::extensions::trust::trust_path`] under the config dir).
pub(crate) fn check(dir: &Path, deadline: Duration) -> Report {
    let trust_file =
        crate::config_dir::config_dir().map(|config| crate::extensions::trust::trust_path(&config));
    check_with(dir, deadline, trust_file.as_deref())
}

/// [`check`] with the trust store's path supplied — the injectable twin
/// (`ActionRegistry::load`/`load_with`'s pattern), so the pin line's three
/// states are testable without a real config dir. `None` means no config
/// dir resolved: the pin line reports that it cannot read a store.
pub(crate) fn check_with(dir: &Path, deadline: Duration, trust_file: Option<&Path>) -> Report {
    let mut lines = Vec::new();
    match run(dir, deadline, &mut lines) {
        Ok(entry) => {
            // The advisory pin line closes a passing run; an author reading
            // a failure is told what broke the mount, not what a pin says.
            lines.push(pin_line(entry.mount_name(), dir, trust_file));
            Report {
                lines,
                failure: None,
            }
        }
        Err(Failure { stage, detail }) => {
            lines.push(format!(
                "{}: failed: {}",
                stage.as_str(),
                sanitize_error_text(&detail)
            ));
            Report {
                lines,
                failure: Some(stage),
            }
        }
    }
}

/// One stage's refusal: which stage, and the refusal text it renders.
struct Failure {
    stage: Stage,
    detail: String,
}

impl Failure {
    fn at(stage: Stage, detail: impl Into<String>) -> Self {
        Self {
            stage,
            detail: detail.into(),
        }
    }
}

/// The six stages in order, each appending its report line; the loaded
/// package entry comes back for the closing pin line, which is keyed by
/// the manifest's mount name.
fn run(dir: &Path, deadline: Duration, lines: &mut Vec<String>) -> Result<ExtensionEntry, Failure> {
    let entry = stage_manifest(dir, lines)?;
    let executable = stage_executable(&entry, lines)?;
    let mut child = stage_spawn(&entry, &executable, deadline, lines)?;
    stage_initialize(&mut child, &entry, lines)?;
    stage_initialized(&mut child, lines)?;
    stage_tool_table(&mut child, lines)?;
    Ok(entry)
}

// ---------------------------------------------------------------------
// 1-2: the package itself (the usage/manifest class)
// ---------------------------------------------------------------------

/// Stage 1: load the package directory with the registry's own loader and
/// the `run` grammar's own manifest checks — the check refuses exactly what
/// the machine would refuse at load, in the same words.
fn stage_manifest(dir: &Path, lines: &mut Vec<String>) -> Result<ExtensionEntry, Failure> {
    let entry = registry::load_package(dir)
        .map_err(|error| Failure::at(Stage::Manifest, error.to_string()))?;
    check_manifest(entry.mount_name(), entry.command())
        .map_err(|error| Failure::at(Stage::Manifest, error.to_string()))?;
    let command = entry.command();
    lines.push(format!(
        "manifest: ok ({} {}, protocol {})",
        sanitize_error_text(&command.name),
        sanitize_error_text(&command.version),
        sanitize_error_text(&command.protocol)
    ));
    Ok(entry)
}

/// Stage 2: the declared executable resolves inside the package directory
/// (stage 1 guaranteed it cannot escape) to a regular file the platform
/// can execute.
fn stage_executable(entry: &ExtensionEntry, lines: &mut Vec<String>) -> Result<PathBuf, Failure> {
    let declared = &entry.command().executable;
    let exe = entry.package_dir.join(declared);
    let metadata = std::fs::metadata(&exe).map_err(|error| {
        let detail = match error.kind() {
            std::io::ErrorKind::NotFound => {
                format!("the extension executable {} does not exist", exe.display())
            }
            _ => format!(
                "cannot read the extension executable {}: {error}",
                exe.display()
            ),
        };
        Failure::at(Stage::Executable, detail)
    })?;
    if !metadata.is_file() {
        return Err(Failure::at(
            Stage::Executable,
            format!(
                "the extension executable {} is not a regular file",
                exe.display()
            ),
        ));
    }
    if !executable_bit(&metadata) {
        return Err(Failure::at(
            Stage::Executable,
            format!(
                "the extension executable {} is not executable",
                exe.display()
            ),
        ));
    }
    lines.push(format!(
        "executable: ok ({})",
        sanitize_error_text(&declared.display().to_string())
    ));
    Ok(exe)
}

/// Whether the platform will run this file: the exec bits on unix, the
/// extension on platforms without one (where the spawn classifies it).
#[cfg(unix)]
fn executable_bit(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_bit(_metadata: &std::fs::Metadata) -> bool {
    true
}

// ---------------------------------------------------------------------
// 3-6: the running package (the runtime class)
// ---------------------------------------------------------------------

/// Stage 3: spawn the child over the mount's std pipes and write its
/// request envelope — the flag table's defaults, since a check passes no
/// flags. The envelope write belongs here: a child that died before
/// reading it never started.
fn stage_spawn(
    entry: &ExtensionEntry,
    executable: &Path,
    deadline: Duration,
    lines: &mut Vec<String>,
) -> Result<Child, Failure> {
    let command = entry.command();
    let mut piped = protocol::spawn_piped(executable, None)
        .map_err(|error| Failure::at(Stage::Spawn, error.to_string()))?;
    let envelope = default_envelope(command);
    if let Err(error) = protocol::write_envelope_line(&mut piped.stdin, &envelope) {
        protocol::kill_and_reap(&mut piped.child);
        return Err(Failure::at(Stage::Spawn, error.to_string()));
    }
    lines.push("spawn: ok".to_string());
    Ok(Child::new(piped, deadline))
}

/// Stage 4: the child's first request is `initialize`, it names the
/// revision this host serves, and it is answered with the mount's own
/// `initialize` result.
fn stage_initialize(
    child: &mut Child,
    entry: &ExtensionEntry,
    lines: &mut Vec<String>,
) -> Result<(), Failure> {
    let request = child.expect_request(
        Stage::Initialize,
        "the initialize request",
        McpError::InitializeMissing.to_string(),
    )?;
    if request.method != METHOD_INITIALIZE {
        return Err(Failure::at(
            Stage::Initialize,
            McpError::InitializeNotFirst {
                method: request.method,
            }
            .to_string(),
        ));
    }
    let Some(id) = request.id else {
        return Err(Failure::at(
            Stage::Initialize,
            McpError::RequestWithoutId {
                method: METHOD_INITIALIZE.to_string(),
            }
            .to_string(),
        ));
    };
    let named = require_protocol_version(request.params.as_ref())
        .map_err(|refusal| Failure::at(Stage::Initialize, refusal.to_string()))?;
    // What the member *says* is the check's own contract — a child of
    // another revision cannot mount, so the check names both instead of
    // answering and letting the child fail on its decode.
    if named != MCP_PROTOCOL_VERSION {
        return Err(Failure::at(
            Stage::Initialize,
            format!(
                "the extension names MCP revision '{named}'; this host serves \
                 '{MCP_PROTOCOL_VERSION}'"
            ),
        ));
    }
    let command = entry.command();
    let facts = initialize_facts(
        Caps::MOUNT,
        command.requires_fresh_store,
        command.commit,
        // No store: the check publishes an empty, unlimited capacity rather
        // than an invented one, and runs no tool that could read it.
        Capacity::new(0, None),
    );
    child
        .answer(&Response::success(id, facts).to_line())
        .map_err(|detail| Failure::at(Stage::Initialize, detail))?;
    let identity = server_info();
    lines.push(format!(
        "initialize: ok (MCP {MCP_PROTOCOL_VERSION}, server {} {})",
        sanitize_error_text(&identity.name),
        sanitize_error_text(&identity.version)
    ));
    Ok(())
}

/// Stage 5: MCP's handshake confirmation — the id-less
/// `notifications/initialized`, the one message a child may send before its
/// first request.
fn stage_initialized(child: &mut Child, lines: &mut Vec<String>) -> Result<(), Failure> {
    let request = child.expect_request(
        Stage::Initialized,
        "notifications/initialized",
        "the extension ended its MCP session before notifications/initialized".to_string(),
    )?;
    if request.method != METHOD_INITIALIZED {
        return Err(Failure::at(
            Stage::Initialized,
            format!(
                "the extension sent '{}' before notifications/initialized",
                request.method
            ),
        ));
    }
    lines.push("notifications/initialized: ok".to_string());
    Ok(())
}

/// Stage 6: the tool table. The check serves the wire kit's one table — a
/// `tools/list` request is answered with it — and refuses a `tools/call`
/// naming a tool the mount does not serve. A child that asks for nothing
/// (it is off doing its import) or that ends after the handshake passes:
/// nothing was asked of the host that the mount could not answer.
fn stage_tool_table(child: &mut Child, lines: &mut Vec<String>) -> Result<(), Failure> {
    let table = extension_tools_list();
    let served = tool_names(&table);
    let called = serve_tool_requests(child, &table, &served)?;
    lines.push(match called {
        Some(tool) => format!(
            "tools/list: ok (the shared tool table: {} tools; the extension calls '{}')",
            served.len(),
            sanitize_error_text(&tool)
        ),
        None => format!(
            "tools/list: ok (the shared tool table: {} tools)",
            served.len()
        ),
    });
    Ok(())
}

/// The most post-handshake requests the tool-table stage serves.
const TOOL_REQUEST_BUDGET: usize = 2;

/// Serve the child's post-handshake requests the way the mount would, at
/// most [`TOOL_REQUEST_BUDGET`] (two): a child that asks for the table and then names its first
/// tool, or one that goes straight to the call. A child that asks for
/// anything else, ends, or stalls has asked nothing more of the host — the
/// mount would keep serving it, but the check's contract is the table, not
/// the import. Returns the tool the child called, when it called one; every
/// request is classified by the mount's own [`Route`].
fn serve_tool_requests(
    child: &mut Child,
    table: &serde_json::Value,
    served: &[String],
) -> Result<Option<String>, Failure> {
    for _ in 0..TOOL_REQUEST_BUDGET {
        let request = match child.next() {
            None | Some(LineArrival::Eof) => return Ok(None),
            Some(LineArrival::Exceeded) => return Err(child.record_too_large(Stage::ToolTable)),
            Some(LineArrival::Failed(detail)) => {
                return Err(Failure::at(
                    Stage::ToolTable,
                    McpError::ReadFailed(detail).to_string(),
                ))
            }
            Some(LineArrival::Line(line)) => decode_request(&line).map_err(|error| {
                Failure::at(Stage::ToolTable, McpRefusal::from(error).to_string())
            })?,
        };
        let route =
            Route::of(request).map_err(|error| Failure::at(Stage::ToolTable, error.to_string()))?;
        match route {
            // A notification asks nothing of the host.
            Route::Notification => {}
            Route::ToolsList { id } => child.answer_at(
                Stage::ToolTable,
                &Response::success(id, table.clone()).to_line(),
            )?,
            Route::ToolsCall { params, .. } => {
                let (name, _arguments) = tool_layer::call_parts(params.as_ref())
                    .map_err(|error| Failure::at(Stage::ToolTable, error.to_string()))?;
                if !served.iter().any(|tool| tool == name) {
                    return Err(Failure::at(
                        Stage::ToolTable,
                        format!(
                            "the extension calls '{name}'; this mount serves: {}",
                            served.join(", ")
                        ),
                    ));
                }
                return Ok(Some(name.to_string()));
            }
            // A second `initialize` or any other method is the mount's own
            // refusal, answered and not a check failure: the mount would
            // answer it too and keep the session.
            Route::Initialize { id, .. } => {
                return child
                    .answer_at(
                        Stage::ToolTable,
                        &McpRefusal::AlreadyInitialized.into_line(id),
                    )
                    .map(|()| None)
            }
            Route::Unknown { id, method } => {
                return child
                    .answer_at(
                        Stage::ToolTable,
                        &McpRefusal::UnknownMethod { method }.into_line(id),
                    )
                    .map(|()| None)
            }
        }
    }
    Ok(None)
}

/// The tool names one `tools/list` result carries, in table order — read
/// off the table itself, so the check's refusal can never name a tool the
/// mount does not serve.
fn tool_names(table: &serde_json::Value) -> Vec<String> {
    table
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .expect("the wire kit's tools/list carries a tools array")
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(serde_json::Value::as_str)
                .expect("the wire kit's every tool carries a name")
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------
// The closing pin line (ADR-0449)
// ---------------------------------------------------------------------

/// What the checked package's recorded GitHub pin is, against the per-user
/// trust store: the three states the line reports, and `unknown` for a
/// marker or store that refused to be read.
///
/// A status report, not a stage: a drifted pin does not stop the installed
/// package from mounting — the bytes on disk are the ones that were
/// approved, and the mount never consults the marker — so it never changes
/// the check's exit code. Drift is refused where it matters, before a
/// fetch, by the install path (`install --github` compares the resolved pin
/// against the store); the check's job is to say that it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PinStatus {
    /// The marker's pin is exactly the trust store's entry for this name.
    Approved(Pin),
    /// The marker's pin is not the store's entry for the name.
    Drifted {
        /// The pin the package's marker records.
        installed: Pin,
        /// The pin the trust store records for the name.
        approved: Pin,
    },
    /// The trust store holds no pin for the name at all; the payload is
    /// the pin the package's marker records.
    Unapproved(Pin),
    /// The package did not arrive by the GitHub tier: its tier, or `None`
    /// when the package carries no marker.
    NotGithub(Option<Tier>),
    /// The marker or the trust store could not be read, or a github-pin
    /// marker carries no pin; the refusal's text rides along.
    Unknown(String),
}

impl PinStatus {
    /// The state word the line leads with — the three the verb's contract
    /// names, plus `unknown` for a read that refused.
    fn as_str(&self) -> &'static str {
        match self {
            Self::Approved(_) => "approved",
            Self::Drifted { .. } | Self::Unapproved(_) => "drifted",
            Self::NotGithub(_) => "not from a GitHub source",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// The package's pin state: the provenance marker's recorded pin against
/// the trust store's entry for the package's mount name. Two local JSON
/// reads — no store opened, no fetch (ADR-0449's trust data is local).
fn pin_status(package_dir: &Path, name: &str, trust_file: Option<&Path>) -> PinStatus {
    let provenance = match Provenance::read(package_dir) {
        Ok(Some(provenance)) => provenance,
        Ok(None) => return PinStatus::NotGithub(None),
        Err(error) => return PinStatus::Unknown(error.to_string()),
    };
    if provenance.tier != Tier::GithubPin {
        return PinStatus::NotGithub(Some(provenance.tier));
    }
    let Some(installed) = provenance.pin else {
        return PinStatus::Unknown(
            "the provenance marker records the github-pin tier without a pin".to_string(),
        );
    };
    let Some(trust_file) = trust_file else {
        return PinStatus::Unknown(crate::config_dir::no_config_dir_text());
    };
    let store = match TrustStore::load(trust_file) {
        Ok(store) => store,
        Err(error) => return PinStatus::Unknown(error.to_string()),
    };
    match store.get(name) {
        Some(approved) if *approved == installed => PinStatus::Approved(installed),
        Some(approved) => PinStatus::Drifted {
            installed,
            approved: approved.clone(),
        },
        None => PinStatus::Unapproved(installed),
    }
}

/// One pin's facts as the line renders them: `repo@tag commit <sha>`.
/// Every fragment is package data, so it passes the shared sanitizer.
fn pin_facts(pin: &Pin) -> String {
    format!(
        "{}@{} commit {}",
        sanitize_error_text(&pin.repo),
        sanitize_error_text(&pin.tag),
        sanitize_error_text(&pin.commit)
    )
}

/// The closing status line: `pin: <state> (<facts>)`. Advisory by design —
/// see [`PinStatus`] for why it never touches the exit code.
fn pin_line(name: &str, package_dir: &Path, trust_file: Option<&Path>) -> String {
    let status = pin_status(package_dir, name, trust_file);
    let detail = match &status {
        PinStatus::Approved(pin) => pin_facts(pin),
        PinStatus::Drifted {
            installed,
            approved,
        } => format!(
            "installed {}; the trust store holds {} — re-approve with \
             'relate extension install {} --github {}@{} --re-approve'",
            pin_facts(installed),
            pin_facts(approved),
            sanitize_error_text(name),
            sanitize_error_text(&installed.repo),
            sanitize_error_text(&installed.tag)
        ),
        PinStatus::Unapproved(installed) => format!(
            "the trust store holds no pin for '{}'; the package carries {}",
            sanitize_error_text(name),
            pin_facts(installed)
        ),
        PinStatus::NotGithub(tier) => tier_label(*tier).to_string(),
        PinStatus::Unknown(detail) => sanitize_error_text(detail),
    };
    format!("pin: {} ({detail})", status.as_str())
}

/// The tier label a package's provenance shows, `unmarked` when it carries
/// no marker — the one spelling `extension check` and `extension list` share.
pub(crate) fn tier_label(tier: Option<Tier>) -> &'static str {
    tier.map_or("unmarked", Tier::as_str)
}

// ---------------------------------------------------------------------
// The hostless session
// ---------------------------------------------------------------------

/// A spawned child the check drives: its stdin (the response channel), the
/// mount's shared deadline-bounded request stream, and the read deadline.
struct Child {
    child: std::process::Child,
    stdin: ChildStdin,
    requests: DeadlineReader,
    /// The stderr drain's handle, detached when the child is dropped: the
    /// check never waits on a child's stderr (a stray grandchild holding it
    /// open would hang the diagnostic), and the mount's rule — a child must
    /// never stall on a full stderr pipe — still holds while the drain runs.
    _stderr_drain: protocol::Drain,
    request_cap: u64,
    deadline: Duration,
}

impl Child {
    /// Take the spawned process apart: the request stream is the mount's
    /// shared deadline reader, so every [`Self::next`] can time out.
    fn new(piped: protocol::PipedChild, deadline: Duration) -> Self {
        let protocol::PipedChild {
            child,
            stdin,
            stdout,
            stderr_drain,
        } = piped;
        let request_cap = Caps::MOUNT.mcp_request_bytes();
        Self {
            child,
            stdin,
            requests: DeadlineReader::spawn(stdout, request_cap),
            _stderr_drain: stderr_drain,
            request_cap,
            deadline,
        }
    }

    /// The child's next request, waiting at most the deadline: `None` on
    /// the deadline, and [`LineArrival::Eof`] when the stream ends (the
    /// reader thread exits without sending).
    fn next(&mut self) -> Option<LineArrival> {
        self.requests.next(self.deadline)
    }

    /// The request a stage expects, decoded by the mount's own strict
    /// decode: the deadline, the stream's end and a failed read carry the
    /// stage's own texts, so every failure names what was expected.
    fn expect_request(
        &mut self,
        stage: Stage,
        waiting: &str,
        ended: String,
    ) -> Result<relate_wire::mcp::Request, Failure> {
        match self.next() {
            Some(LineArrival::Line(line)) => decode_request(&line)
                .map_err(|error| Failure::at(stage, McpRefusal::from(error).to_string())),
            Some(LineArrival::Exceeded) => Err(self.record_too_large(stage)),
            Some(LineArrival::Failed(detail)) => {
                Err(Failure::at(stage, McpError::ReadFailed(detail).to_string()))
            }
            Some(LineArrival::Eof) => Err(Failure::at(stage, ended)),
            None => Err(Failure::at(
                stage,
                format!(
                    "the extension wrote no request within {:?} ({waiting} was expected)",
                    self.deadline
                ),
            )),
        }
    }

    /// The mount's record-cap refusal for a line past the framing bound.
    fn record_too_large(&self, stage: Stage) -> Failure {
        Failure::at(
            stage,
            McpError::RecordTooLarge {
                limit: self.request_cap,
            }
            .to_string(),
        )
    }

    /// [`Child::answer`] with its failure recorded at `stage`.
    fn answer_at(&mut self, stage: Stage, line: &str) -> Result<(), Failure> {
        self.answer(line)
            .map_err(|detail| Failure::at(stage, detail))
    }

    /// Write one response line to the child — the mount's own write, so the
    /// failure text is the mount's.
    fn answer(&mut self, line: &str) -> Result<(), String> {
        write_line(&mut self.stdin, line)
            .map_err(|error| McpError::ResponseWriteFailed(error.to_string()).to_string())
    }
}

impl Drop for Child {
    /// The child never outlives the check: a stage that failed leaves it
    /// waiting on a response that will never come, so the drop kills and
    /// reaps it — the mount's reaping policy (ADR-0377), applied to the
    /// diagnostic.
    fn drop(&mut self) {
        protocol::kill_and_reap(&mut self.child);
    }
}

// The check's stages and session are pinned in `check/tests.rs` (the
// extension tree's dir-module pattern) so this file stays scannable beside
// the stages it documents, under the repo's 1k-line budget.
#[cfg(test)]
mod tests;
