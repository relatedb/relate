//! The run lifecycle (ADR-0430 §7): the host-owned store, the MCP session,
//! the summary rendering, and the interactive handoff/checkpoint.

use relate::{Database, Namespace};
use relate_formatter::sanitize_error_text;

use crate::extensions::protocol::{Caps, ProtocolError};
use crate::extensions::registry::ExtensionEntry;
use crate::extensions::write;
use crate::telemetry::Telemetry;

use super::ExtensionRun;

// ---------------------------------------------------------------------
// The run lifecycle (ADR-0430 §7): host-owned store, wire batches
// ---------------------------------------------------------------------

/// A refused extension run (ADR-0430 §5's runtime class): typed variants
/// with pinned texts, the `ImportError` pattern — the CLI renders each as
/// one sanitized `Error:` line and exits 1, and tests assert the wording
/// here in one place. The child's own words (its `error` record) ride the
/// variant that collected them, rendered through `print_error`'s
/// sanitization, never raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostError {
    /// The wire refused: the protocol's typed refusal, its own pinned
    /// text (a decode violation, a cap, a killed child, a failed exit).
    Wire(ProtocolError),
    /// The engine failed at the write boundary — carried across intact
    /// (the `ImportError::Engine` rule, ADR-0392), so the `Error:` line
    /// renders the engine's own text, byte for byte.
    Engine {
        /// The engine's typed error, exactly as the public API returned it.
        error: relate::Error,
    },
    /// A `requires-fresh-store` action refused a target that holds nodes
    /// (ADR-0394's freshness read, as the extension's authored refusal):
    /// checked before the child spawns, so the refusal costs no child.
    FreshStoreRequired {
        /// The extension's mount name.
        name: String,
        /// The node count the target already holds.
        nodes: i64,
    },
    /// The MCP mount's fatal refusal (ADR-0442): a broken request stream
    /// or a session that never initialized. Per-call refusals are JSON-RPC
    /// error responses the child reads, never host errors.
    Mcp(crate::extensions::mcp::McpError),
}

impl From<ProtocolError> for HostError {
    fn from(error: ProtocolError) -> Self {
        Self::Wire(error)
    }
}

impl From<relate::Error> for HostError {
    fn from(error: relate::Error) -> Self {
        Self::Engine { error }
    }
}

impl From<crate::extensions::mcp::McpError> for HostError {
    fn from(error: crate::extensions::mcp::McpError) -> Self {
        Self::Mcp(error)
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(error) => write!(f, "{error}"),
            Self::Engine { error } => write!(f, "{error}"),
            Self::FreshStoreRequired { name, nodes } => write!(
                f,
                "the extension '{name}' requires a fresh store, but the target holds {nodes} nodes"
            ),
            Self::Mcp(error) => write!(f, "{error}"),
        }
    }
}

/// What a successful extension run reported (ADR-0442 §2): the child's
/// one-line result and its starter queries for the REPL handoff —
/// absent when the child ended its session without calling
/// `report_output`, which the session admits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunSummary {
    /// The child's one-line result.
    pub(crate) text: String,
    /// Starter queries offered to the interactive handoff (ADR-0286).
    pub(crate) starters: Vec<String>,
}

/// Run the resolved action (ADR-0430 §7) — the `run_import` lifecycle,
/// with the wire as the importer: open the host-owned store (the
/// `:memory:` default; an explicit `-d`/`--database` names the target),
/// refuse a non-fresh target for a `requires-fresh-store` action before
/// anything runs, spawn the child with its envelope, drive the admitted
/// batches under the manifest's commit policy, print the summary, then
/// classify the storage, decide the interactive handoff and checkpoint.
/// Returns the process exit code: 0 success, 1 every runtime failure.
pub(crate) fn run_action(
    entry: &ExtensionEntry,
    run: ExtensionRun,
    telemetry: &mut Telemetry,
) -> i32 {
    // One line per runtime failure: the message through the shared
    // sanitizer, the exit code the contract pins (ADR-0430 §5).
    fn fail(error: impl std::fmt::Display) -> i32 {
        crate::print_error(format!("Error: {error}"));
        1
    }
    let command = entry.command();
    // An omitted `--database` is the default in-memory target (ADR-0286),
    // resolved once here the way `run_import` resolves it.
    let db_path = run.db.as_deref().unwrap_or(":memory:");
    let db = match Database::open(db_path) {
        Ok(db) => db,
        Err(e) => {
            crate::print_error(format!("Failed to open Relate: {e}"));
            return 1;
        }
    };
    let mut conn = match db.connect_typed(Namespace::Neo4j) {
        Ok(conn) => conn,
        Err(e) => return fail(HostError::from(e)),
    };
    // The ADR-0394 freshness read, as the extension's refusal: before
    // the child spawns, so a refusal costs no child to kill.
    if command.requires_fresh_store {
        let nodes = match write::node_count(&mut conn) {
            Ok(nodes) => nodes,
            Err(e) => return fail(HostError::from(e)),
        };
        if nodes > 0 {
            return fail(HostError::FreshStoreRequired {
                name: entry.mount_name().to_string(),
                nodes,
            });
        }
    }
    // The executable resolves inside the package directory (the load
    // refused an escaping path, ADR-0429 §2); existence is spawn-time —
    // `spawn_piped`'s typed refusal (ADR-0430 §5). The child inherits
    // the user's cwd: the host passes no directory of its own.
    let exe = entry.package_dir.join(&command.executable);
    // The mount's one wire (ADR-0447): the MCP stdio server drives the
    // child as its client, under the host-owned store, the fresh-store
    // check, the manifest's commit policy, the summary rendering and the
    // checkpoint.
    let process = match crate::extensions::mcp::process::spawn(
        &exe,
        &run.envelope,
        Caps::MOUNT,
        command,
        None,
    ) {
        Ok(process) => process,
        Err(e) => return fail(HostError::from(e)),
    };
    let summary = match process.drive(&db, &mut conn) {
        Ok(summary) => summary,
        Err(e) => return fail(e),
    };
    print_summary(summary.as_ref());

    // The canonical memory classification (ADR-0163 §5), decided once —
    // then the ADR-0286 handoff: the default in-memory run opens the
    // shell on a TTY with `repl` compiled in; an explicit target exits
    // after the summary.
    let storage = crate::telemetry::StorageKind::for_uri(db_path);
    let interactive =
        crate::session::opens_interactive_shell(run.db.as_deref(), crate::is_terminal());
    let code = if interactive {
        crate::session::run_import_interactive(&db, db_path, storage, telemetry)
    } else {
        0
    };
    crate::session::final_checkpoint(&db, storage, code)
}

/// Print the run's summary (ADR-0430 §7): the child's one-line result,
/// then each starter query indented two spaces — the append layout the
/// CLI's compiled-in importers used (LABS-421's one append policy),
/// applied here to child-authored text: every fragment through the
/// `print_error`-grade sanitization, so a child can never forge a second
/// line, ANSI, or control characters into the host's output (ADR-0430
/// §5). Nothing at all when the child ended without a summary.
fn print_summary(summary: Option<&RunSummary>) {
    let Some(summary) = summary else { return };
    let mut text = sanitize_error_text(&summary.text);
    for starter in &summary.starters {
        text.push_str("\n  ");
        text.push_str(&sanitize_error_text(starter));
    }
    println!("{text}");
}

// The host's behavior tests live in `tests.rs` (the dir-module pattern
// the extension tree's protocol module uses, LABS-421) so
// the parse, the help rendering and the driver stay scannable in this
// file, beside their tests, under the repo's 1k-line budget.
