//! The `git-to-graph` extension child (ADR-0431 §11, ADR-0447): the
//! importer's MCP half. Its whole input is one request envelope on stdin —
//! the resolved flag values the host parsed from the committed manifest's
//! flag table — and its whole output is the MCP session it drives as the
//! client: the `initialize` handshake, the three range indexes as
//! `update_schema` calls before the first write, the six write phases as
//! `write_cypher` calls with their rows, the post-commit `commit_subject`
//! full-text index as a closing `update_schema`, and the run's summary
//! through `report_output`.
//!
//! It carries no `Database` and never touches a store (ADR-0447 §4): the
//! host owns the store, the commit policy, the caps and the exit codes; this
//! child's only power is the calls it makes. The kit's
//! [`serve`](relate_wire::mcp::serve) owns the bootstrap — the envelope
//! read, the handshake, the failure report, the exit — and every failure (a
//! value the host's kind-checked parse could not refuse, an oversized
//! `git log`, an unsafe path, the pair cap, a cap) prints the importer's own
//! wording on stderr and exits 1, so the host renders it under its `Error:`
//! line.

use relate_git_to_graph::plan;
use relate_git_to_graph::{git_log, starters, Caps, Envelope};
use relate_wire::envelope::resolve_dir;
use relate_wire::mcp::{serve, StdioChild};

/// The mount name the handshake reports — the committed manifest declares
/// the same package (ADR-0430 §3: the manifest declares, the host enforces,
/// the child names itself).
const NAME: &str = "git-to-graph";

fn main() {
    serve(NAME, env!("CARGO_PKG_VERSION"), run)
}

/// One import over the session: validate and lower the envelope, resolve DIR
/// (absent means the current directory the host left the child in), read and
/// plan the history under the ADR-0377 caps, create the range indexes before
/// the first write and the six phases, then the post-commit index and the
/// summary. The importer's failures are its own typed texts; the kit's render
/// through its pinned `Display`, the one text channel the child's stderr
/// carries.
fn run(child: &mut StdioChild, envelope: Envelope) -> Result<(), String> {
    let opts = envelope.opts()?;
    let dir = resolve_dir(envelope.dir)?;
    // Every ADR-0377 refusal fires before the first call: the log cap while
    // reading, the path rule and the pair cap while building the plan
    // (ADR-0392) — an import that would refuse has written nothing.
    let caps = Caps::ADR;
    let history = git_log::read_history(&dir, &opts, caps.log_bytes).map_err(|e| e.to_string())?;
    let built =
        plan::build(&dir, history, opts.max_changeset, caps.pairs).map_err(|e| e.to_string())?;
    let stats = built.stats();

    for statement in plan::RANGE_INDEXES {
        child.update_schema(statement)?;
    }
    for (statement, rows) in built.phases() {
        child.write_rows(statement, rows)?;
    }
    child.update_schema(plan::INDEX_COMMIT_SUBJECT)?;

    let text = format!(
        "imported {} commits, {} people, {} files ({} TOUCHED, {} CO_CHANGED edges) \
         into the store — try:",
        stats.commits, stats.people, stats.files, stats.touched, stats.co_changed
    );
    child.report_output(&text, &starters())?;
    Ok(())
}
