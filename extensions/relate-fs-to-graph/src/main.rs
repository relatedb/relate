//! The `fs-to-graph` extension child (ADR-0431 §11, ADR-0447): the
//! importer's MCP half. Its whole input is one request envelope on stdin —
//! the resolved flag values the host parsed from the committed manifest's
//! flag table — and its whole output is the MCP session it drives as the
//! client: the `initialize` handshake, the three write phases as
//! `write_cypher` calls with their rows, the `file_content` index as one
//! `update_schema` after the write phases, and the run's summary through
//! `report_output`.
//!
//! It carries no `Database` and never touches a store (ADR-0447 §4): the
//! host owns the store, the commit policy, the caps and the exit codes; this
//! child's only power is the calls it makes. The kit's
//! [`serve`](relate_wire::mcp::serve) owns the bootstrap — the envelope
//! read, the handshake, the failure report, the exit — and every failure (a
//! value the host's kind-checked parse could not refuse, an unreadable tree,
//! a cap) prints the importer's own wording on stderr and exits 1, so the
//! host renders it under its `Error:` line.

use relate_fs_to_graph::{
    walk, Envelope, INDEX_FILE_CONTENT, STARTER_QUERIES, WRITE_EDGES, WRITE_FILES, WRITE_FOLDERS,
};
use relate_wire::envelope::resolve_dir;
use relate_wire::mcp::{serve, StdioChild};

/// The mount name the handshake reports — the committed manifest declares
/// the same package (ADR-0430 §3: the manifest declares, the host enforces,
/// the child names itself).
const NAME: &str = "fs-to-graph";

fn main() {
    serve(NAME, env!("CARGO_PKG_VERSION"), run)
}

/// One import over the session: validate and lower the envelope, resolve DIR
/// (absent means the current directory the host left the child in), walk the
/// tree, write the three phases, create the `file_content` index at the
/// import's index-creation point, then report the summary. The importer's
/// failures are its own texts; the kit's render through its pinned `Display`,
/// the one text channel the child's stderr carries.
fn run(child: &mut StdioChild, envelope: Envelope) -> Result<(), String> {
    let opts = envelope.opts()?;
    let dir = resolve_dir(envelope.dir)?;
    let rows = walk(&dir, &opts)?;
    let stats = rows.stats();

    child.write_rows(WRITE_FOLDERS, rows.folders.into_iter())?;
    child.write_rows(WRITE_FILES, rows.files.into_iter())?;
    child.write_rows(WRITE_EDGES, rows.edges.into_iter())?;
    child.update_schema(INDEX_FILE_CONTENT)?;

    let text = format!(
        "imported {} folders, {} files ({} CONTAINS edges) into the store — search it with:",
        stats.folders, stats.files, stats.edges
    );
    let starters: Vec<String> = STARTER_QUERIES.iter().map(|q| q.to_string()).collect();
    child.report_output(&text, &starters)?;
    Ok(())
}
