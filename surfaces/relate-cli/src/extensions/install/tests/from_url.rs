use super::*;

// --- the `--from URL` flow (ADR-0431 §5's remote form) end to end, over
// a loopback route the plain-http loopback transport gate allows ---

/// The request paths a scripted loopback server saw, in arrival order.
fn request_paths(handle: std::thread::JoinHandle<Vec<String>>) -> Vec<String> {
    handle
        .join()
        .expect("the fixture server thread")
        .iter()
        .map(|head| {
            head.split_whitespace()
                .nth(1)
                .expect("the request line's path")
                .to_string()
        })
        .collect()
}

/// The `--from URL` install lands the accounted set — manifest plus
/// executable, nothing else — with the third-party marker, the fixed
/// executable mode, and the loud line naming the URL as the invoker gave
/// it. A URL whose last segment ends in `.json` is the manifest itself,
/// and the executable is fetched from the manifest's own folder.
#[test]
fn from_url_installs_the_accounted_set_with_the_marker_and_the_loud_line() {
    let world = World::new();
    let (server, port) = relate_test_support::loopback::spawn_scripted_responses(
        "200 OK",
        vec![manifest_text("greet", "1.2.3"), "#!/bin/sh\n".to_string()],
    );
    let url = format!("http://127.0.0.1:{port}/pkg/extension.bash.json");
    install_from_remote(&world, "greet", &url).unwrap();
    // The requests the install made: the manifest as named, then the
    // executable joined onto the manifest's folder.
    assert_eq!(
        request_paths(server),
        vec!["/pkg/extension.bash.json", "/pkg/run.sh"]
    );
    let target = world.extensions_dir().join("greet");
    // The installed dir holds exactly the accounted set plus the marker.
    assert!(target.join("extension.json").is_file());
    assert!(target.join("run.sh").is_file());
    assert!(target.join(MARKER_FILE).is_file());
    // The third-party marker: the tier the remote form lands at, the
    // source the URL as the invoker named it.
    let provenance = Provenance::read(&target).unwrap().expect("the marker");
    assert_eq!(provenance.tier, Tier::ThirdParty);
    assert_eq!(provenance.source, url);
    // No member-mode field exists on this tier: the executable lands with
    // the fixed 755 mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "{mode:o}");
    }
    // The loud line (§6's shape, the third-party tier's source
    // reference): the version read back from what landed, the URL as
    // named.
    assert_eq!(
        loud_line("greet", "1.2.3", &provenance),
        format!("installed third-party extension greet 1.2.3 from {url}")
    );
}

/// A URL that does not end in `.json` names a directory:
/// `extension.json` is appended to it, then the same behavior — the
/// alternative shape the ADR pins.
#[test]
fn from_url_appends_the_manifest_filename_to_a_directory_url() {
    let world = World::new();
    let (server, port) = relate_test_support::loopback::spawn_scripted_responses(
        "200 OK",
        vec![manifest_text("greet", "1.2.3"), "#!/bin/sh\n".to_string()],
    );
    // No trailing slash, no `.json` last segment: the manifest filename
    // is appended with the separator.
    install_from_remote(&world, "greet", &format!("http://127.0.0.1:{port}/pkg")).unwrap();
    assert_eq!(
        request_paths(server),
        vec!["/pkg/extension.json", "/pkg/run.sh"]
    );
    assert!(world
        .extensions_dir()
        .join("greet")
        .join("run.sh")
        .is_file());
}

/// The manifest's executable may name a relative subfolder: it is
/// fetched from the manifest's folder joined with the path, and staged
/// under the same subfolder — never above the manifest's folder.
#[test]
fn from_url_fetches_a_subfolder_executable_from_the_manifests_folder() {
    let world = World::new();
    let (server, port) = relate_test_support::loopback::spawn_scripted_responses(
        "200 OK",
        vec![
            manifest_with(
                "greet",
                "1.2.3",
                "bin/run.sh",
                relate_wire::mcp::MCP_PROTOCOL_VERSION,
            ),
            "#!/bin/sh\n".to_string(),
        ],
    );
    install_from_remote(&world, "greet", &format!("http://127.0.0.1:{port}/pkg/")).unwrap();
    assert_eq!(
        request_paths(server),
        vec!["/pkg/extension.json", "/pkg/bin/run.sh"]
    );
    let target = world.extensions_dir().join("greet");
    assert!(target.join("bin").join("run.sh").is_file());
}

/// The transport gate (§6's posture, the `--url` tier's one predicate):
/// a plain-http URL to a non-loopback host is the usage class, refused
/// with the `--from` wording before any fetch. (A non-http scheme never
/// reaches this gate — the grammar classifies it as a local directory
/// name, and the local tier's own read refusal answers for it.)
#[test]
fn from_url_refuses_a_non_https_url_with_the_authored_error() {
    let world = World::new();
    let url = "http://example.test/greet";
    let err = refusal(install_from_remote(&world, "greet", url));
    assert_eq!(err.exit_code(), 2, "{}", err.stderr_message());
    assert!(
        err.stderr_message()
            .contains("--from accepts only https URLs (or plain http to the loopback interface)"),
        "{}",
        err.stderr_message()
    );
    assert!(
        err.stderr_message().contains(url),
        "{}",
        err.stderr_message()
    );
}

/// The executable's manifest refusals — an empty path and an escaping one
/// — both fire after the manifest decode and *before* the executable is
/// fetched: the usage class, the pinned text, and exactly one request (the
/// manifest). One table, one scaffold, so the one-request postcondition
/// cannot be dropped from a copy.
#[test]
fn from_url_refuses_a_bad_executable_before_the_executable_fetch() {
    for (executable, needle) in [
        ("", "the manifest's executable path is empty"),
        (
            "../run.sh",
            "executable path '../run.sh' escapes the package directory",
        ),
    ] {
        let world = World::new();
        let (server, port) = relate_test_support::loopback::spawn_scripted_responses(
            "200 OK",
            vec![manifest_with(
                "greet",
                "1.2.3",
                executable,
                relate_wire::mcp::MCP_PROTOCOL_VERSION,
            )],
        );
        let err = refusal(install_from_remote(
            &world,
            "greet",
            &format!("http://127.0.0.1:{port}/pkg/extension.json"),
        ));
        assert_eq!(err.exit_code(), 2, "{executable}: {}", err.stderr_message());
        assert!(
            err.stderr_message().contains(needle),
            "{executable}: {}",
            err.stderr_message()
        );
        // Exactly one request: the manifest.
        assert_eq!(
            request_paths(server),
            vec!["/pkg/extension.json"],
            "{executable}"
        );
    }
}
