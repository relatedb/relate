//! The extension mount's shared kit (ADR-0447): the one crate both sides of
//! the MCP stdio wire link.
//!
//! Three parts, one home each:
//!
//! - [`read_cap`] — the bounded-read mechanism ([`read_cap::read_capped`],
//!   [`read_cap::read_line_capped`], [`read_cap::DeadlineReader`],
//!   [`read_cap::drain_stderr`]), moved here from the CLI so the mount's
//!   read side and the importer children's capped source reads run through
//!   one mechanism (ADR-0377, ADR-0430 §4). The CLI links this crate for
//!   its own capped reads (`--fetch-params`, the mount's request reader)
//!   and for the deadline-bounded record reader both the `run` session loop
//!   and the `check` verb's staged session wait on.
//! - the caps, the commit-intent spellings and the stored-path renderings
//!   at this crate's root: the numbers the host admits ([`MAX_STATEMENT_BYTES`],
//!   [`MAX_ROWS_PER_STATEMENT`], [`MAX_STATEMENTS`], [`MAX_STREAM_BYTES`]),
//!   the vocabulary the `initialize` result and the write layer share
//!   ([`WHOLE_IMPORT`], [`PER_BATCH`]), and the one path rendering both
//!   importer children store through ([`display`], [`file_name`]). The
//!   host's `extensions::protocol` builds its `Caps` from the numbers; the
//!   child kit chunks and refuses against them, and both importer children
//!   render their stored paths through the one rendering.
//! - [`envelope`] — the request envelope's shared child facts: the `DIR`
//!   positional's resolution and the envelope-keys pin the children's tests
//!   call.
//! - [`mcp`] — the mount's MCP stdio wire (ADR-0442): the tool vocabulary —
//!   one table of names both the extension mount and the product MCP
//!   server serve — the `tools/list` result, the JSON-RPC 2.0 record types
//!   with their strict decode, and the child-side [`mcp::McpChild`] client
//!   kit ([`mcp::serve`] owns the whole bootstrap). The host side of that
//!   wire stays in the CLI.
//!
//! The host side of the wire — spawning, the request envelope, the session
//! loop, the commit framing and the exit classification — stays in the CLI
//! (`surfaces/relate-cli/src/extensions/protocol.rs` and
//! `…/extensions/mcp.rs`): it is host execution, not a shared mechanism. The
//! tool *bodies* are not here: they live once in `relate-mcp`, which
//! both the extension host and the product MCP server call (ADR-0447 §3).
//! Third-party children need none of this crate (ADR-0447's
//! language-agnosticism: the wire is printable JSON-RPC, and the CLI e2e's
//! `sh` fixture child links nothing); the kit exists so the first-party
//! importers never fork the emission logic.

pub mod envelope;
pub mod mcp;
pub mod read_cap;

/// The per-statement byte cap (ADR-0430 §4, re-anchored per call by the MCP
/// mount): 8 MiB, sized for a `write_cypher` call whose `rows` carry
/// content-bearing rows — the fs importer's default file row carries 64 KiB
/// of content (ADR-0157), so a call admits many such rows without inviting
/// a hostile line. It bounds both directions of the pipe: the host refuses
/// a request line past it, and the child's capped response read refuses a
/// reply past it. The child kit chunks its rows under it and refuses a
/// single row that cannot fit one call.
pub const MAX_STATEMENT_BYTES: u64 = 8 * 1024 * 1024;

/// The rows-per-statement cap (ADR-0430 §4): 1000 rows, the same batch the
/// shared write path chunks parameterized writes by (ADR-0392). One home
/// for both sides: the host refuses a `write_cypher` call past it, the
/// child kit flushes a call at it.
pub const MAX_ROWS_PER_STATEMENT: usize = 1000;

/// The statements-per-call cap (ADR-0430 §4): 4096 statements in one
/// `batch_cypher` call — many phases for any legitimate import, few enough
/// that a looping child is refused, not humored. Host-enforced (the child's
/// statement count is the host's admission decision); stated here so the
/// cap set has one home.
pub const MAX_STATEMENTS: usize = 4096;

/// The whole-run stream cap (ADR-0430 §4, scoped by ADR-0598): 128 MiB of
/// JSON-RPC across a `whole-import` session — the framing that retains
/// every call in one transaction until its single commit, the one thing
/// the per-call caps cannot bound. A `per-batch` session has no whole-run
/// cap: its bounds are the per-call caps and the host store's live-graph
/// limit, so a long per-batch import streams every chunk they admit. The
/// host refuses the stream past it; the child kit counts what it writes
/// against the published cap and refuses first with an importer-flavored
/// message, so a legitimate-but-oversized whole-import fails before the
/// host kills the child mid-call.
pub const MAX_STREAM_BYTES: u64 = 128 * 1024 * 1024;

/// The `whole-import` commit intent's wire spelling (ADR-0430 §1: one
/// explicit transaction around the whole import, ADR-0157) — the value the
/// manifest's `commit` field decodes from and the word the `initialize`
/// result publishes as the session's framing. One home for the vocabulary:
/// the host's `CommitPolicy::spelling` renders these constants, so the
/// decode, the publish and the write layer cannot drift apart.
pub const WHOLE_IMPORT: &str = "whole-import";

/// The `per-batch` commit intent's wire spelling (ADR-0430 §1: each call
/// commits as it lands, ADR-0392) — [`WHOLE_IMPORT`]'s sibling, the same
/// one-home rule.
pub const PER_BATCH: &str = "per-batch";

// ---------------------------------------------------------------------
// The stored-path rendering both importer children share (ADR-0376 §5:
// `File.path` renders with `to_string_lossy`, and a `:File` row's name is
// derived from the stored path once). One rendering, one home, so the
// fs and git children store the same path bytes for the same file.
// ---------------------------------------------------------------------

/// Render a path the way both importers store it.
pub fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Render a path's final component, falling back to the whole path for a
/// path without one (the walked root `/`).
pub fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| display(path))
}
