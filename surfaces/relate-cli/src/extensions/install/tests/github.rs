//! The GitHub pin tier's tests (ADR-0449): the resolution over
//! GitHub-shaped loopback fixtures, the recorded pin in the marker and the
//! per-user trust store, the pre-fetch drift refusal, re-approval, the
//! remove + reinstall property, the commit pin's release scan, and the
//! pin comparison the flow decides on.
//!
//! Fixtures (`World`, `install_github`, `tag_api`, the artifact builders,
//! the commit consts) are `tests/fixtures.rs`'s, beside the suites.

use super::fixtures::*;
use super::*;
use crate::extensions::install::sources::compare_pin;
use crate::extensions::provenance::Pin;
use crate::extensions::source::{Approval, GithubPin, GithubSpec};
use relate_test_support::loopback::{
    spawn_capture_server, spawn_scripted_responses_capturing, spawn_single_response_bytes_capturing,
};

#[test]
fn a_github_install_resolves_the_pin_records_it_and_lands() {
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    let (api_heads, api_port) = tag_api(COMMIT_A, &digest);
    let (download_heads, download_port) =
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
    .expect("the pinned install lands");

    let expected = Pin {
        repo: "owner/repo".to_string(),
        tag: GITHUB_TAG.to_string(),
        commit: COMMIT_A.to_string(),
        asset_sha256: digest.clone(),
    };
    // The per-user trust store recorded the pin (the invocation was the
    // approval).
    assert_eq!(world.recorded_pin("greet"), Some(expected.clone()));
    // The provenance marker carries the same pin.
    let target = world.extensions_dir().join("greet");
    let marker = std::fs::read_to_string(target.join(MARKER_FILE)).unwrap();
    assert!(marker.contains("\"tier\":\"github-pin\""), "{marker}");
    assert!(
        marker.contains(&format!("\"commit\":\"{COMMIT_A}\"")),
        "{marker}"
    );
    assert!(
        marker.contains(&format!("\"assetSha256\":\"{digest}\"")),
        "{marker}"
    );
    assert!(target.join("extension.json").is_file());
    assert!(target.join("run.sh").is_file());
    assert_eq!(
        Provenance::read(&target).unwrap(),
        Some(Provenance {
            tier: Tier::GithubPin,
            source: format!("owner/repo@{GITHUB_TAG}"),
            pin: Some(expected),
        })
    );
    // The trust store is the config dir's file, not a hardcoded path.
    assert!(world.trust_file().is_file());

    // The API resolution: the commit lookup, then the release lookup.
    let api_heads = api_heads.join();
    assert_eq!(api_heads.len(), 2, "{api_heads:?}");
    assert!(
        api_heads[0].starts_with(&format!(
            "GET /repos/owner/repo/commits/{GITHUB_TAG} HTTP/1.1\r\n"
        )),
        "{api_heads:?}"
    );
    assert!(
        api_heads[1].starts_with(&format!(
            "GET /repos/owner/repo/releases/tags/{GITHUB_TAG} HTTP/1.1\r\n"
        )),
        "{api_heads:?}"
    );
    // One asset fetch, through the probe-then-fallback route's direct URL.
    let download_heads = download_heads.join();
    assert_eq!(download_heads.len(), 1, "{download_heads:?}");
    assert!(
        download_heads[0].starts_with(&format!(
            "GET /{GITHUB_TAG}/{} HTTP/1.1\r\n",
            github_asset()
        )),
        "{download_heads:?}"
    );
}

#[test]
fn a_same_pin_reinstall_succeeds() {
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    let world = World::new();
    for _ in 0..2 {
        let (api_heads, api_port) = tag_api(COMMIT_A, &digest);
        let (download_heads, download_port) =
            spawn_single_response_bytes_capturing("200 OK", artifact.clone());
        install_github(
            &world,
            "greet",
            spec(GITHUB_TAG),
            None,
            &base(api_port),
            &base(download_port),
        )
        .expect("the same pin reinstall proceeds without re-approval");
        assert_eq!(api_heads.join().len(), 2);
        assert_eq!(download_heads.join().len(), 1);
    }
    assert_eq!(world.recorded_pin("greet").unwrap().commit, COMMIT_A);
}

#[test]
fn a_moved_tag_refuses_before_the_asset_fetch_naming_both_pins() {
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    let world = World::new();
    let (api_heads, api_port) = tag_api(COMMIT_A, &digest);
    let (download_heads, download_port) =
        spawn_single_response_bytes_capturing("200 OK", artifact.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    )
    .unwrap();
    assert_eq!(api_heads.join().len(), 2);
    assert_eq!(download_heads.join().len(), 1);

    // The tag now resolves to another commit (and other bytes). The fetch
    // must not start: the download fixture is never asked.
    let moved = github_artifact();
    let moved_digest = crate::release_channel::sha256_hex(&moved);
    let (api_heads, api_port) = tag_api(COMMIT_B, &moved_digest);
    let (download_sink, download_port) = spawn_capture_server("200 OK", "unexpected asset bytes");
    let err = refusal(install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    ));
    assert_eq!(
        api_heads.join().len(),
        2,
        "the metadata resolution still runs"
    );
    assert_eq!(
        download_sink.request_count(),
        0,
        "the moved pin must refuse before the asset fetch"
    );
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    let text = err.stderr_message();
    assert!(
        text.contains(&format!("commit {COMMIT_A}"))
            && text.contains(&format!("commit {COMMIT_B}")),
        "the refusal names the recorded and the resolved pin: {text}"
    );
    assert!(text.contains("refusing before any fetch"), "{text}");
    assert!(
        text.contains("--re-approve") && text.contains("--approve"),
        "{text}"
    );
    // Nothing changed: the recorded pin and the installed package stay.
    assert_eq!(world.recorded_pin("greet").unwrap().commit, COMMIT_A);
    assert!(world
        .extensions_dir()
        .join("greet")
        .join("run.sh")
        .is_file());
}

#[test]
fn re_approving_a_moved_tag_updates_the_pin_after_verification() {
    let first = github_artifact();
    let first_digest = crate::release_channel::sha256_hex(&first);
    let world = World::new();
    let (_heads, api_port) = tag_api(COMMIT_A, &first_digest);
    let (_heads, download_port) = spawn_single_response_bytes_capturing("200 OK", first.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    )
    .unwrap();

    let moved = github_artifact();
    let moved_digest = crate::release_channel::sha256_hex(&moved);
    let (api_heads, api_port) = tag_api(COMMIT_B, &moved_digest);
    let (download_heads, download_port) =
        spawn_single_response_bytes_capturing("200 OK", moved.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        Some(Approval::ReApprove),
        &base(api_port),
        &base(download_port),
    )
    .expect("--re-approve accepts the moved pin");
    assert_eq!(api_heads.join().len(), 2);
    assert_eq!(download_heads.join().len(), 1);
    let recorded = world.recorded_pin("greet").unwrap();
    assert_eq!(recorded.commit, COMMIT_B);
    assert_eq!(recorded.asset_sha256, moved_digest);
    let marker =
        std::fs::read_to_string(world.extensions_dir().join("greet").join(MARKER_FILE)).unwrap();
    assert!(
        marker.contains(&format!("\"commit\":\"{COMMIT_B}\"")),
        "{marker}"
    );
}

#[test]
fn approve_accepts_only_the_commit_the_pin_resolves_to() {
    let first = github_artifact();
    let first_digest = crate::release_channel::sha256_hex(&first);
    let world = World::new();
    let (_heads, api_port) = tag_api(COMMIT_A, &first_digest);
    let (_heads, download_port) = spawn_single_response_bytes_capturing("200 OK", first.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    )
    .unwrap();

    let moved = github_artifact();
    let moved_digest = crate::release_channel::sha256_hex(&moved);
    // Approving the recorded commit while the pin resolves elsewhere
    // refuses, and the asset is never fetched.
    let (_heads, api_port) = tag_api(COMMIT_B, &moved_digest);
    let (download_sink, download_port) = spawn_capture_server("200 OK", "unexpected asset bytes");
    let err = refusal(install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        Some(Approval::Approve(COMMIT_A.to_string())),
        &base(api_port),
        &base(download_port),
    ));
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert!(
        err.stderr_message().contains(&format!(
            "resolves to commit {COMMIT_B}, not the approved commit {COMMIT_A}"
        )),
        "{}",
        err.stderr_message()
    );
    assert_eq!(download_sink.request_count(), 0);
    assert_eq!(world.recorded_pin("greet").unwrap().commit, COMMIT_A);

    // Approving the resolved commit accepts it.
    let (_heads, api_port) = tag_api(COMMIT_B, &moved_digest);
    let (_heads, download_port) = spawn_single_response_bytes_capturing("200 OK", moved.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        Some(Approval::Approve(COMMIT_B.to_string())),
        &base(api_port),
        &base(download_port),
    )
    .expect("--approve <resolved commit> re-approves");
    assert_eq!(world.recorded_pin("greet").unwrap().commit, COMMIT_B);
}

#[test]
fn changed_asset_bytes_at_the_same_commit_need_reapproval() {
    let first = github_artifact();
    let first_digest = crate::release_channel::sha256_hex(&first);
    let world = World::new();
    let (_heads, api_port) = tag_api(COMMIT_A, &first_digest);
    let (_heads, download_port) = spawn_single_response_bytes_capturing("200 OK", first.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    )
    .unwrap();

    // Same tag, same commit, different asset bytes: the digest moved.
    let changed = github_artifact_with(b"#!/bin/sh\n# rebuilt\n");
    let changed_digest = crate::release_channel::sha256_hex(&changed);
    let (api_heads, api_port) = tag_api(COMMIT_A, &changed_digest);
    let (download_sink, download_port) = spawn_capture_server("200 OK", "unexpected asset bytes");
    let err = refusal(install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    ));
    assert_eq!(api_heads.join().len(), 2);
    assert_eq!(download_sink.request_count(), 0);
    let text = err.stderr_message();
    assert!(
        text.contains(&first_digest) && text.contains(&changed_digest),
        "{text}"
    );
    assert_eq!(
        world.recorded_pin("greet").unwrap().asset_sha256,
        first_digest
    );

    // The explicit re-approval lands the new bytes and records them.
    let (_heads, api_port) = tag_api(COMMIT_A, &changed_digest);
    let (_heads, download_port) = spawn_single_response_bytes_capturing("200 OK", changed.clone());
    install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        Some(Approval::ReApprove),
        &base(api_port),
        &base(download_port),
    )
    .unwrap();
    assert_eq!(
        world.recorded_pin("greet").unwrap().asset_sha256,
        changed_digest
    );
}

#[test]
fn asset_bytes_that_do_not_match_the_resolved_digest_never_land() {
    let good = github_artifact();
    let digest = crate::release_channel::sha256_hex(&good);
    let (api_heads, api_port) = tag_api(COMMIT_A, &digest);
    let (download_heads, download_port) =
        spawn_single_response_bytes_capturing("200 OK", b"tampered bytes".to_vec());
    let world = World::new();
    let err = refusal(install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    ));
    assert_eq!(api_heads.join().len(), 2);
    assert_eq!(download_heads.join().len(), 1);
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert!(
        err.stderr_message().contains(&format!(
            "checksum verification failed for {} (expected {digest}, got {}); not installing",
            github_asset(),
            crate::release_channel::sha256_hex(b"tampered bytes")
        )),
        "{}",
        err.stderr_message()
    );
    // The failed first install records no pin and leaves no package.
    assert_eq!(world.recorded_pin("greet"), None);
    assert!(!world.extensions_dir().join("greet").exists());
}

#[test]
fn removing_the_package_does_not_forget_the_pin() {
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

    // `extension remove` deletes the package directory only: the trust
    // store keeps the pin, so a reinstall cannot start from a clean slate.
    std::fs::remove_dir_all(world.extensions_dir().join("greet")).unwrap();
    assert_eq!(world.recorded_pin("greet").unwrap().commit, COMMIT_A);

    let moved = github_artifact();
    let moved_digest = crate::release_channel::sha256_hex(&moved);
    let (_heads, api_port) = tag_api(COMMIT_B, &moved_digest);
    let (download_sink, download_port) = spawn_capture_server("200 OK", "unexpected asset bytes");
    let err = refusal(install_github(
        &world,
        "greet",
        spec(GITHUB_TAG),
        None,
        &base(api_port),
        &base(download_port),
    ));
    assert_eq!(download_sink.request_count(), 0);
    assert!(
        err.stderr_message().contains("refusing before any fetch"),
        "{}",
        err.stderr_message()
    );
    assert!(!world.extensions_dir().join("greet").exists());
}

#[test]
fn a_commit_pin_resolves_the_release_that_targets_it() {
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    // A commit pin scans the release listing, then resolves each candidate
    // tag's commit.
    let (api_heads, api_port) = spawn_scripted_responses_capturing(
        "200 OK",
        vec![
            format!("[{}]", release_doc(GITHUB_TAG, &digest)),
            commit_doc(COMMIT_A),
        ],
    );
    let (download_heads, download_port) =
        spawn_single_response_bytes_capturing("200 OK", artifact.clone());
    let world = World::new();
    install_github(
        &world,
        "greet",
        GithubSpec {
            repo: "owner/repo".to_string(),
            pin: GithubPin::Commit(COMMIT_A.to_string()),
        },
        None,
        &base(api_port),
        &base(download_port),
    )
    .expect("a commit pin installs the release that targets it");
    assert_eq!(download_heads.join().len(), 1);
    let api_heads = api_heads.join();
    assert_eq!(api_heads.len(), 2, "{api_heads:?}");
    assert!(
        api_heads[0].starts_with("GET /repos/owner/repo/releases?per_page=100 HTTP/1.1"),
        "{api_heads:?}"
    );
    assert!(
        api_heads[1].starts_with(&format!(
            "GET /repos/owner/repo/commits/{GITHUB_TAG} HTTP/1.1"
        )),
        "{api_heads:?}"
    );
    assert_eq!(
        world.recorded_pin("greet").unwrap(),
        Pin {
            repo: "owner/repo".to_string(),
            tag: GITHUB_TAG.to_string(),
            commit: COMMIT_A.to_string(),
            asset_sha256: digest,
        }
    );
}

#[test]
fn a_commit_targeted_by_two_releases_refuses_naming_both() {
    let artifact = github_artifact();
    let digest = crate::release_channel::sha256_hex(&artifact);
    let (api_heads, api_port) = spawn_scripted_responses_capturing(
        "200 OK",
        vec![
            format!(
                "[{},{}]",
                release_doc("v2.0.0", &digest),
                release_doc("v1.0.0", &digest)
            ),
            commit_doc(COMMIT_A),
            commit_doc(COMMIT_A),
        ],
    );
    let (download_sink, download_port) = spawn_capture_server("200 OK", "unexpected asset bytes");
    let world = World::new();
    let err = refusal(install_github(
        &world,
        "greet",
        GithubSpec {
            repo: "owner/repo".to_string(),
            pin: GithubPin::Commit(COMMIT_A.to_string()),
        },
        None,
        &base(api_port),
        &base(download_port),
    ));
    assert_eq!(api_heads.join().len(), 3);
    assert_eq!(download_sink.request_count(), 0);
    let text = err.stderr_message();
    assert!(text.contains("more than one release"), "{text}");
    assert!(text.contains("v2.0.0") && text.contains("v1.0.0"), "{text}");
}

#[test]
fn compare_pin_is_the_trust_decision() {
    let recorded = Pin {
        repo: "owner/repo".to_string(),
        tag: GITHUB_TAG.to_string(),
        commit: COMMIT_A.to_string(),
        asset_sha256: "a".repeat(64),
    };
    let same = recorded.clone();
    assert!(compare_pin("greet", None, &same, None).is_ok());
    assert!(compare_pin("greet", Some(&recorded), &same, None).is_ok());

    let moved = Pin {
        commit: COMMIT_B.to_string(),
        asset_sha256: "b".repeat(64),
        ..recorded.clone()
    };
    let err = compare_pin("greet", Some(&recorded), &moved, None).unwrap_err();
    assert!(err.stderr_message().contains(COMMIT_A) && err.stderr_message().contains(COMMIT_B));
    assert!(compare_pin("greet", Some(&recorded), &moved, Some(&Approval::ReApprove)).is_ok());
    assert!(compare_pin(
        "greet",
        Some(&recorded),
        &moved,
        Some(&Approval::Approve(COMMIT_B.to_string()))
    )
    .is_ok());
    assert!(compare_pin(
        "greet",
        Some(&recorded),
        &moved,
        Some(&Approval::Approve(COMMIT_A.to_string()))
    )
    .is_err());

    // Another repository is a moved pin too — the trust records where the
    // bytes came from, not only which commit.
    let elsewhere = Pin {
        repo: "other/repo".to_string(),
        ..same.clone()
    };
    assert!(compare_pin("greet", Some(&recorded), &elsewhere, None).is_err());
}

#[test]
fn the_github_loud_line_names_the_repo_the_tag_and_the_commit() {
    let pin = Pin {
        repo: "owner/repo".to_string(),
        tag: "v1.2.3".to_string(),
        commit: COMMIT_A.to_string(),
        asset_sha256: "a".repeat(64),
    };
    assert_eq!(
        loud_line(
            "greet",
            "1.2.3",
            &Provenance {
                tier: Tier::GithubPin,
                source: "owner/repo@v1.2.3".to_string(),
                pin: Some(pin),
            }
        ),
        format!(
            "installed github-pin extension greet 1.2.3 from owner/repo@v1.2.3 (commit {COMMIT_A})"
        )
    );
}
