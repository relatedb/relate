//! Query-latency suite on the graph-benchmark-ldbc workload (ADR-0243).
//!
//! The 30 Cypher queries of prrao87/graph-benchmark-ldbc run verbatim
//! against the LDBC SNB `CsvBasic-StringDateFormatter` data set, loaded into
//! a [`relate::Database`] with upstream's schema. Each query's answer is
//! checked against frozen expected rows and its latency is measured over
//! repeated rounds, recording both wall time and the engine-reported
//! statement elapsed (ADR-0221).
//!
//! The pieces, in the order a run uses them:
//!
//! - [`fetch`] downloads and verifies the data set (sha256-pinned, never
//!   vendored), extracting it with pure-Rust zstd and tar;
//! - [`load`] builds the graph with batched `UNWIND $rows` statements from
//!   the [`schema`] table, then checks every count;
//! - [`run`] times each query under a cancellable per-query timeout;
//! - [`answers`] compares rows with the expected ones (upstream's rules,
//!   no type coercion);
//! - [`report`] is the JSON report and the console table;
//! - [`suite`] ties them into one run;
//! - [`profile`] loops one query under a sampling profiler (ADR-0244),
//!   and the sampled-stack model — `relate_test_support::stacks`, shared
//!   with the bulk harness — attributes and renders what it sampled;
//! - [`corpus`] runs the QPP/LDBC workloads (ADR-0479): the translated
//!   PathDB RPQ corpus over the same data, and the in-house fraud QPP
//!   suite — run-and-report with an ok-set ratchet, no latency gate.
//!
//! The workload input helpers are shared by every workload (`graph-benchmark-ldbc`'s
//! queries and index scripts, the corpus workloads' query files and
//! fixtures) and live here at the crate root: [`split_header`] (a
//! leading `//` header, then exactly one statement without a trailing
//! `;`) and [`read`] (read a file, its error naming the path).
//!
//! Not an LDBC Benchmark; see `NOTICE` for attribution and fair use.

pub mod answers;
pub mod corpus;
pub mod fetch;
pub mod load;
pub mod profile;
pub mod report;
pub mod run;
pub mod schema;
pub mod suite;
pub mod workload;

use std::path::{Path, PathBuf};

/// Every fallible step reports a human-readable message: the suite is a
/// harness, and its callers print the error and exit.
pub type Result<T> = std::result::Result<T, String>;

/// The suite's working directory, the root of everything it writes (data
/// cache, reports, stores, profiles): `$CARGO_TARGET_DIR/query-latency`
/// when that is set, else the workspace's `target/query-latency`.
pub fn work_root() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // testing/<crate>/ → the workspace root.
            let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest
                .ancestors()
                .nth(2)
                .unwrap_or(manifest)
                .join("target")
        });
    target.join("query-latency")
}

/// The `workloads/` root in this crate, beside [`work_root`] (the
/// suite's working directory): a crate-wide fact — every workload's
/// static inputs live under it (the graph-benchmark-ldbc workload's own
/// directory in [`workload`], the corpus workloads' directories via
/// [`corpus::Workload::dir`]) — and the one derivation every
/// workload-directory spelling consults.
pub(crate) fn workloads_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("workloads")
}

/// Split a workload input file into its header comment and its one
/// statement: the file format every workload's `.cypher` inputs share —
/// `graph-benchmark-ldbc`'s query files and index scripts, the corpus workloads' query
/// files and fixtures. A file holds exactly one statement without a
/// trailing `;`.
pub fn split_header(file: &str) -> Result<(String, String)> {
    let lines: Vec<&str> = file.trim().lines().collect();
    let body_start = lines
        .iter()
        .position(|l| !l.trim_start().starts_with("//"))
        .unwrap_or(lines.len());
    let header = lines[..body_start].join("\n");
    let body = lines[body_start..].join("\n").trim().to_string();
    if body.is_empty() || body.ends_with(';') {
        return Err("a query file holds one statement without a trailing ';'".into());
    }
    Ok((header, body))
}

/// Read a workload input file, the read error naming the path — the one
/// spelling of "read a file" every workload loader in the crate shares.
pub fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

/// Remove the file at `path` if it exists. Fail closed: a missing file
/// is the expected case and ignored, but every other error (a
/// permissions failure, a directory at the path, a locked file on
/// Windows) is returned as `removing <path>: <error>` — the one policy
/// for the suite's stamp and marker removals, each of which would
/// otherwise let a stale file certify what it no longer describes.
pub(crate) fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("removing {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::{remove_if_present, split_header};

    #[test]
    fn remove_if_present_ignores_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        remove_if_present(&dir.path().join(".ds.ok")).unwrap();
    }

    #[test]
    fn remove_if_present_propagates_a_non_notfound_error() {
        let dir = tempfile::tempdir().unwrap();
        // A directory at the stamp path fails remove_file with a kind
        // other than NotFound; the fail-closed contract propagates it.
        let stamp = dir.path().join(".ds.ok");
        std::fs::create_dir(&stamp).unwrap();
        let err = remove_if_present(&stamp).unwrap_err();
        assert!(err.contains("removing"), "{err}");
    }

    #[test]
    fn header_split_rejects_trailing_semicolon_and_empty_body() {
        assert!(split_header("// h\nRETURN 1;").is_err());
        assert!(split_header("// only a header").is_err());
        let (h, b) = split_header("// a\n// b\nMATCH (n)\nRETURN n\n").unwrap();
        assert_eq!(h, "// a\n// b");
        assert_eq!(b, "MATCH (n)\nRETURN n");
    }
}
