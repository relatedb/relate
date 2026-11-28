//! The pin/trust-store tests (ADR-0449, REL8-2381's fix): the contract
//! stated once on `stage_and_land` — a `--github` install records its pin
//! **before** the package lands and rolls that entry back when the landing
//! fails, so the recorded pin and the package commit together or not at
//! all. Both failure directions are pinned here: a failed trust write
//! lands nothing, a failed landing leaves the store byte-identical.
//!
//! Fixtures (`World`, `install_github`, `tag_api`, the artifact builders,
//! the commit consts) are `tests/fixtures.rs`'s, beside the suites.

use super::fixtures::*;
use super::*;
use crate::extensions::source::Approval;
use crate::extensions::trust::TrustStore;
use relate_test_support::loopback::spawn_single_response_bytes_capturing;

/// A failed trust write lands nothing (the pin-before-land order,
/// REL8-2381's fix): the config dir turns read-only after a first
/// install, so the moved-pin reinstall's store write fails — and the
/// failure must refuse BEFORE the package lands. The pre-fix order wrote
/// the pin after the rename: the package landed, the command reported
/// failure, and the recorded pin was missing — a later reinstall took
/// the first-install path and silently approved a moved pin.
#[cfg(unix)]
#[test]
fn a_failed_trust_write_lands_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    let (api, api_port) = tag_api(COMMIT_A, &digest);
    let (download, download_port) =
        spawn_single_response_bytes_capturing("200 OK", artifact.clone());
    let world = World::new();
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    )
    .expect("the first pinned install lands");
    api.join();
    download.join();
    let target = world.extensions_dir().join("greet");
    let marker_before = std::fs::read_to_string(target.join(MARKER_FILE)).unwrap();

    // The config dir turns read-only: the store's staged write
    // (`extensions-trust.json.tmp` beside it) fails, while the extensions
    // dir below keeps its own write bits.
    std::fs::set_permissions(world.config_dir(), std::fs::Permissions::from_mode(0o555)).unwrap();
    // A moved pin, re-approved, with a fresh artifact: every step up to
    // the trust write succeeds, so the write is the one that fails.
    let moved = github_artifact_with(b"#!/bin/sh\n# moved\n");
    let moved_digest = crate::release_channel::sha256_hex(&moved);
    let (api2, api2_port) = tag_api(COMMIT_B, &moved_digest);
    let (download2, download2_port) =
        spawn_single_response_bytes_capturing("200 OK", moved.clone());
    let err = refusal(install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        Some(Approval::ReApprove),
        &base(api2_port),
        &base(download2_port),
    ));
    std::fs::set_permissions(world.config_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    // The store names its own file (F-1-8's write-side error contract).
    assert!(
        err.stderr_message()
            .contains("cannot write the extension trust store"),
        "{}",
        err.stderr_message()
    );
    // Nothing landed: the target's marker is byte-identical (still the
    // first install's commit), and no staging residue remains.
    assert_eq!(
        std::fs::read_to_string(target.join(MARKER_FILE)).unwrap(),
        marker_before
    );
    assert!(marker_before.contains(COMMIT_A), "{marker_before}");
    let residue: Vec<_> = std::fs::read_dir(world.extensions_dir())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| crate::staging::is_staging_name(&entry.file_name()))
        .collect();
    assert!(residue.is_empty(), "staging residue: {residue:?}");
    api2.join();
    download2.join();
}

/// The rename-failure arm of the pin contract (stated once on
/// `stage_and_land`): the staged directory sits inside the installed
/// target, so the replace's aside park takes it away before the landing
/// rename — the landing fails after the pin was recorded, and the
/// recorded pins must be byte-identical to before the attempt, with the
/// previous install restored. The other arm (a landing that refuses at
/// the park) is `a_failed_landing_restores_the_trust_store_to_its_prior_bytes`.
#[test]
fn a_failed_rename_restores_the_recorded_pins() {
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    let world = World::new();
    let (_heads, api_port) = tag_api(COMMIT_A, &digest);
    let (_heads, download_port) = spawn_single_response_bytes_capturing("200 OK", artifact.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    )
    .unwrap();
    let before = std::fs::read(world.trust_file()).unwrap();

    // The second attempt re-approves a moved pin — a store write before the
    // landing would be visible as changed bytes — and stages into a
    // directory INSIDE the installed target: the aside park moves the whole
    // target away, so the staged dir is missing when the landing renames
    // it. The source step (resolution, download, checksum, unpack, marker)
    // all succeed; only the landing fails, after the park.
    let moved = github_artifact_with(b"#!/bin/sh\n# rebuilt\n");
    let moved_digest = crate::release_channel::sha256_hex(&moved);
    let (_heads, api_port) = tag_api(COMMIT_B, &moved_digest);
    let (_heads, download_port) = spawn_single_response_bytes_capturing("200 OK", moved);
    let staged = world.extensions_dir().join("greet").join("stage");
    std::fs::create_dir_all(&staged).unwrap();
    let route = GithubRoute::at(&base(api_port), &base(download_port));
    let mut trust = TrustStore::load(&world.trust_file()).unwrap();
    let err = stage_and_land(
        &world.extensions_dir(),
        &|_| Ok(route.clone()),
        &install_args(
            "greet",
            InstallSource::Github(spec(GITHUB_TAG)),
            Some(Approval::ReApprove),
        ),
        &mut trust,
        &staged,
    )
    .expect_err("the landing fails: the aside park took the staged dir with it");
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert!(
        err.stderr_message().contains("landing the package"),
        "{}",
        err.stderr_message()
    );

    // The recorded pins are exactly the bytes from before the attempt, the
    // recorded pin still names the landed package, and that package is
    // back in place.
    assert_eq!(std::fs::read(world.trust_file()).unwrap(), before);
    assert_eq!(world.recorded_pin("greet").unwrap().commit, COMMIT_A);
    assert!(world
        .extensions_dir()
        .join("greet")
        .join("run.sh")
        .is_file());
}

/// A failed landing restores the trust store to its prior bytes (the
/// pin-before-land order, REL8-2381's fix): the target turns foreign
/// between the pre-fetch checks and the landing — the land-time
/// re-decision's own scenario — so the moved-pin reinstall records its
/// pin, refuses at the park, and must put the store back exactly as it
/// was. The pre-fix order never touched the store on a landing failure
/// but could leave a landed package without its pin; the new order's
/// failure direction is a recorded pin without a package, the safe one
/// (a later install compares against it and refuses a moved pin).
#[test]
fn a_failed_landing_restores_the_trust_store_to_its_prior_bytes() {
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    let (api, api_port) = tag_api(COMMIT_A, &digest);
    let (download, download_port) =
        spawn_single_response_bytes_capturing("200 OK", artifact.clone());
    let world = World::new();
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    )
    .expect("the first pinned install lands");
    api.join();
    download.join();

    // The target turns foreign at land time: the marker is gone, so the
    // land-time re-decision refuses where the pre-fetch snapshot had a
    // marked target of the same tier.
    std::fs::remove_file(world.extensions_dir().join("greet").join(MARKER_FILE)).unwrap();

    // The moved-pin reinstall drives stage_and_land directly (the
    // pre-fetch checks are the caller's; this unit's contract is the
    // land-time decision and the store's restoration).
    let moved = github_artifact_with(b"#!/bin/sh\n# moved\n");
    let moved_digest = crate::release_channel::sha256_hex(&moved);
    let (api2, api2_port) = tag_api(COMMIT_B, &moved_digest);
    let (download2, download2_port) =
        spawn_single_response_bytes_capturing("200 OK", moved.clone());
    let before = std::fs::read(world.trust_file()).unwrap();
    let mut trust = crate::extensions::trust::TrustStore::load(&world.trust_file()).unwrap();
    let staged = staging_dir(&world);
    let route = GithubRoute::at(&base(api2_port), &base(download2_port));
    let err = stage_and_land(
        &world.extensions_dir(),
        &|_| Ok(route.clone()),
        &install_args(
            "greet",
            InstallSource::Github(spec(GITHUB_TAG)),
            Some(Approval::ReApprove),
        ),
        &mut trust,
        &staged,
    )
    .expect_err("the foreign-at-land-time target refuses");
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert!(
        err.stderr_message().contains("refusing to touch"),
        "{}",
        err.stderr_message()
    );
    // The store's bytes are exactly what they were before the attempt:
    // the moved pin was recorded and rolled back.
    assert_eq!(std::fs::read(world.trust_file()).unwrap(), before);
    // The recorded pin reads through the fixture helper — one spelling.
    assert_eq!(
        world.recorded_pin("greet").map(|pin| pin.commit),
        Some(COMMIT_A.to_string())
    );
    // The parked original is back in place (markerless, as it was).
    assert!(world
        .extensions_dir()
        .join("greet")
        .join("run.sh")
        .is_file());
    api2.join();
    download2.join();
}

#[test]
fn a_malformed_trust_store_refuses_before_any_fetch() {
    let world = World::new();
    std::fs::create_dir_all(world.trust_file().parent().unwrap()).unwrap();
    std::fs::write(world.trust_file(), "{ not a store").unwrap();
    // An unreachable API base: reaching the network would fail with a
    // download error, so the trust-store text proves the store was read
    // first.
    let err = refusal(install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    ));
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert!(
        err.stderr_message()
            .contains("invalid extension trust store"),
        "{}",
        err.stderr_message()
    );
}
