//! End-to-end tests for `relate clone SOURCE DEST` across the dir/`.rl`
//! matrix (ADR-0156, ADR-0282). The command is built into every CLI bundle,
//! so these run under `full`, `agent`, and `minimal` alike.

use crate::support;

use support::{
    bundle_manifest_codec, manifest_codec, run_cli, seed_database, CliRunOptions as Opts,
};

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

const SEED: &str = "CREATE (:N {name: 'ada'})";

#[test]
fn test_help_lists_clone_but_not_snapshot() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["clone", "--help"]).in_home(home.path()));
    assert!(status.success(), "clone --help failed: {stderr}");
    assert!(
        stdout.contains(
            "relate clone [--wal-dir DIR] [--to-snapshot-format F] \
             [--markdown-body-properties LIST] SOURCE DEST"
        ),
        "expected the clone synopsis in clone --help, got: {stdout}"
    );
    assert!(
        stdout.contains("--wal-dir"),
        "expected --wal-dir in the clone help, got: {stdout}"
    );
    assert!(
        stdout.contains("--to-snapshot-format"),
        "expected --to-snapshot-format in the clone help, got: {stdout}"
    );
    // ADR-0282: the snapshot verb is removed outright, so the help must not
    // list it.
    assert!(
        !stdout.contains("relate snapshot"),
        "clone --help must not list the removed snapshot command, got: {stdout}"
    );
}

#[test]
fn test_snapshot_word_now_names_a_database() {
    // ADR-0282: `snapshot` left COMMAND_WORDS. The store of that name is
    // `--database snapshot`; the first positional is the statement.
    let home = tempdir();
    let cwd = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "snapshot", "RETURN 1 AS n"])
            .in_home(home.path())
            .in_dir(cwd.path()),
    );
    assert!(status.success(), "query on ./snapshot failed: {stderr}");
    assert!(stdout.contains('1'), "expected output, got {stdout}");
    assert!(
        cwd.path().join("snapshot").is_dir(),
        "expected a database directory named snapshot"
    );
}

#[test]
fn test_clone_round_trip_dir_to_bundle_to_dir() {
    let home = tempdir();
    let db = home.path().join("db");
    let bundle = home.path().join("backup.rl");
    let restored = home.path().join("restored");
    seed_database(home.path(), &db, &[], SEED);

    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["clone", db.to_str().unwrap(), bundle.to_str().unwrap()]).in_home(home.path()),
    );
    assert!(status.success(), "clone dir->bundle failed: {stderr}");
    assert!(bundle.is_file(), "expected the bundle file at {bundle:?}");

    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "clone",
            bundle.to_str().unwrap(),
            restored.to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert!(status.success(), "clone bundle->dir failed: {stderr}");

    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--database",
            restored.to_str().unwrap(),
            "MATCH (n:N) RETURN n.name AS name",
        ])
        .in_home(home.path()),
    );
    assert!(status.success(), "query on the clone failed: {stderr}");
    assert!(
        stdout.contains("ada"),
        "clone missed the source row: {stdout}"
    );
}

// Unix-only: Windows rejects control characters in file names (os error
// 123), so the crafted path cannot be created there — the sanitization path
// is exercised on unix.
#[cfg(unix)]
#[test]
fn test_clone_error_line_sanitizes_control_characters() {
    // A crafted path carrying an ANSI escape introducer must not reach the
    // terminal raw (control-sequence injection). clone prints no success
    // line, so the echo surface is the error path: the engine error
    // interpolates the raw path and the CLI's stderr sanitizer strips the
    // ESC byte. Sanitization is display-only — the engine still receives
    // the raw path.
    let home = tempdir();
    let db = home.path().join("db");
    seed_database(home.path(), &db, &[], SEED);

    let dest = home.path().join("evil\x1b[31m-missing").join("bundle.rl");
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["clone", db.to_str().unwrap(), dest.to_str().unwrap()]).in_home(home.path()),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("clone destination parent does not exist"),
        "expected the missing-parent refusal, got: {stderr}"
    );
    assert!(
        stderr.contains("evil[31m"),
        "expected the sanitized path remainder in stderr, got: {stderr:?}"
    );
    assert!(
        !stderr.contains('\x1b'),
        "a raw ESC byte reached stderr: {stderr:?}"
    );
}

#[test]
fn test_clone_bundle_to_bundle() {
    let home = tempdir();
    let db = home.path().join("db");
    let bundle = home.path().join("backup.rl");
    let copy = home.path().join("copy.rl");
    seed_database(home.path(), &db, &[], SEED);

    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["clone", db.to_str().unwrap(), bundle.to_str().unwrap()]).in_home(home.path()),
    );
    assert!(status.success(), "clone dir->bundle failed: {stderr}");
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["clone", bundle.to_str().unwrap(), copy.to_str().unwrap()])
            .in_home(home.path()),
    );
    assert!(status.success(), "clone bundle->bundle failed: {stderr}");
    assert!(copy.is_file(), "expected the bundle file at {copy:?}");

    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--database",
            copy.to_str().unwrap(),
            "MATCH (n:N) RETURN n.name AS name",
        ])
        .in_home(home.path()),
    );
    assert!(status.success(), "query on the copy failed: {stderr}");
    assert!(
        stdout.contains("ada"),
        "copy missed the source row: {stdout}"
    );
}

#[test]
fn test_clone_wal_dir_names_the_directory_source_wal() {
    // `--wal-dir` may precede or follow the positionals; it is the source
    // WAL of a directory source.
    let home = tempdir();
    let db = home.path().join("db");
    let wal = home.path().join("wal");
    let bundle = home.path().join("backup.rl");
    seed_database(
        home.path(),
        &db,
        &["--wal-dir", wal.to_str().unwrap()],
        SEED,
    );

    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "clone",
            "--wal-dir",
            wal.to_str().unwrap(),
            db.to_str().unwrap(),
            bundle.to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert!(status.success(), "clone --wal-dir failed: {stderr}");
    assert!(bundle.is_file(), "expected the bundle file at {bundle:?}");
}

#[test]
fn test_clone_wal_dir_with_a_bundle_source_fails() {
    // A bundle embeds its WAL (ADR-0160/ADR-0282): `--wal-dir` with a `.rl`
    // source is a runtime refusal, not a usage error. The canonical check
    // lives in the storage layer (ADR-0074, ADR-0163 §4).
    let home = tempdir();
    let db = home.path().join("db");
    let bundle = home.path().join("backup.rl");
    seed_database(home.path(), &db, &[], SEED);
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["clone", db.to_str().unwrap(), bundle.to_str().unwrap()]).in_home(home.path()),
    );
    assert!(status.success(), "clone dir->bundle failed: {stderr}");

    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "clone",
            "--wal-dir",
            "./w",
            bundle.to_str().unwrap(),
            home.path().join("out").to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("wal_dir is not supported for .rl"),
        "expected the wal_dir refusal, got: {stderr}"
    );
}

#[test]
fn test_clone_rejects_unknown_flag_and_extra_positional() {
    let home = tempdir();
    for args in [
        vec!["clone", "--bogus", "./a", "./b"],
        vec!["clone", "./a", "./b", "./c"],
    ] {
        let (status, _stdout, stderr) = run_cli(Opts::new(&args).in_home(home.path()));
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains("unexpected argument to clone"),
            "{args:?}: expected a usage error, got: {stderr}"
        );
    }
}

#[test]
fn test_clone_refuses_an_existing_destination_both_kinds() {
    let home = tempdir();
    let db = home.path().join("db");
    let bundle = home.path().join("backup.rl");
    let restored = home.path().join("restored");
    seed_database(home.path(), &db, &[], SEED);

    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["clone", db.to_str().unwrap(), bundle.to_str().unwrap()]).in_home(home.path()),
    );
    assert!(status.success(), "first clone failed: {stderr}");
    // Second export to the same bundle: refused (ADR-0161).
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["clone", db.to_str().unwrap(), bundle.to_str().unwrap()]).in_home(home.path()),
    );
    assert_eq!(status.code(), Some(1), "re-clone must fail: {stderr}");
    assert!(
        stderr.contains("already exists"),
        "expected the destination refusal, got: {stderr}"
    );

    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "clone",
            bundle.to_str().unwrap(),
            restored.to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert!(status.success(), "first restore clone failed: {stderr}");
    // Second clone into the same directory: refused (ADR-0161).
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "clone",
            bundle.to_str().unwrap(),
            restored.to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(1), "re-clone must fail: {stderr}");
    assert!(
        stderr.contains("already exists"),
        "expected the destination refusal, got: {stderr}"
    );
}

/// The shared flow of the codec-preservation/transcode tests: seed `db`
/// (opened with `--snapshot-format json`, so the manifest assertion below is
/// fixed), clone it to a bundle with `clone1_args` asserting the bundle
/// manifest carries `want_bundle_codec`, clone the bundle back to a
/// directory with `clone2_args` asserting `want_restored_codec`, then query
/// the restored clone for the seeded row.
fn assert_clone_both_ways(
    clone1_args: &[&str],
    want_bundle_codec: &str,
    clone2_args: &[&str],
    want_restored_codec: &str,
) {
    let home = tempdir();
    let db = home.path().join("db");
    let bundle = home.path().join("backup.rl");
    let restored = home.path().join("restored");
    seed_database(home.path(), &db, &["--snapshot-format", "json"], SEED);
    assert_eq!(manifest_codec(&db), "json");

    let mut args: Vec<&str> = vec!["clone"];
    args.extend_from_slice(clone1_args);
    args.extend([db.to_str().unwrap(), bundle.to_str().unwrap()]);
    let (status, _stdout, stderr) = run_cli(Opts::new(&args).in_home(home.path()));
    assert!(status.success(), "clone dir->bundle failed: {stderr}");
    assert_eq!(
        bundle_manifest_codec(&bundle),
        want_bundle_codec,
        "the bundle must carry the expected codec (ADR-0282, ADR-0163)"
    );

    let mut args: Vec<&str> = vec!["clone"];
    args.extend_from_slice(clone2_args);
    args.extend([bundle.to_str().unwrap(), restored.to_str().unwrap()]);
    let (status, _stdout, stderr) = run_cli(Opts::new(&args).in_home(home.path()));
    assert!(status.success(), "clone bundle->dir failed: {stderr}");
    assert_eq!(
        manifest_codec(&restored),
        want_restored_codec,
        "the cloned directory must carry the expected codec (ADR-0282, ADR-0163)"
    );

    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--database",
            restored.to_str().unwrap(),
            "MATCH (n:N) RETURN n.name AS name",
        ])
        .in_home(home.path()),
    );
    assert!(status.success(), "query on the clone failed: {stderr}");
    assert!(
        stdout.contains("ada"),
        "clone missed the source row: {stdout}"
    );
}

#[test]
fn test_clone_preserves_the_json_codec_both_ways() {
    // ADR-0282: with no --to-snapshot-format, both clones keep the source codec.
    assert_clone_both_ways(&[], "json", &[], "json");
}

/// ADR-0163: `--to-snapshot-format` overrides the ADR-0282 codec-preservation
/// default and transcodes the published checkpoint. Parquet-to-anything needs
/// the `parquet` feature (full bundle), so the case is gated on it.
#[cfg(feature = "parquet")]
#[test]
fn test_clone_to_snapshot_format_transcodes_both_ways() {
    assert_clone_both_ways(
        &["--to-snapshot-format", "parquet"],
        "parquet",
        &["--to-snapshot-format", "json"],
        "json",
    );
}

#[test]
fn test_clone_to_snapshot_format_usage_errors() {
    // ADR-0163: an unknown codec value and a missing value are usage errors
    // (exit 2), like the rest of the hand-rolled parser. A repeated flag is
    // last-wins, like every other CLI value flag.
    let home = tempdir();
    for (args, expected) in [
        (
            vec!["clone", "--to-snapshot-format", "xml", "./a", "./b"],
            "invalid --to-snapshot-format",
        ),
        (
            vec!["clone", "./a", "./b", "--to-snapshot-format"],
            "--to-snapshot-format requires a value",
        ),
    ] {
        let (status, _stdout, stderr) = run_cli(Opts::new(&args).in_home(home.path()));
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains(expected),
            "{args:?}: expected {expected:?}, got: {stderr}"
        );
    }
}
