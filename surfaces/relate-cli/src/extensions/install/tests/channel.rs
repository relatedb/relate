use super::*;

// --- the signed channel's post-gate steps (ADR-0431 §4 steps 2-4) ---

/// The committed channel-index fixture: the document the publisher's
/// expected output is skeleton-pinned against, whose two platform rows
/// carry the greet tarball's SHA256 — the signed row the install's
/// checksum verifies against.
const CHANNEL_EXTENSIONS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/extension/extensions.json"
));

/// The fixture signer's SSHSIG over [`CHANNEL_EXTENSIONS_JSON`] and the
/// signer's public half — the first-party chain in miniature (ADR-0431
/// §1): an SSHSIG over the exact index bytes in the release namespace,
/// by a key the verifier pins, just not *the* pinned release key (that
/// refusal is the e2e's — the ADR pins no key seam).
const CHANNEL_TEST_KEY_SIG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/extension/extensions-test-key.sig"
));
const CHANNEL_TEST_SIGNER_PUB: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/extension/ext-sign-test.pub"
));

/// The committed package tarball the index rows checksum — the bytes the
/// loopback route serves for whichever platform row the host resolves
/// (the fixture's two rows carry this one digest).
const GREET_TARBALL: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/extension/greet-1.2.3-darwin-arm64.tar.gz"
));

#[test]
fn a_first_party_style_signed_index_installs_the_package_behind_the_gate() {
    // The signature gate is the one step this driver cannot cross for
    // real — the pinned release key never verifies a fixture-key
    // signature and the ADR pins no key seam (the e2e pins that refusal
    // end to end). So the fixture signer's pair carries the same
    // verification shape here (SSHSIG over the exact index bytes,
    // namespace relate-release — the TEST_SIGNER pattern), and the
    // install runs the real steps behind the gate: resolve the host
    // platform's row, download through the route machinery, verify the
    // SHA256 against the signed row, unpack into staging.
    use crate::release_channel::signing::verify_with_key;

    verify_with_key(
        CHANNEL_EXTENSIONS_JSON.as_bytes(),
        CHANNEL_TEST_KEY_SIG.as_bytes(),
        CHANNEL_TEST_SIGNER_PUB,
    )
    .expect("the fixture signature verifies against the fixture signer");

    let (fixture, port) = relate_test_support::loopback::spawn_single_response_bytes_capturing(
        "200 OK",
        GREET_TARBALL.to_vec(),
    );
    let base = format!("http://127.0.0.1:{port}");
    let world = World::new();
    let staged = staging_dir(&world);

    let index = crate::extensions::channel::with_this_host(
        crate::extensions::channel::decode_index(CHANNEL_EXTENSIONS_JSON.as_bytes())
            .expect("the fixture index decodes"),
    );
    let (provenance, manifest) =
        install_from_index(&index, "greet", None, &base, &staged).expect("the install lands");

    // The adapter hands back the channel tag (§7's source reference; the
    // loud line renders it, pinned above) — the tier is the source's own
    // (`InstallSource::tier`), stamped by the install.
    assert_eq!(provenance.source, "relate-extensions-greet-1.2.3");
    // The staged package holds exactly the accounted set, the executable
    // with its exec bit from the member mode (§2).
    assert!(staged.join("extension.json").is_file());
    assert!(staged.join("greet.sh").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(staged.join("greet.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "the executable keeps its exec bit: {mode:o}"
        );
    }
    // The manifest's own version — the loud line reads it back.
    let Manifest::Command(command) = &manifest;
    assert_eq!(command.version, "1.2.3");

    // One request: the direct-route download of the host platform's
    // tarball, at the tag/asset path the route builds (§4 step 3 riding
    // ADR-0358's probe machinery).
    let heads = fixture.join();
    assert_eq!(heads.len(), 1, "{heads:?}");
    assert!(
        heads[0].starts_with(&format!(
            "GET /relate-extensions-greet-1.2.3/greet-1.2.3-{}-{}.tar.gz HTTP/1.1\r\n",
            host().os,
            host().arch
        )),
        "unexpected request: {:?}",
        heads[0]
    );
}

#[test]
fn a_tampered_tarball_refuses_at_the_signed_row_checksum() {
    // The signed row's SHA256 is the tarball's authentication (§4 step 3):
    // bytes that do not match refuse with the pinned checksum line before
    // anything unpacks, and nothing lands. (The member-contract refusals
    // of the same unpacker are the `--url` e2e's tampered-tarball test;
    // this is the channel's own gate, in front of the unpack.)
    let (fixture, port) = relate_test_support::loopback::spawn_single_response_bytes_capturing(
        "200 OK",
        b"tampered bytes".to_vec(),
    );
    let base = format!("http://127.0.0.1:{port}");
    let world = World::new();
    let staged = staging_dir(&world);

    let index = crate::extensions::channel::with_this_host(
        crate::extensions::channel::decode_index(CHANNEL_EXTENSIONS_JSON.as_bytes())
            .expect("the fixture index decodes"),
    );
    let err = install_from_index(&index, "greet", None, &base, &staged)
        .expect_err("a tampered tarball refuses at the signed row's checksum");
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert_eq!(
        err.stderr_message(),
        format!(
            "Error: extension install: checksum verification failed for greet-1.2.3-{}-{}.tar.gz \
             (expected af7ccfe3866c7dfeef5c75a438d586ce2490fdbb23c3c3ce288d5798610775ad, got {}); \
             not installing",
            host().os,
            host().arch,
            crate::release_channel::sha256_hex(b"tampered bytes")
        )
    );
    // Nothing unpacked into staging: the checksum fires before the unpack.
    assert!(std::fs::read_dir(&staged).unwrap().next().is_none());
    let heads = fixture.join();
    assert_eq!(heads.len(), 1, "{heads:?}");
}

// --- the walkthrough's own package through the signed channel (LABS-552) ---

/// The git-to-graph crate's committed package manifest — the first-party
/// package the walkthrough installs (LABS-552: `extension install
/// git-to-graph` installs it from the signed channel), included from its
/// live bytes so the driver tracks the crate, never a frozen fixture copy.
const GIT_TO_GRAPH_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../extensions/relate-git-to-graph/package/extension.json"
));

/// The pack stage's tarball for git-to-graph (ADR-0431 §2, the release
/// workflow's pack step in miniature): the manifest's own bytes plus the
/// executable member its `executable` names, the exec bit the contract
/// lands. The executable member is a stand-in — the install checks
/// member accounting and manifest shape and never spawns it; the
/// walkthrough's run leg (`tests/e2e_walkthrough.rs`) drives the real
/// child through the offline variant.
fn git_to_graph_artifact(manifest: &Manifest, executable_payload: &[u8]) -> Vec<u8> {
    let Manifest::Command(command) = manifest;
    let executable = command
        .executable
        .to_str()
        .expect("the manifest's executable is UTF-8");
    crate::extensions::testtar::tarball(&[
        ("extension.json", 0o644, GIT_TO_GRAPH_MANIFEST.as_bytes()),
        (executable, 0o755, executable_payload),
    ])
}

/// The channel index row the release pipeline derives from the crate's
/// manifest: the tag `relate-extensions-<name>-<X.Y.Z>` and the asset
/// `<name>-<X.Y.Z>-<os>-<arch>.tar.gz`, the host platform's row, with the
/// given artifact's SHA256 as the signed checksum the install verifies
/// against.
fn git_to_graph_index(manifest: &Manifest, sha256: &str) -> String {
    let Manifest::Command(command) = manifest;
    let host = host();
    format!(
        r#"{{"packages":[{{"name":"git-to-graph","versions":[{{"version":"{v}","protocol":"{p}","assets":[{{"os":"{os}","arch":"{arch}","tag":"relate-extensions-git-to-graph-{v}","asset":"git-to-graph-{v}-{os}-{arch}.tar.gz","sha256":"{sha256}"}}]}}]}}]}}"#,
        v = command.version,
        p = command.protocol,
        os = host.os,
        arch = host.arch,
        sha256 = sha256
    )
}

/// The walkthrough's channel variant behind the signature gate: the
/// channel e2e stops at the gate (the pinned release key never verifies a
/// fixture-key signature, and the ADR pins no key seam), so the real
/// steps behind it run here over the first-party package itself — the
/// index row the publisher would derive from the crate's manifest, the
/// tarball the pack stage packs, the route download, the signed-row
/// checksum, the unpack — the greet fixture above pins the machinery;
/// this pins the product flowing through it.
#[test]
fn the_walkthroughs_package_installs_from_the_first_party_channel_index() {
    // The verification shape, the fixture signer's chain (the TEST_SIGNER
    // pattern, the same shape the committed fixture carries above): an
    // SSHSIG over the exact index bytes in the release namespace, by a
    // key the verifier pins — the step the pinned-key entry point runs
    // over the real channel's index.
    use crate::release_channel::signing::verify_with_key;
    verify_with_key(
        CHANNEL_EXTENSIONS_JSON.as_bytes(),
        CHANNEL_TEST_KEY_SIG.as_bytes(),
        CHANNEL_TEST_SIGNER_PUB,
    )
    .expect("the fixture signature verifies against the fixture signer");

    let manifest: Manifest = serde_json::from_str(GIT_TO_GRAPH_MANIFEST)
        .expect("the crate's committed manifest decodes");
    let Manifest::Command(command) = &manifest;
    let artifact = git_to_graph_artifact(&manifest, b"#!/bin/sh\n");
    let index = crate::extensions::channel::decode_index(
        git_to_graph_index(&manifest, &crate::release_channel::sha256_hex(&artifact)).as_bytes(),
    )
    .expect("the walkthrough's index row decodes");

    let (fixture, port) =
        relate_test_support::loopback::spawn_single_response_bytes_capturing("200 OK", artifact);
    let base = format!("http://127.0.0.1:{port}");
    let world = World::new();
    let staged = staging_dir(&world);

    let (provenance, landed) = install_from_index(&index, "git-to-graph", None, &base, &staged)
        .expect("the walkthrough's channel install lands");

    // The channel tag is the source reference (§7); the tier is the
    // source's own, stamped by the install.
    assert_eq!(
        provenance.source,
        format!("relate-extensions-git-to-graph-{}", command.version)
    );
    // The staged package is the crate's own manifest plus the executable
    // member its `executable` names, the exec bit preserved (§2).
    assert_eq!(
        std::fs::read(staged.join("extension.json")).unwrap(),
        GIT_TO_GRAPH_MANIFEST.as_bytes(),
        "the crate's committed manifest must land byte for byte"
    );
    let Manifest::Command(landed_command) = &landed;
    assert_eq!(landed_command.name, "git-to-graph");
    assert_eq!(landed_command.version, command.version);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(staged.join(&landed_command.executable))
            .unwrap()
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "the executable keeps its exec bit: {mode:o}"
        );
    }

    // One request: the direct-route download of the host platform's
    // tarball, at the tag/asset path the route builds.
    let heads = fixture.join();
    assert_eq!(heads.len(), 1, "{heads:?}");
    let host = host();
    assert!(
        heads[0].starts_with(&format!(
            "GET /relate-extensions-git-to-graph-{v}/\
             git-to-graph-{v}-{os}-{arch}.tar.gz HTTP/1.1\r\n",
            v = command.version,
            os = host.os,
            arch = host.arch
        )),
        "unexpected request: {:?}",
        heads[0]
    );
}

/// The channel's two tamper gates over the walkthrough's own artifact
/// (§4 step 3's checksum, then the member accounting the unpack feeds):
/// bytes swapped after the index was signed, and a smuggled member
/// inside a tarball whose checksum the signed row carries. Each refuses
/// with its pinned text before any package lands (the staged-dir
/// cleanup on the error path is the flow's own contract, pinned by
/// `a_failed_install_leaves_no_residue_and_no_target` above).
#[test]
fn the_walkthroughs_tampered_channel_artifacts_refuse() {
    let manifest: Manifest = serde_json::from_str(GIT_TO_GRAPH_MANIFEST)
        .expect("the crate's committed manifest decodes");
    let Manifest::Command(command) = &manifest;
    let executable = command
        .executable
        .to_str()
        .expect("the manifest's executable is UTF-8");
    let host = host();

    // Bytes that do not match the signed row: the artifact tampered
    // after the index was signed.
    let good = git_to_graph_artifact(&manifest, b"#!/bin/sh\n");
    let good_sha = crate::release_channel::sha256_hex(&good);
    let index = crate::extensions::channel::decode_index(
        git_to_graph_index(&manifest, &good_sha).as_bytes(),
    )
    .unwrap();
    let (fixture, port) = relate_test_support::loopback::spawn_single_response_bytes_capturing(
        "200 OK",
        b"tampered bytes".to_vec(),
    );
    let base = format!("http://127.0.0.1:{port}");
    let world = World::new();
    let staged = staging_dir(&world);
    let err = install_from_index(&index, "git-to-graph", None, &base, &staged)
        .expect_err("swapped bytes refuse at the signed row's checksum");
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert_eq!(
        err.stderr_message(),
        format!(
            "Error: extension install: checksum verification failed for git-to-graph-{v}-{os}-{arch}.tar.gz \
             (expected {good_sha}, got {got}); not installing",
            v = command.version,
            os = host.os,
            arch = host.arch,
            got = crate::release_channel::sha256_hex(b"tampered bytes")
        )
    );
    // The checksum fires before the unpack: staging stays empty.
    assert!(std::fs::read_dir(&staged).unwrap().next().is_none());
    assert_eq!(fixture.join().len(), 1);

    // A checksum-matching smuggled member: the signed row authenticates
    // bytes, not member shapes — the member accounting (§2) refuses what
    // the manifest does not account for, naming the smuggled member.
    let smuggled = crate::extensions::testtar::tarball(&[
        ("extension.json", 0o644, GIT_TO_GRAPH_MANIFEST.as_bytes()),
        (executable, 0o755, b"#!/bin/sh\n".as_slice()),
        ("smuggled.txt", 0o644, b"stolen".as_slice()),
    ]);
    let index = crate::extensions::channel::decode_index(
        git_to_graph_index(&manifest, &crate::release_channel::sha256_hex(&smuggled)).as_bytes(),
    )
    .unwrap();
    let (_fixture, port) =
        relate_test_support::loopback::spawn_single_response_bytes_capturing("200 OK", smuggled);
    let base = format!("http://127.0.0.1:{port}");
    let world = World::new();
    let staged = staging_dir(&world);
    let err = install_from_index(&index, "git-to-graph", None, &base, &staged)
        .expect_err("a smuggled member refuses at the accounting");
    assert_eq!(err.exit_code(), 1, "{}", err.stderr_message());
    assert_eq!(
        err.stderr_message(),
        format!(
            "Error: extension install: the tarball's members (extension.json, {executable}, \
             smuggled.txt) are not the package the manifest accounts for (extension.json, \
             {executable})"
        )
    );
}
