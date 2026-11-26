//! LABS-903 (ADR-0500): `--load SOURCE` — the source half of the two-axis
//! store grammar (`--database` names the target). The `:memory:` target
//! (the default) **turns** the source into the in-memory store: the engine
//! loads as usual and then releases the source (closed, unlocked), and the
//! CLI removes a remote download's scratch copy right after the load
//! instead of at exit. A path target creates the store there from the
//! source (the engine clone), then opens it normally.

use crate::support;

use support::{person_bundle, run_cli_in_home};

#[test]
fn a_load_turns_a_bundle_into_the_in_memory_store_and_releases_the_source() {
    let home = tempfile::tempdir().unwrap();
    let bundle = person_bundle(home.path());
    let bundle = bundle.to_str().unwrap();

    let (status, stdout, stderr) = run_cli_in_home(
        &[
            "-json",
            "--load",
            bundle,
            "-c",
            "MATCH (p:Person) RETURN p.name AS name",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("Alice"), "stdout: {stdout}");

    // The release is observable: a writable open of the same bundle works
    // the moment the loaded run exits — no lock is left behind.
    let (status, stdout, stderr) = run_cli_in_home(
        &[
            "--database",
            bundle,
            "-c",
            "CREATE (:Writer); MATCH (n) RETURN count(n) AS total",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("2"), "the writable open landed: {stdout}");
}

#[test]
fn a_memory_target_load_refuses_writes_like_a_read_only_one() {
    let home = tempfile::tempdir().unwrap();
    let bundle = person_bundle(home.path());
    let bundle = bundle.to_str().unwrap();

    let (status, _stdout, stderr) = run_cli_in_home(
        &["--load", bundle, "-c", "CREATE (n:Fresh {id: 1})"],
        home.path(),
    );
    // Exit 6 is the engine's read-only write refusal — the turn-into is
    // read-only by construction, exactly like a `--read-only` open.
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(
        stderr.contains("WriteOnReadOnlyAccessDatabase"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_load_cannot_follow() {
    let home = tempfile::tempdir().unwrap();
    let (status, _stdout, stderr) = run_cli_in_home(
        &["--load", "x.rl", "--read-only=live", "-c", "RETURN 1"],
        home.path(),
    );
    // Refused at parse time, whatever the target: a loaded source is
    // loaded once and released — there is no WAL left to follow.
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("--load cannot follow"), "stderr: {stderr}");
}

#[test]
fn a_durable_target_load_creates_the_store_and_opens_it_writable() {
    let home = tempfile::tempdir().unwrap();
    let bundle = person_bundle(home.path());
    let bundle = bundle.to_str().unwrap();
    let target = home.path().join("loaded.rl");

    let (status, stdout, stderr) = run_cli_in_home(
        &[
            "-json",
            "--database",
            target.to_str().unwrap(),
            "--load",
            bundle,
            "-c",
            "CREATE (:Writer); MATCH (n) RETURN count(n) AS total",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    // The load created the store at the caller's path and the run opened
    // it writable: the clone's data plus this run's write.
    assert!(stdout.contains("2"), "the write landed: {stdout}");
    assert!(
        stderr.contains(&format!(
            "--load {bundle}: loaded into {}",
            target.display()
        )),
        "the load is announced: {stderr}"
    );
    assert!(target.is_file(), "the target store exists");
}

#[test]
fn a_durable_target_load_refuses_an_existing_target() {
    let home = tempfile::tempdir().unwrap();
    let bundle = person_bundle(home.path());
    let bundle = bundle.to_str().unwrap();
    let target = home.path().join("there.rl");
    std::fs::write(&target, b"already here").unwrap();

    let (status, _stdout, stderr) = run_cli_in_home(
        &[
            "--database",
            target.to_str().unwrap(),
            "--load",
            bundle,
            "-c",
            "RETURN 1",
        ],
        home.path(),
    );
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("does not exist yet"), "stderr: {stderr}");
    // The existing file is untouched.
    assert_eq!(std::fs::read(&target).unwrap(), b"already here");
}

#[test]
fn a_read_only_sighting_cannot_govern_a_durable_target_load() {
    let home = tempfile::tempdir().unwrap();
    let bundle = person_bundle(home.path());
    let bundle = bundle.to_str().unwrap();

    let (status, _stdout, stderr) = run_cli_in_home(
        &[
            "--read-only",
            "--database",
            home.path().join("out.rl").to_str().unwrap(),
            "--load",
            bundle,
            "-c",
            "RETURN 1",
        ],
        home.path(),
    );
    // Refused at parse time: the load creates the target and opens it
    // writable; a read-only sighting cannot govern that. (With the
    // :memory: target a plain --read-only is a harmless restatement —
    // pinned by the write-refusal test above.)
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("--read-only cannot govern a --load"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_url_in_the_database_slot_is_refused_with_the_load_form() {
    let home = tempfile::tempdir().unwrap();
    // The target-axis refusal is one rule for both dispositions: a URL in
    // the target slot is refused identically with and without a --load,
    // before any network I/O (the loopback port would surface a fetch as
    // exit 1) and before the engine's clone could refuse it as a runtime
    // error.
    for load in [None, Some("x.rl")] {
        let mut args: Vec<&str> = vec!["--database", "https://example.com/store.rl"];
        if let Some(source) = load {
            args.extend(["--load", source]);
        }
        args.extend(["-c", "RETURN 1"]);
        let (status, _stdout, stderr) = run_cli_in_home(&args, home.path());
        assert_eq!(status.code(), Some(2), "{load:?}: {stderr}");
        assert!(
            stderr.contains("a remote store is a --load source"),
            "{load:?}: {stderr}"
        );
    }
}

/// The remote leg needs the fetch feature; the every-bundle lane pins the
/// fail-closed refusal without it (e2e_bundles).
#[cfg(feature = "remote-database")]
#[test]
fn a_remote_load_is_released_right_after_the_load() {
    use relate_test_support::loopback::spawn_single_response_bytes_capturing;
    use support::staged_path;

    let home = tempfile::tempdir().unwrap();
    let bundle = person_bundle(home.path());
    let bytes = std::fs::read(&bundle).expect("read the cloned bundle");
    let (_fixture, port) = spawn_single_response_bytes_capturing("200 OK", bytes);

    let (status, stdout, stderr) = run_cli_in_home(
        &[
            "-json",
            "--load",
            &format!("http://127.0.0.1:{port}/store.rl"),
            "-c",
            "MATCH (p:Person) RETURN p.name AS name",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("Alice"), "stdout: {stdout}");

    // The announcement names the copy, and the copy is already gone: the
    // load released the source, so the scratch is removed right after it.
    let staged = staged_path(&stderr);
    assert!(
        !staged.exists(),
        "released after the load: {}",
        staged.display()
    );
}
