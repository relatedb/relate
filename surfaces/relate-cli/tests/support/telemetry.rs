//! The telemetry fixtures' support cluster: the spool-side helpers the
//! telemetry-delivering e2e suites share — the file-name constants, the
//! pending/contents scans, the seed writer, and the work query a
//! delivery follow-up run executes. The sink side lives in
//! `mock_mixpanel.rs`; both are re-exported by the parent so the suites
//! import from `support` as before.

use std::path::Path;

/// The telemetry spool's file-name prefix, pinned for the e2e suites —
/// the e2e twin of the CLI's own fact (`SPOOL_PREFIX` in
/// `src/telemetry/spool.rs`; e2e binaries cannot reach the bin's private
/// items). Every e2e spelling of a spool file name — scans and seeds —
/// derives from this one constant, like the other pinned strings here.
#[allow(dead_code)]
pub const SPOOL_FILE_PREFIX: &str = "telemetry-spool-";

/// The claim file-name prefix, the e2e twin of `CLAIM_PREFIX` — a
/// sweep's in-flight rename target. The pending-file scan below counts
/// both, so a drain that has claimed (renamed) files mid-test can never
/// make the spool look falsely empty.
#[allow(dead_code)]
pub const CLAIM_FILE_PREFIX: &str = "telemetry-claim-";

/// The delivery follow-ups' work query (ADR-0545 §2): a follow-up's
/// whole job is to stay alive long enough for its drain thread to POST,
/// and engine work buys that window even on a saturated runner where a
/// `RETURN 1` run may live only milliseconds. The one spelling of the
/// query, shared by every suite's follow-up closure — the env wiring
/// around it stays per-suite.
#[allow(dead_code)]
pub const DELIVERY_WORK_QUERY: &str = "UNWIND range(1, 100000) AS i RETURN count(i) AS c";

/// The one directory walk both scans read: the regular entries under
/// `dir` whose names start with any of `prefixes`, in read order. The
/// two named predicates below are the fixed-arity shapes over it, so
/// the walk's shape cannot drift between them — the same split the
/// production sweep uses (`spool_paths` + the name grammar).
#[allow(dead_code)]
fn telemetry_files_in(dir: &Path, prefixes: &[&str]) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| prefixes.iter().any(|prefix| name.starts_with(prefix)))
        })
        .map(|entry| entry.path())
        .collect()
}

/// The `telemetry-spool-*.jsonl` files currently under `dir` — the one
/// spool-scan helper for the e2e suites, so the scan shape cannot drift
/// between them.
#[allow(dead_code)]
pub fn spool_files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    telemetry_files_in(dir, &[SPOOL_FILE_PREFIX])
}

/// The pending telemetry files currently under `dir` — spool files and
/// claims alike, the sweep's whole vocabulary: a drain renames spool
/// files to claim names while it delivers, so a scan that counts only
/// one prefix can race the drain and report a falsely empty spool.
#[allow(dead_code)]
pub fn pending_telemetry_files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    telemetry_files_in(dir, &[SPOOL_FILE_PREFIX, CLAIM_FILE_PREFIX])
}

/// Seed one pending spool file carrying `marker` in the config
/// directory — the delivery tests' backlog, written the way the run
/// before it would have written it, name grammar included. Returns the
/// seeded path for the assertions that check it survives untouched.
#[allow(dead_code)]
pub fn seed_spool(cfg: &Path, marker: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(cfg).expect("create config dir");
    let path = cfg.join(format!("{SPOOL_FILE_PREFIX}{marker}.jsonl"));
    let line = serde_json::json!({"event": marker}).to_string();
    std::fs::write(&path, format!("{line}\n")).expect("seed spool");
    path
}

/// Each pending telemetry file's name and contents — the delivery
/// tests' one view of the residue, on top of
/// [`pending_telemetry_files_in`]: the reads fail loudly, because a
/// file the scan just listed that cannot be read is a broken fixture,
/// not an empty one.
#[allow(dead_code)]
pub fn pending_telemetry_contents_in(dir: &Path) -> Vec<(String, String)> {
    pending_telemetry_files_in(dir)
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("a pending file carries a name")
                .to_string_lossy()
                .into_owned();
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read pending telemetry file {name}: {e}"));
            (name, contents)
        })
        .collect()
}
