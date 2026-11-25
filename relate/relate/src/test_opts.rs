//! Open presets and closed-gate preconditions shared by unit tests and
//! `tests/`.
use std::env::VarError;
use std::time::{Duration, Instant};

use super::{Database, DurabilityMode, OpenMode, OpenOptions, Value};

/// Assert the process does not opt into any preview feature: the closed-gate
/// fixture precondition (ADR-0371). `RELATE_PREVIEW` must be unset or
/// blank — `Database::open` resolves it from the live process environment, so
/// a dev shell exporting it silently changes what a fail-closed fixture tests;
/// this fails loudly instead, naming the variable and the leaked setting.
#[doc(hidden)]
pub fn assert_preview_unset() {
    if let Err(leaked) = closed_preview_setting(std::env::var("RELATE_PREVIEW")) {
        panic!(
            "the test environment sets {leaked}; unset it for the closed-gate fixture \
             (Database::open reads RELATE_PREVIEW from the process environment)"
        );
    }
}

/// Classify one `RELATE_PREVIEW` read: `Ok(())` when the setting is absent
/// or blank, `Err(leaked)` naming it otherwise. Split from the environment read
/// so both branches are unit-tested without mutating process-global state.
fn closed_preview_setting(setting: Result<String, VarError>) -> Result<(), String> {
    match setting {
        Ok(value) if value.trim().is_empty() => Ok(()),
        Ok(value) => Err(format!("RELATE_PREVIEW={value:?}")),
        Err(VarError::NotPresent) => Ok(()),
        Err(VarError::NotUnicode(raw)) => {
            Err(format!("RELATE_PREVIEW={raw:?} (not valid Unicode)"))
        }
    }
}

#[doc(hidden)]
pub fn writable() -> OpenOptions {
    OpenOptions {
        checkpoint_interval: Some(Duration::ZERO),
        ..OpenOptions::default()
    }
}

/// [`writable`] with `DurabilityMode::EverySecond`: a commit is acknowledged
/// once it is applied, and the WAL becomes durable on the one-second timer.
#[doc(hidden)]
pub fn every_second() -> OpenOptions {
    OpenOptions {
        durability_mode: Some(DurabilityMode::EverySecond),
        ..writable()
    }
}

#[doc(hidden)]
pub fn read_only() -> OpenOptions {
    OpenOptions {
        mode: OpenMode::ReadOnly,
        checkpoint_interval: Some(Duration::ZERO),
        ..OpenOptions::default()
    }
}

#[doc(hidden)]
pub fn following() -> OpenOptions {
    OpenOptions {
        mode: OpenMode::Follow,
        checkpoint_interval: Some(Duration::ZERO),
        ..OpenOptions::default()
    }
}

#[doc(hidden)]
pub use crate::publish::set_fail_after_dest;

#[doc(hidden)]
pub use crate::publish::{set_plant_destination, PlantShape, PLANTED_SENTINEL};

#[doc(hidden)]
pub fn count(db: &Database, label: &str) -> i64 {
    let rows = db
        .connect()
        .unwrap()
        .query(&format!("MATCH (n:{label}) RETURN count(n)"))
        .unwrap()
        .rows;
    match &rows[0][0] {
        Value::Int(n) => *n,
        other => panic!("count was {other:?}"),
    }
}

/// Poll `db` until `:label` holds `expect` nodes — the one follower poller
/// and timeout policy shared by the in-crate follow suite and `tests/`.
/// `count` panics on a follow error, so a stuck follower names the reason.
#[doc(hidden)]
pub fn wait_count(db: &Database, label: &str, expect: i64) {
    let start = Instant::now();
    loop {
        if count(db, label) == expect {
            return;
        }
        if start.elapsed() > Duration::from_secs(3) {
            panic!(
                "timed out waiting for {expect} :{label}, have {}",
                count(db, label)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A store's published image, stamped for the skip tests: the bytes and the
/// modification time of `path` (a manifest or sidecar). A rewrite, even of
/// identical bytes through the tmp/rename publication, stamps a fresh mtime,
/// so equality of two images proves nothing was published between them.
#[doc(hidden)]
pub fn published_image(path: &std::path::Path) -> (Vec<u8>, std::time::SystemTime) {
    const PUBLISHED: &str = "the store published this image before it was measured";
    let bytes = std::fs::read(path).expect(PUBLISHED);
    let mtime = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .expect(PUBLISHED);
    (bytes, mtime)
}

/// The WAL segment files of the native database at `root`. The `wal/`
/// directory also holds the lease lock files, which are not segments.
///
/// This reads the directory itself and re-spells the `*.log` naming that
/// `relate-storage` owns, on purpose: the tests that use it are oracles for
/// what storage left on disk, so they must not ask the code under test which
/// files it wrote. `tests/common/store.rs::active_segment` is the same
/// reading through a `Vfs`, for tests that also cover bundles.
#[doc(hidden)]
pub fn wal_segments(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(root.join("wal"))
        .expect("a native store has a wal/ directory")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("log"))
        .collect()
}

/// The newest (highest-numbered) WAL segment of the native database at
/// `root`: the one a writer appends to and a follower tails.
#[doc(hidden)]
pub fn latest_wal_segment(root: &std::path::Path) -> std::path::PathBuf {
    wal_segments(root)
        .into_iter()
        .max()
        .expect("the store wrote a WAL segment")
}

/// The store's current WAL LSN: the count of committed records, each commit
/// advancing it by one. The bulk-write batching tests (ADR-0209) count
/// batches as LSN deltas through this seam. Like every other `Database`
/// durable-state read it takes the publication read guard first, so a
/// poisoned store answers `Error::Storage` rather than a stale LSN.
#[doc(hidden)]
pub fn wal_lsn(db: &Database) -> crate::Result<u64> {
    let _read = db.inner.read_guard()?;
    Ok(db.inner.wal.lock()?.lsn)
}

#[cfg(test)]
mod tests {
    use std::env::VarError;
    use std::ffi::OsString;

    use super::closed_preview_setting;

    /// Absent and blank settings both leave every preview feature closed.
    #[test]
    fn closed_preview_setting_accepts_absent_and_blank_values() {
        for setting in [
            Err(VarError::NotPresent),
            Ok(String::new()),
            Ok(" \t\n".to_string()),
        ] {
            assert_eq!(
                closed_preview_setting(setting),
                Ok(()),
                "a blank RELATE_PREVIEW disables every preview feature"
            );
        }
    }

    /// A leaked opt-in is reported with the variable name and the value, so a
    /// developer can find the shell that exported it.
    #[test]
    fn closed_preview_setting_names_a_leaked_opt_in() {
        let leaked = closed_preview_setting(Ok("ontology".to_string())).unwrap_err();
        assert!(leaked.contains("RELATE_PREVIEW"), "{leaked}");
        assert!(leaked.contains("ontology"), "{leaked}");

        let leaked =
            closed_preview_setting(Err(VarError::NotUnicode(OsString::from("x")))).unwrap_err();
        assert!(leaked.contains("RELATE_PREVIEW"), "{leaked}");
        assert!(leaked.contains("not valid Unicode"), "{leaked}");
    }
}
