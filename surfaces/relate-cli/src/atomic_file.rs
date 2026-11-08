//! Shared atomic file writes for CLI-owned files, and the staging-name
//! recipe every staged file the CLI creates shares.
//!
//! Both the telemetry `state.json` / `config.json` files and the self-upgrade
//! binary replacement need the same hardening: stage the bytes at a
//! non-predictable temp name in the *same* directory (guaranteeing the same
//! filesystem, so the rename is atomic), never truncate an existing path, and
//! create the staging file with restrictive permissions from birth rather than
//! chmod-ing a world-readable file afterwards. The remote `--database`
//! bundle (LABS-872) stages into the system temp dir instead of beside a
//! target, so it shares only the name recipe ([`staging_name`]) — the
//! writers below are the write-atomicity half, compiled when one of their
//! two writers is. The telemetry spool's per-run files (ADR-0545 §1) are
//! written directly rather than staged, so they share the creation step
//! ([`create_exclusive`]) with the staging loop below.
//!
//! The module compiles with the `staging` marker feature (the three
//! staging features list it), so the shared name/create step carries no
//! gate; the narrower `telemetry`/`upgrade` gates below mark what only the
//! write-atomicity half needs.

use std::fs;
#[cfg(any(feature = "telemetry", feature = "upgrade"))]
use std::io::Write;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Bounded retries for staging-name collisions (essentially impossible given
/// the name space, but never touch an existing file to recover).
const STAGING_ATTEMPTS: usize = 16;

/// Process-local sequence that makes each staging name unique even within one
/// process (two writes in the same nanosecond still differ).
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A non-predictable staging name built from `prefix` and `suffix`: the
/// prefix plus pid, a process-local sequence, and the current nanosecond
/// timestamp, with `suffix` after it (empty for the write-atomicity staging
/// files, `.rl` for the remote `--database` bundle the engine must
/// recognize). The pid alone is predictable, which would let a hostile
/// co-tenant pre-create or symlink a `.tmp-<pid>` path; mixing in the
/// counter and clock removes that. One recipe for every staged file the CLI
/// creates — the atomic write's staging file and the remote bundle
/// (LABS-872) — so the hardening cannot drift between them.
///
/// A pre-epoch clock makes the timestamp term zero rather than panicking:
/// the process-local counter above is what guarantees uniqueness, so the
/// degraded clock only costs entropy a hostile co-tenant could otherwise
/// guess at.
pub(crate) fn staging_name(prefix: &str, suffix: &str) -> String {
    let seq = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(".{prefix}.tmp-{}-{seq}-{nanos}{suffix}", std::process::id())
}

/// Open `path` for writing, created exclusively with `mode` applied at
/// creation on unix: the one creation step behind every CLI-created
/// private file — the staging kit's collision loop below and the
/// telemetry spool's per-run files (ADR-0545 §1). A path that already
/// exists is refused, never truncated; `mode` is ignored where
/// permission bits do not apply.
pub(crate) fn create_exclusive(path: &Path, mode: u32) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path)
}

/// Create one exclusive staging file in `dir` under [`staging_name`]: the
/// shared creation step of every staged file — [`create_exclusive`] plus
/// a bounded retry on a
/// name collision that surfaces the last collision rather than inventing an
/// error. The caller owns what follows: a write and a rename
/// ([`write_atomic`]) or a write and an open (the remote `--database`
/// bundle).
pub(crate) fn create_staging_file(
    dir: &Path,
    prefix: &str,
    suffix: &str,
    mode: u32,
) -> io::Result<(PathBuf, fs::File)> {
    let mut collision = None;
    for _ in 0..STAGING_ATTEMPTS {
        let path = dir.join(staging_name(prefix, suffix));
        match create_exclusive(&path, mode) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => collision = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(collision.expect("every attempt returns or records its collision"))
}

/// Write `bytes` to `path` atomically-ish: create a uniquely named staging file
/// in the same directory with `create_new` (so an existing file is never
/// truncated), apply `mode` at creation on unix, then rename over `path`. On
/// any failure before the rename the staging file is removed and the target is
/// left untouched; a failure of the post-rename directory sync is returned as
/// `Err` even though the rename already replaced the target.
///
/// `mode` is the unix permission bits for the final file (`0o600` for private
/// telemetry state, `0o755` for the replacement binary); it is ignored on
/// non-unix platforms, where permission bits do not apply.
#[cfg(any(feature = "telemetry", feature = "upgrade"))]
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", path.display()),
        )
    })?;
    // A bare relative target ("state.json") has an empty parent path; treat
    // it as the current directory so the directory fsync does not open "".
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("relate");

    let (tmp, mut file) = create_staging_file(dir, name, "", mode)?;
    let staged = file.write_all(bytes).and_then(|()| file.sync_all());
    // Close the staging file before renaming: on Windows an open handle
    // blocks the rename.
    drop(file);
    // The rename may already have replaced the target when the post-rename
    // directory sync runs, so a sync error is not a "target untouched"
    // failure.
    let result = staged
        .and_then(|()| fs::rename(&tmp, path))
        .and_then(|()| sync_parent_dir(dir));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// fsync the parent directory so the rename itself is durable, not just the
/// file contents. Non-unix platforms cannot portably open a directory for
/// syncing, so this is a no-op there.
#[cfg(any(feature = "telemetry", feature = "upgrade"))]
#[cfg(unix)]
fn sync_parent_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(any(feature = "telemetry", feature = "upgrade"))]
#[cfg(not(unix))]
fn sync_parent_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

// The write-atomicity half's tests, compiled with it.
#[cfg(all(test, any(feature = "telemetry", feature = "upgrade")))]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn staging_names_are_unique_and_not_pid_predictable() {
        // Many names in a tight loop: even if the clock is frozen (same
        // nanosecond for every call) the process-local counter must keep every
        // name distinct.
        let names: Vec<String> = (0..1024).map(|_| staging_name("state.json", "")).collect();
        let unique: HashSet<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "staging names must all be unique across calls"
        );

        let pid = std::process::id();
        // The old predictable form was exactly `.<name>.tmp-<pid>`; a hostile
        // co-tenant must not be able to pre-create or symlink it.
        let predictable = format!(".state.json.tmp-{pid}");
        for name in &names {
            assert_ne!(name, &predictable, "staging name is pid-predictable");
            assert!(
                name.starts_with(".state.json.tmp-"),
                "unexpected staging name shape: {name}"
            );
            // The pid-only prefix must not be the whole name: the counter and
            // clock suffix have to contribute variation.
            assert!(
                name.len() > predictable.len(),
                "staging name carries no sequence/clock suffix: {name}"
            );
        }
    }

    #[test]
    fn write_atomic_accepts_bare_relative_target() {
        // A bare relative filename has an empty parent path; the directory
        // sync must treat it as the current directory rather than failing
        // with ENOENT after the rename already succeeded.
        static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = write_atomic(Path::new("bare.json"), b"data", 0o600);
        std::env::set_current_dir(&original).unwrap();
        result.unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("bare.json")).unwrap(),
            "data"
        );
    }

    #[test]
    fn write_atomic_replaces_target_and_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("config.json");
        write_atomic(&target, b"first", 0o600).unwrap();
        write_atomic(&target, b"second", 0o600).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "second");
        let entries: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "only the target should remain");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_sets_mode_at_creation() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("state.json");
        write_atomic(&target, b"secret", 0o600).unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got mode {mode:o}");
    }
}
