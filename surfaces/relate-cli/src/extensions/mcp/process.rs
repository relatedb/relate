//! The spawned mount (ADR-0442): the process half of the MCP session —
//! the spawned child, its stderr drain, and the end-of-session hook that
//! classifies the child's exit inside the session's framing. [`spawn`]
//! builds the mount over std pipes and hands the run the
//! [`McpProcess`] it drives; everything process-shaped lives here so the
//! parent module stays the session its own doc describes.

use std::path::Path;
use std::process::{Child, ChildStdin};

use serde_json::{Map, Value};

use relate::{Connection, Database};

use super::Session;
use crate::extensions::host::{HostError, RunSummary};
use crate::extensions::manifest::CommandManifest;
use crate::extensions::protocol::{self, Caps};

/// How the session's request loop ended — the one fact the end-of-session
/// hook needs to decide between classifying an exit and killing a refused
/// child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionEnd {
    /// The child closed its request stream; its exit is still to classify.
    Complete,
    /// The loop was refused; the child is to be killed and reaped.
    Refused,
}

/// The end-of-session hook the real mount installs: close the pipes and
/// classify the child's exit, inside the framed import.
pub(super) type EndHook = Box<dyn FnOnce(SessionEnd) -> Result<(), HostError>>;

/// A spawned MCP child with its session and reaping state: the process,
/// the stderr drain, and the session over its pipes. `drive` hands the
/// child and the drain to the session's end hook; until then this struct
/// owns them, and its `Drop` is the safety net for a mount that never
/// drives.
pub(crate) struct McpProcess {
    /// The spawned child with its stderr drain — one field, one
    /// lifecycle fact: set together by [`spawn`], taken together by
    /// [`McpProcess::drive`], killed together by `Drop`, so no half-set
    /// state exists.
    child: Option<(Child, protocol::Drain)>,
    session: Session<ChildStdin>,
}

impl McpProcess {
    /// Drive the session to its end: the manifest's framing around the
    /// whole exchange, the summary returned, and the child's exit
    /// classified *inside* that framing — at the request stream's end for a
    /// clean loop, after the frame for a refused one. A refused session
    /// kills and reaps the child first.
    pub(crate) fn drive(
        mut self,
        store: &Database,
        conn: &mut Connection,
    ) -> Result<Option<RunSummary>, HostError> {
        let (mut child, drain) = self
            .child
            .take()
            .expect("the child is driven once, with its drain");
        self.session.set_end(Box::new(move |end| match end {
            SessionEnd::Complete => {
                // The child closed its request stream; closing its stdin
                // and reaping it classifies the exit (a non-zero exit
                // after a clean stream is a failed run, never a silent
                // success). The drain is joined here, where the exit
                // classification needs the stderr it retained.
                protocol::wait_and_reap(&mut child, drain)?;
                Ok(())
            }
            SessionEnd::Refused => {
                protocol::kill_and_reap(&mut child);
                // The drain detaches, exactly as `Drop` detaches it: a
                // helper the child launched can inherit stderr and hold
                // the pipe open past the kill, so joining here could
                // block the refusal forever. The detached thread ends
                // when the pipe closes, and its retained bytes stay
                // unrendered — the refusal's own text is the pinned
                // contract, not the child's commentary.
                drop(drain);
                Ok(())
            }
        }));
        self.session.drive(store, conn)
    }
}

impl Drop for McpProcess {
    /// The child never outlives its mount: a process dropped without
    /// `drive` (the caller stopped early) kills and reaps it — the mount's
    /// Drop safety net. The stderr drain ends on its own once the child is
    /// dead, so dropping its handle here cannot block (the same reason the
    /// refusal path detaches rather than joins it); its retained bytes
    /// stay unrendered — the caller's own failure text is the contract,
    /// not the child's commentary.
    fn drop(&mut self) {
        if let Some((child, _drain)) = self.child.as_mut() {
            protocol::kill_and_reap(child);
        }
    }
}

/// Spawn the MCP child: the shared pipe setup, the request envelope as one
/// line with stdin KEPT OPEN (the child's responses arrive there), and the
/// session over the pipes. The first request the session reads must be
/// `initialize`.
pub(crate) fn spawn(
    exe: &Path,
    envelope: &Map<String, Value>,
    caps: Caps,
    command: &CommandManifest,
    current_dir: Option<&Path>,
) -> Result<McpProcess, protocol::ProtocolError> {
    let mut piped = protocol::spawn_piped(exe, current_dir)?;
    if let Err(error) = protocol::write_envelope_line(&mut piped.stdin, envelope) {
        protocol::kill_and_reap(&mut piped.child);
        return Err(error);
    }
    Ok(McpProcess {
        child: Some((piped.child, piped.stderr_drain)),
        session: Session::new(piped.stdout, piped.stdin, caps, command),
    })
}
