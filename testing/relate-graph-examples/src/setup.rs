//! The pull/setup plumbing of a dataset run's artifact, pull and open
//! phases (ADR-0144): fetch the sha-pinned artifacts, pull a fresh copy
//! through the real CLI, open it (database registration — phase 2 — is one
//! call, made inline in the suite).

use std::path::{Path, PathBuf};

use crate::cache::{ensure_adoc, ensure_dump};
use crate::docker::DatasetTest;
use crate::manifest::Dataset;
use crate::pull::{is_transient_failure, pull_whole_database};

/// Phase 1 — artifacts: dump + adoc, cached and sha-pinned. Returns the
/// adoc text.
pub(crate) fn fetch_artifacts(dataset: &Dataset, fetch: bool) -> Result<String, String> {
    let _dump = ensure_dump(dataset, fetch)?;
    let adoc_path = ensure_adoc(dataset, fetch)?;
    std::fs::read_to_string(&adoc_path).map_err(|e| format!("reading {}: {e}", adoc_path.display()))
}

/// Phase 3 — pull the whole database into a fresh directory through the
/// CLI; returns the pulled copy's directory. A transient stream failure
/// (ADR-0204: the reference closed the response mid-statement) is retried
/// exactly once from scratch; every other failure is returned as-is.
pub(crate) fn pull_fresh_copy(
    reference: &DatasetTest,
    dataset: &Dataset,
    workdir: &Path,
) -> Result<PathBuf, String> {
    let destination = workdir.join(dataset.name);
    let env_file = workdir.join(format!("{}.env", dataset.name));
    write_env_file(reference, dataset, &env_file)?;
    with_one_transient_retry(dataset.name, || {
        pull_into_fresh_destination(dataset, &destination, &env_file)
    })?;
    Ok(destination)
}

/// The pull env file (written once; a retry reuses it). The env file
/// carries the reference password; keep it owner-only (pull warns on
/// group/world-readable env files).
fn write_env_file(
    reference: &DatasetTest,
    dataset: &Dataset,
    env_file: &Path,
) -> Result<(), String> {
    std::fs::write(env_file, reference.pull_env_file(dataset.name))
        .map_err(|e| format!("writing {}: {e}", env_file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(env_file, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod 600 {}: {e}", env_file.display()))?;
    }
    Ok(())
}

/// Wipe and recreate the destination, then pull into it. The destination
/// must be wiped before every attempt: pull MERGEs into an existing graph,
/// and adoc walkthrough blocks mutate, so a leftover directory from a
/// previous run — or from the failed first attempt of a retry — would seed
/// the next one with stale data.
fn pull_into_fresh_destination(
    dataset: &Dataset,
    destination: &Path,
    env_file: &Path,
) -> Result<(), String> {
    reset_destination(destination)?;
    let _pull_log = pull_whole_database(dataset.name, destination, env_file)?;
    Ok(())
}

/// Remove a stale destination directory and recreate it empty.
fn reset_destination(destination: &Path) -> Result<(), String> {
    if destination.exists() {
        std::fs::remove_dir_all(destination)
            .map_err(|e| format!("removing stale {}: {e}", destination.display()))?;
    }
    std::fs::create_dir_all(destination)
        .map_err(|e| format!("creating {}: {e}", destination.display()))
}

/// The ADR-0204 retry policy: run `attempt`; when it fails with a
/// transient stream-disconnect signature ([`is_transient_failure`], the
/// pull-transport classifier that lives next to `pull_once`), log to
/// stderr and re-run it exactly once. A retry that also fails returns
/// the second error, marked so the report shows one retry happened.
/// Permanent failures (auth, syntax, a timebox kill) never retry.
fn with_one_transient_retry<T>(
    dataset: &str,
    mut attempt: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    match attempt() {
        Ok(done) => Ok(done),
        Err(first) if is_transient_failure(&first) => {
            eprintln!("{dataset}: transient pull stream failure; retrying the pull once from scratch: {first}");
            attempt().map_err(|second| format!("{second} (failed again after one retry)"))
        }
        Err(permanent) => Err(permanent),
    }
}

/// Phase 4 — open the pulled copy and hand the opened database back. No
/// store injection here: the attach hook is a suite-level concern
/// (ADR-0665), applied to this handle while it is quiescent — the
/// precondition the hook documents.
pub(crate) fn open_pulled_copy(destination: &Path) -> Result<relate::Database, String> {
    relate::Database::open(destination.to_string_lossy().as_ref())
        .map_err(|e| format!("opening pulled {}: {e}", destination.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0204: exactly one retry on a transient failure (from scratch, so
    /// the attempt closure re-runs), none on a permanent one, and a failed
    /// retry surfaces the second error marked with the retry.
    #[test]
    fn retry_policy_retries_transient_once_and_never_permanent() {
        use std::cell::Cell;

        // Transient, then success: two attempts, second result wins.
        let calls = Cell::new(0);
        let out = with_one_transient_retry("movies", || {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err("reading response body failed mid-statement: Peer disconnected".to_string())
            } else {
                Ok(7)
            }
        });
        assert_eq!(out, Ok(7));
        assert_eq!(calls.get(), 2, "one transient failure retries exactly once");

        // Permanent: no retry, error unchanged.
        let calls = Cell::new(0);
        let out: Result<(), String> = with_one_transient_retry("movies", || {
            calls.set(calls.get() + 1);
            Err("authentication failed".to_string())
        });
        assert_eq!(out, Err("authentication failed".to_string()));
        assert_eq!(calls.get(), 1, "a permanent failure must not retry");

        // Retry also fails: the second error, marked with the retry.
        let calls = Cell::new(0);
        let out: Result<(), String> = with_one_transient_retry("movies", || {
            calls.set(calls.get() + 1);
            Err("failed mid-statement: Peer disconnected".to_string())
        });
        let error = out.unwrap_err();
        assert!(error.contains("failed mid-statement"), "{error}");
        assert!(error.contains("failed again after one retry"), "{error}");
        assert_eq!(calls.get(), 2, "no more than one retry");
    }

    /// The destination reset wipes stale contents (so a retry never merges
    /// into a half-pulled graph) and works on a missing directory too.
    #[test]
    fn reset_destination_wipes_stale_contents_and_creates_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let destination = tmp.path().join("movies");
        std::fs::create_dir_all(&destination).expect("seed dir");
        std::fs::write(destination.join("stale.wal"), b"x").expect("seed file");
        reset_destination(&destination).expect("reset existing");
        assert!(destination.is_dir());
        assert!(
            std::fs::read_dir(&destination)
                .expect("read dir")
                .next()
                .is_none(),
            "stale contents must be gone"
        );
        std::fs::remove_dir_all(&destination).expect("remove");
        reset_destination(&destination).expect("reset missing");
        assert!(destination.is_dir(), "a missing destination is created");
    }
}
