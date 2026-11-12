//! `.info` (ADR-0165 Decision 4): the read-only session diagnostic — store
//! metadata as a cold `store_info` read beside the open store, live counts
//! and the catalog as Cypher through the open connection. Its only link to
//! the statement-execution machinery is the shared
//! [`crate::exec_query::exec_relate_query`] renderer, used for the catalog
//! readouts like any other session query.

use relate_formatter::{sanitize_error_text, Formatter};

use crate::exec_query::{exec_relate_query, RenderOpts};
use crate::{print_error, store_info_render, telemetry};

/// Run the full `.info` readout (see the module docs for the contract) and
/// return its exit code. Every section is best-effort: a failure is
/// reported and the readout continues, with a non-zero command result.
pub(crate) fn run_info(
    conn: &mut relate::Connection,
    db_path: &str,
    params: &std::collections::BTreeMap<String, relate::Value>,
    formatter: &Formatter,
    storage: telemetry::StorageKind,
) -> i32 {
    let mut code = 0;
    // Memory-vs-persistent was classified once, in `main` (or the `lab`
    // bootstrap), as `query_ctx.storage`; consult it here, exactly as
    // `exit_closed` does, instead of re-deriving it from the database path.
    if storage.is_persistent() {
        info_line("store:", sanitize_error_text(db_path));
        // `store_info_uri` owns the URI classification — a `file://` prefix
        // is stripped, a remote scheme rejected — the same rules
        // `parse_uri_inner` applied when the session opened.
        match relate_storage::store_info_uri(db_path) {
            Ok(info) => print_store_info(&info),
            Err(e) => {
                print_error(format_args!(
                    "Error: .info could not read store metadata: {e}"
                ));
                code = 1;
            }
        }
    } else {
        info_line("store:", ":memory: (store metadata not applicable)");
    }
    // Exact live counts: the graph is already materialized, so `count` over
    // the open connection is the precise answer.
    for (key, query) in [
        ("nodes:", "MATCH (n) RETURN count(n)"),
        ("relationships:", "MATCH ()-[r]->() RETURN count(r)"),
    ] {
        match live_count(conn, query) {
            Ok(n) => info_line(key, n),
            Err(e) => {
                print_error(e);
                code = 1;
            }
        }
    }
    // The current catalog, including WAL-replayed DDL, rendered like any
    // other session query result.
    for (heading, query) in [
        ("indexes:", "SHOW INDEXES"),
        ("constraints:", "SHOW CONSTRAINTS"),
    ] {
        println!("{heading}");
        // `.info` prints the catalog: never capped, no timer line, and it
        // reads only the run's exit code — the ADR-0402 effect facts belong
        // to the session's note policy, not to this readout. The jsonl
        // terminal timing line follows the formatter's run-level setting
        // (ADR-0581) — the readout holds no policy of its own.
        let show_code =
            exec_relate_query(conn, query, params, formatter, RenderOpts::default()).code;
        if show_code != 0 {
            code = show_code;
        }
    }
    code
}

/// One `.info` key/value line, aligned like the rest of the session
/// readout. The width fits the longest key of the shared store block
/// (`wal segments past checkpoint:`, 29 chars) plus one space.
fn info_line(key: &str, value: impl std::fmt::Display) {
    println!("{key:<30}{value}");
}

/// The store-metadata half of `.info` for an open persistent database: the
/// cold `store_info` readout (ADR-0165 Decision 1), rendered from the shared
/// [`store_info_render::InfoReport`] projection so the wording matches
/// `relate info` exactly (commit timestamp included, ADR-0165 Decision
/// 4). A JSON-codec manifest is a codec envelope only (ADR-0165 Decision 5),
/// so the projection reports its format version as unavailable; a fresh
/// store before its first checkpoint reports the `snapshot: none yet` state
/// instead of erroring.
fn print_store_info(info: &relate_storage::StoreInfo) {
    let report = store_info_render::InfoReport::from_store_info(info);
    for (key, value) in report.store_lines() {
        info_line(&format!("{key}:"), value);
    }
}

/// Exact live count via Cypher through the open connection.
fn live_count(conn: &mut relate::Connection, query: &str) -> relate::Result<i64> {
    match conn.query(query)?.rows.first().and_then(|row| row.first()) {
        Some(relate::Value::Int(n)) => Ok(*n),
        _ => Err(relate::Error::internal(
            "count query returned no integer row",
        )),
    }
}
