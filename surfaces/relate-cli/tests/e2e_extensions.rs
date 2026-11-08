//! End-to-end tests for the `extension` lifecycle commands (ADR-0431):
//! `relate extension install <name> [--url URL] [--from DIR]
//! [--version V] [--force]`, `relate extension list [--available]`,
//! `relate extension remove <name>`.
//!
//! The argument-grammar tests never open a socket. The lifecycle runs
//! offline through the `--from` fixture (the CI story, ADR-0431 §5);
//! the `--url` tier's transport gate is pinned here too — it accepts
//! only https URLs, so the loopback-served tarball the happy path once
//! fetched is now the no-request refusal (the fetch itself is
//! unit-pinned over loopback in `extensions/install/tests.rs`; the
//! real-binary tarball lifecycle runs through `--from`). The channel
//! tests (`--available`, the default install) stop at the signature
//! gate: the pinned release key never verifies a fixture-key signature,
//! so the happy channel path is pinned by unit tests — the decoded
//! index and the loopback-driven post-gate steps in
//! `extensions/install/tests.rs` — and by the signature failure here
//! (the ADR's stated plan — pure-function verification with fixture
//! keys, ADR-0431's consequences).
//!
//! `run <name>` — the walkthrough's third step — is the run mount's
//! surface (ADR-0429, LABS-552) and lands in its own slice; this suite
//! pins the install → list → remove lifecycle around it.

#![cfg(feature = "extensions")]

mod support;

use std::path::Path;

use relate_test_support::loopback::{
    spawn_capture_server, spawn_scripted_responses_capturing, spawn_single_response_bytes_capturing,
};
use support::extensions::{
    extensions_dir, install_package, install_sh_child, pack_extension_tarball,
};
use support::{run_cli, tempdir, CliRunOptions as Opts};

/// The channel index fixture (the committed `extensions.json`): one
/// visible package (`greet` 1.2.3, protocol 1, both platform rows).
const EXTENSIONS_JSON: &str = include_str!("fixtures/extension/extensions.json");

/// A valid SSHSIG over [`EXTENSIONS_JSON`] made by a fixture key — NOT the
/// pinned release key, so every channel e2e that serves it stops at
/// verification (the signature-failure refusal), deterministically.
const TEST_KEY_SIG: &str = include_str!("fixtures/extension/extensions-test-key.sig");

/// The committed package tarball: `extension.json` (name `greet`, version
/// 1.2.3, protocol 1, one string flag, a positional, per-batch commit) and
/// the `greet.sh` executable — `tar(1)` output in the closed contract's
/// shape.
const GREET_TARBALL: &[u8] = include_bytes!("fixtures/extension/greet-1.2.3-darwin-arm64.tar.gz");

/// An isolated world: `$XDG_CONFIG_HOME` under a tempdir, so every run
/// reads and writes this test's extensions dir alone. The install
/// surface is the shared kit's (`support::extensions`): the package
/// dir, its manifest, and its `sh` child are written by the one kit
/// every extension e2e uses, so this suite's installs cannot drift
/// from the run-verb and fixture-language suites'.
struct World {
    home: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        Self { home: tempdir() }
    }

    /// The extensions dir under this world's config dir — the kit's one
    /// join, so the suite's assertions read the same path the installs
    /// wrote.
    fn extensions_dir(&self) -> std::path::PathBuf {
        extensions_dir(self.home.path())
    }

    /// The run options with this world's `$XDG_CONFIG_HOME` (the harness
    /// scrubs it by default; an explicit set wins): the pair lands in the
    /// options' owned env Vec, its values borrowing this world — the
    /// single lifetime covers the world and the argument words the caller
    /// passes beside it.
    fn opts<'s>(&'s self, args: &'s [&'s str]) -> Opts<'s> {
        let xdg = self
            .home
            .path()
            .to_str()
            .expect("the world's path is UTF-8");
        let mut opts = Opts::new(args);
        opts.env.push(("XDG_CONFIG_HOME", xdg));
        opts
    }

    /// Install a package directory (manifest + executable) under this
    /// world's extensions dir, optionally with a provenance marker
    /// (`tier`, `source`) — the package dir and manifest from the kit's
    /// `install_package`, the no-op `sh` child from its `install_sh_child`
    /// (nothing in this suite spawns the child; the lifecycle verbs
    /// never run it).
    fn install_fixture(&self, dir_name: &str, manifest: &str, provenance: Option<(&str, &str)>) {
        let dir = install_package(self.home.path(), dir_name, manifest);
        install_sh_child(&dir, "");
        if let Some((tier, source)) = provenance {
            std::fs::write(
                dir.join(".relate-provenance.json"),
                format!("{{\"tier\":\"{tier}\",\"source\":{source}}}\n"),
            )
            .unwrap();
        }
    }

    /// A `--from` source package directory outside the extensions dir —
    /// the manifest is this suite's own fixture fact (its name and
    /// version vary per refusal), the `sh` child the kit's one writer.
    fn source_package(&self, name: &str, version: &str) -> String {
        let dir = self.home.path().join("src").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("extension.json"),
            format!(
                r#"{{"kind":"command","name":"{name}","version":"{version}","protocol":"2025-06-18","flags":[],"commit":"per-batch","executable":"run.sh"}}"#
            ),
        )
        .unwrap();
        install_sh_child(&dir, "");
        dir.display().to_string()
    }
}

fn run(world: &World, args: &[&str]) -> (std::process::ExitStatus, String, String) {
    run_cli(world.opts(args))
}

// --- the positional grammar (ADR-0431 §3) ---

#[test]
fn test_extension_usage_errors() {
    let world = World::new();
    // A missing action lists the closed vocabulary.
    let (status, _, stderr) = run(&world, &["extension"]);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("extension requires an action (init, install, list, remove or check)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Run 'relate extension --help' for more information."),
        "{stderr}"
    );
    // An unknown action is the unexpected-argument shape.
    let (status, _, stderr) = run(&world, &["extension", "upgrade"]);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("unexpected argument to extension: upgrade"),
        "{stderr}"
    );
}

#[cfg(feature = "extension-install")]
#[test]
fn test_extension_install_usage_errors() {
    let world = World::new();
    for (args, needle) in [
        (
            &["extension", "install"][..],
            "extension install requires a package name",
        ),
        (
            &["extension", "install", "a", "b"],
            "unexpected argument to extension install: b",
        ),
        (
            &["extension", "install", "greet", "--url", "u", "--from", "d"],
            "--url and --from are mutually exclusive (they both name the install source)",
        ),
        (
            &[
                "extension",
                "install",
                "greet",
                "--url",
                "u",
                "--version",
                "1",
            ],
            "--version is not valid with --url, --from or --github (the named source \
             already determines the version)",
        ),
    ] {
        let (status, _, stderr) = run(&world, args);
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
        // The usage error prints the command's per-build synopsis.
        assert!(
            stderr.contains("relate extension install <name>"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn test_extension_remove_has_no_force() {
    let world = World::new();
    let (status, _, stderr) = run(&world, &["extension", "remove", "greet", "--force"]);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("unexpected argument to extension remove: --force"),
        "{stderr}"
    );
}

/// A database named `extension` stays reachable (the COMMAND_WORDS
/// reservation, ADR-0104): `--database extension` opens a store.
#[test]
fn test_database_flag_accepts_extension_as_store_path() {
    let world = World::new();
    let dir = tempdir();
    let xdg = world.home.path().to_str().unwrap();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "extension", "RETURN 1 AS n"])
            .in_dir(dir.path())
            .with_envs(&[("XDG_CONFIG_HOME", xdg)]),
    );
    assert!(status.success(), "{stderr}");
    assert!(stdout.contains('1'), "{stdout}");
    assert!(dir.path().join("extension").is_dir());
}

// --- the offline lifecycle: install --from → list → remove (ADR-0431 §5) ---

#[cfg(feature = "extension-install")]
#[test]
fn test_install_from_dir_list_remove_lifecycle() {
    let world = World::new();
    let source = world.source_package("greet", "1.2.3");

    // install: the loud line names the package, its version, and the source.
    let (status, stdout, stderr) = run(
        &world,
        &["extension", "install", "greet", "--from", &source],
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        stdout.trim(),
        format!("installed local extension greet 1.2.3 from {source}")
    );
    let target = world.extensions_dir().join("greet");
    assert!(target.join("extension.json").is_file());
    assert!(target.join("run.sh").is_file());
    assert!(target.join(".relate-provenance.json").is_file());
    // The source directory is left untouched.
    assert!(Path::new(&source).join("run.sh").is_file());

    // list: one row — name, version, tier, source.
    let (status, stdout, stderr) = run(&world, &["extension", "list"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(stdout.trim(), format!("greet 1.2.3 local {source}"));

    // remove: the marker permits it; the package directory goes.
    let (status, stdout, stderr) = run(&world, &["extension", "remove", "greet"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("removed extension greet"), "{stdout}");
    assert!(!target.exists());
    // The empty installed set prints nothing, exit 0.
    let (status, stdout, stderr) = run(&world, &["extension", "list"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    // Removing again names the missing package.
    let (status, _, stderr) = run(&world, &["extension", "remove", "greet"]);
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("no installed extension 'greet'"),
        "{stderr}"
    );
}

/// A package whose manifest carries hostile output fragments — a name
/// with an embedded newline, a version with an escape — installs and
/// lists sanitized: the loud line and every list row pass their
/// package-authored fragments through the shared sanitizer, so control
/// characters can never forge a second line or ANSI into the verb's
/// output (the `print_error` doctrine, ADR-0430 §5, applied to the
/// lifecycle verbs' own lines).
#[cfg(feature = "extension-install")]
#[test]
fn test_hostile_manifest_fragments_render_sanitized() {
    let world = World::new();
    // The source directory's own name is clean; the manifest's name (the
    // mount name, which the install's positional must agree with) and
    // version carry the control characters, JSON-escaped so the strict
    // decode accepts them: a real newline and a real ESC.
    let dir = world.home.path().join("src").join("hostile");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        r#"{"kind":"command","name":"gr\neet","version":"1.2\u001b","protocol":"2025-06-18","flags":[],"commit":"per-batch","executable":"run.sh"}"#,
    )
    .unwrap();
    install_sh_child(&dir, "");
    let source = dir.display().to_string();

    let (status, stdout, stderr) = run(
        &world,
        &["extension", "install", "gr\neet", "--from", &source],
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    // The newline renders as a space, the escape is dropped — one line.
    assert_eq!(
        stdout.trim(),
        format!("installed local extension gr eet 1.2 from {source}")
    );

    let (status, stdout, stderr) = run(&world, &["extension", "list"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(stdout.trim(), format!("gr eet 1.2 local {source}"));
}

#[cfg(feature = "extension-install")]
#[test]
fn test_install_from_dir_replaces_the_same_tier_on_a_version_change() {
    let world = World::new();
    let v1 = world.source_package("greet", "1.0.0");
    let (status, _, stderr) = run(&world, &["extension", "install", "greet", "--from", &v1]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    let v2 = world.source_package("greet", "2.0.0");
    let (status, stdout, stderr) = run(&world, &["extension", "install", "greet", "--from", &v2]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("installed local extension greet 2.0.0"),
        "{stdout}"
    );
    let (status, stdout, _) = run(&world, &["extension", "list"]);
    assert_eq!(status.code(), Some(0));
    assert!(stdout.contains("greet 2.0.0 local"), "{stdout}");
}

#[cfg(feature = "extension-install")]
#[test]
fn test_install_from_dir_refusals() {
    let world = World::new();
    // A reserved-word package name: exit 2, the registry's pinned text.
    let source = world.source_package("serve", "1.0.0");
    let (status, _, stderr) = run(
        &world,
        &["extension", "install", "serve", "--from", &source],
    );
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("extension name 'serve' collides with the reserved command word 'serve'"),
        "{stderr}"
    );
    // A name that is not a single path segment: exit 2.
    let (status, _, stderr) = run(
        &world,
        &["extension", "install", "../evil", "--from", &source],
    );
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("'../evil' is not a valid package name"),
        "{stderr}"
    );
    // A manifest mismatch: the request and the package's own name.
    let source = world.source_package("greet", "1.0.0");
    let (status, _, stderr) = run(
        &world,
        &["extension", "install", "other", "--from", &source],
    );
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("the package's manifest names 'greet' but the install was for 'other'"),
        "{stderr}"
    );
    // Nothing landed from any refused install.
    assert!(!world.extensions_dir().join("greet").exists());
    assert!(!world.extensions_dir().join("other").exists());
    // A foreign target directory — a hand-copied, unmarked package: the
    // registry loads it (it is well-formed), the marker is absent, and
    // the install refuses without --force, clobbers with it (the skill
    // install refusal, ADR-0261; ADR-0431 §3).
    world.install_fixture(
        "greet",
        r#"{"kind":"command","name":"greet","version":"0.0.1","protocol":"2025-06-18","flags":[],"commit":"per-batch","executable":"run.sh"}"#,
        None,
    );
    let (status, _, stderr) = run(
        &world,
        &["extension", "install", "greet", "--from", &source],
    );
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("refusing to touch") && stderr.contains("pass --force to override"),
        "{stderr}"
    );
    let (status, _, stderr) = run(
        &world,
        &[
            "extension",
            "install",
            "greet",
            "--from",
            &source,
            "--force",
        ],
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(world
        .extensions_dir()
        .join("greet")
        .join("run.sh")
        .is_file());
}

#[cfg(feature = "extension-install")]
#[test]
fn test_a_cross_tier_reinstall_refuses_naming_both_sides() {
    // §7: a first-party package can never be replaced by a third-party
    // one, or the reverse, without an explicit remove — realized over the
    // local tier here (a --url install over a marked --from install).
    let world = World::new();
    let source = world.source_package("greet", "1.0.0");
    let (status, _, stderr) = run(
        &world,
        &["extension", "install", "greet", "--from", &source],
    );
    assert_eq!(status.code(), Some(0), "{stderr}");

    let (fixture, port) = spawn_single_response_bytes_capturing("200 OK", GREET_TARBALL.to_vec());
    // https past the live server: the cross-tier refusal must fire
    // before the fetch, so the URL's scheme never gets a chance to be
    // the refusal this test pins.
    let url = format!("https://127.0.0.1:{port}/greet-1.2.3.tar.gz");
    let (status, stdout, stderr) = run(&world, &["extension", "install", "greet", "--url", &url]);
    assert_eq!(status.code(), Some(1), "{stdout}\n{stderr}");
    assert_eq!(
        stderr.trim(),
        format!(
            "Error: extension install: extension 'greet' is installed local (source: {source}); \
             refusing a third-party install over it — remove it first: \
             relate extension remove greet"
        )
    );
    // No request: the refusal fires before the fetch (§7 — before
    // anything is unpacked into place).
    assert_eq!(fixture.join().len(), 0);
    // The same tier still replaces cleanly (§3).
    let source = world.source_package("greet", "2.0.0");
    let (status, _, stderr) = run(
        &world,
        &["extension", "install", "greet", "--from", &source],
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
}

#[cfg(feature = "extension-install")]
#[test]
fn test_install_refuses_when_no_config_dir_resolves() {
    // A relative `$XDG_CONFIG_HOME` is treated as unset (the shared
    // config-dir rule): with `$HOME` gone too, no base resolves and the
    // install refuses with its authored text — never an extensions dir
    // resolved against the current working directory (the drive-by
    // guard, ADR-0429 §6). The harness scrubs every config-dir base by default; the
    // explicit relative set wins over the scrub, and HOME is removed.
    let world = World::new();
    let source = world.source_package("greet", "1.2.3");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["extension", "install", "greet", "--from", &source])
            .with_envs(&[("XDG_CONFIG_HOME", "relconf")])
            .without_envs(&["HOME"]),
    );
    assert_eq!(status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr.trim(),
        "Error: extension install: no config directory resolves: set $XDG_CONFIG_HOME, $HOME, $APPDATA, or $USERPROFILE"
    );
    assert!(!world.extensions_dir().join("greet").exists());
}

// --- the copied-in package: runs, lists unmarked, refuses removal (§5) ---

#[test]
fn test_a_copied_in_package_lists_unmarked_and_refuses_removal() {
    let world = World::new();
    // An image can COPY a package directory in (ADR-0431 §5): it runs
    // (discovery loads it), lists honestly, and `remove` refuses it — the
    // marker is required (§3).
    world.install_fixture(
        "hand",
        r#"{"kind":"command","name":"hand","version":"3.1.4","protocol":"2025-06-18","flags":[],"commit":"per-batch","executable":"run.sh"}"#,
        None,
    );
    let (status, stdout, stderr) = run(&world, &["extension", "list"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(stdout.trim(), "hand 3.1.4 unmarked");
    let (status, _, stderr) = run(&world, &["extension", "remove", "hand"]);
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("refusing to remove") && stderr.contains("no .relate-provenance.json"),
        "{stderr}"
    );
}

#[test]
fn test_a_broken_installed_set_refuses_list_and_remove() {
    // A hand-broken manifest (the registry's fail-closed discovery,
    // ADR-0429 §6) refuses the listing naming the offender — never a
    // partial listing.
    let world = World::new();
    world.install_fixture("broken", "{ nope", None);
    let (status, stdout, stderr) = run(&world, &["extension", "list"]);
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("extension 'broken': invalid manifest"),
        "{stderr}"
    );
}

// --- the third-party URL tier (ADR-0431 §6): the transport gate ---

#[cfg(feature = "extension-install")]
#[test]
fn test_install_from_url_refuses_a_non_https_url() {
    // The tier's transport gate (§6): no signature covers a third-party
    // tarball, so https is the act's one leg of integrity — a non-https,
    // non-loopback URL refuses with the authored usage error before any
    // fetch (the live server never sees a request), and nothing lands.
    // A plain-http loopback URL is the one carve-out (the local fixture
    // registry shape, pinned by the loopback lane in e2e_run.rs).
    let (fixture, port) = spawn_single_response_bytes_capturing("200 OK", GREET_TARBALL.to_vec());
    let url = format!("http://example.com:{port}/greet-1.2.3.tar.gz");
    let world = World::new();
    let (status, stdout, stderr) = run(&world, &["extension", "install", "greet", "--url", &url]);
    assert_eq!(status.code(), Some(2), "{stdout}\n{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr.trim(),
        format!("Error: extension install: --url accepts only https URLs (or plain http to the loopback interface); '{url}' is neither")
    );
    assert!(
        fixture.join().is_empty(),
        "no request: the gate fires before the fetch"
    );
    assert!(!world.extensions_dir().join("greet").exists());
}

#[cfg(feature = "extension-install")]
#[test]
fn test_install_from_url_fails_closed_when_the_download_fails() {
    // An https URL nothing serves (loopback port 1, the harness's
    // refused-endpoint port): the fetch fails closed naming the URL —
    // the tier's own wrap, end to end through the real binary.
    let world = World::new();
    let (status, _, stderr) = run(
        &world,
        &[
            "extension",
            "install",
            "greet",
            "--url",
            "https://127.0.0.1:1/gone.tar.gz",
        ],
    );
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "could not download the package tarball from https://127.0.0.1:1/gone.tar.gz"
        ),
        "{stderr}"
    );
    assert!(!world.extensions_dir().join("greet").exists());
}

// --- the channel: the signature gate and the offline story (§4, §9) ---

#[cfg(feature = "extension-install")]
#[test]
fn test_list_available_fails_closed_when_the_index_is_missing() {
    let (fixture, port) = spawn_scripted_responses_capturing("404 Not Found", vec![String::new()]);
    let url = format!("http://127.0.0.1:{port}/extensions.json");
    let world = World::new();
    let (status, stdout, stderr) = run_cli(
        world
            .opts(&["extension", "list", "--available"])
            .with_envs(&[("RELATE_EXTENSIONS_JSON_URL", url.as_str())]),
    );
    // Offline/missing: exit 1, naming the index URL and the network cause
    // — never an empty list, never a silent fallback (§9).
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr.trim(),
        format!(
            "Error: extension list: could not read the extension channel index from {url}: \
             GET {url}: http status: 404"
        )
    );
    let heads = fixture.join();
    assert_eq!(heads.len(), 1, "only the index request: {heads:?}");
}

#[cfg(feature = "extension-install")]
#[test]
fn test_list_available_fails_closed_on_a_signature_that_does_not_verify() {
    // The fixture-key signature never verifies against the pinned release
    // key: the channel is not trusted, nothing is downloaded, and the
    // refusal names the artifact (§4 step 1 — fail closed, no
    // signature-optional path).
    let (fixture, port) = spawn_scripted_responses_capturing(
        "200 OK",
        vec![EXTENSIONS_JSON.to_string(), TEST_KEY_SIG.to_string()],
    );
    let url = format!("http://127.0.0.1:{port}/extensions.json");
    let world = World::new();
    let (status, stdout, stderr) = run_cli(
        world
            .opts(&["extension", "list", "--available"])
            .with_envs(&[("RELATE_EXTENSIONS_JSON_URL", url.as_str())]),
    );
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("extensions.json.sig"), "{stderr}");
    assert!(
        stderr.contains("failed verification against the pinned release key"),
        "{stderr}"
    );
    let heads = fixture.join();
    assert_eq!(
        heads.len(),
        2,
        "the index and its signature only: {heads:?}"
    );
}

#[cfg(feature = "extension-install")]
#[test]
fn test_channel_install_stops_at_the_signature_gate() {
    // The default install (no --url, no --from) fetches the index and its
    // signature; a signature that does not verify refuses before any
    // tarball request (§4 step 1 fires before step 3's download).
    let (fixture, port) = spawn_scripted_responses_capturing(
        "200 OK",
        vec![EXTENSIONS_JSON.to_string(), TEST_KEY_SIG.to_string()],
    );
    let url = format!("http://127.0.0.1:{port}/extensions.json");
    let world = World::new();
    let (status, _, stderr) = run_cli(world.opts(&["extension", "install", "greet"]).with_envs(&[
        ("RELATE_EXTENSIONS_JSON_URL", url.as_str()),
        (
            "RELATE_RELEASE_DOWNLOAD_URL",
            format!("http://127.0.0.1:{port}").as_str(),
        ),
    ]));
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("failed verification against the pinned release key"),
        "{stderr}"
    );
    let heads = fixture.join();
    assert_eq!(
        heads.len(),
        2,
        "the index and its signature only: {heads:?}"
    );
}

/// `extension list` (no flag) never fetches (ADR-0431 §4: the network is
/// touched only by the install verb and `--available`): an empty listing
/// with a bogus index URL still succeeds offline.
#[test]
fn test_extension_list_never_fetches() {
    let world = World::new();
    let (status, stdout, stderr) = run_cli(world.opts(&["extension", "list"]).with_envs(&[(
        "RELATE_EXTENSIONS_JSON_URL",
        "http://127.0.0.1:1/extensions.json",
    )]));
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}

// --- the GitHub pin tier (ADR-0449) ---

const GITHUB_COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const GITHUB_COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const GITHUB_TAG: &str = "v1.2.3";

/// The host platform in the channel's vocabulary (the package asset name
/// carries it).
fn github_platform() -> (&'static str, &'static str) {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => ("darwin", "amd64"),
        ("macos", "aarch64") => ("darwin", "arm64"),
        ("linux", "x86_64") => ("linux", "amd64"),
        ("linux", "aarch64") => ("linux", "arm64"),
        other => panic!("no channel platform for {other:?}"),
    }
}

fn github_asset() -> String {
    let (os, arch) = github_platform();
    format!("greet-1.2.3-{os}-{arch}.tar.gz")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The greet package artifact an e2e GitHub release serves, and its
/// digest.
fn github_package(world: &World) -> (Vec<u8>, String) {
    let dir = world.source_package("greet", "1.2.3");
    let dir = Path::new(&dir);
    let manifest = std::fs::read(dir.join("extension.json")).unwrap();
    let artifact = pack_extension_tarball(&manifest, &dir.join("run.sh"));
    let digest = sha256_hex(&artifact);
    (artifact, digest)
}

fn github_commit_doc(sha: &str) -> String {
    format!(r#"{{"sha":"{sha}","commit":{{"message":"release"}}}}"#)
}

fn github_release_doc(digest: &str) -> String {
    format!(
        r#"{{"tag_name":"{GITHUB_TAG}","name":"{GITHUB_TAG}","assets":[{{"id":7,"name":"{}","digest":"sha256:{digest}"}}]}}"#,
        github_asset()
    )
}

/// One scripted API fixture: the commit lookup, then the release lookup.
fn github_api(
    commit: &str,
    digest: &str,
) -> (relate_test_support::loopback::CapturingFixture, u16) {
    spawn_scripted_responses_capturing(
        "200 OK",
        vec![github_commit_doc(commit), github_release_doc(digest)],
    )
}

#[cfg(feature = "extension-install")]
#[test]
fn test_github_install_grammar_refusals() {
    let world = World::new();
    for (case, args, needle) in [
        (
            "untagged",
            &["extension", "install", "greet", "--github", "owner/repo"][..],
            "an untagged repository is a moving target",
        ),
        (
            "bad slug",
            &["extension", "install", "greet", "--github", "owner@v1"],
            "is not an owner/repo repository slug",
        ),
        (
            "re-approve without github",
            &[
                "extension",
                "install",
                "greet",
                "--from",
                "d",
                "--re-approve",
            ],
            "--re-approve and --approve <commit-sha> only apply to --github installs",
        ),
        (
            "short approve",
            &[
                "extension",
                "install",
                "greet",
                "--github",
                "o/r@v1",
                "--approve",
                "abc",
            ],
            "--approve requires a full 40-hex commit sha",
        ),
        (
            "trust beside available",
            &["extension", "list", "--trust", "--available"],
            "--trust and --available ask different questions",
        ),
    ] {
        let (status, _, stderr) = run(&world, args);
        assert_eq!(status.code(), Some(2), "case {case}: {stderr}");
        assert!(stderr.contains(needle), "case {case}: {stderr}");
        assert!(
            stderr.contains("relate extension"),
            "case {case}: the synopsis prints: {stderr}"
        );
    }
}

#[cfg(feature = "extension-install")]
#[test]
fn test_github_install_records_lists_and_forgets_the_pin() {
    let world = World::new();
    let (artifact, digest) = github_package(&world);
    let (api, api_port) = github_api(GITHUB_COMMIT_A, &digest);
    let (download, download_port) =
        spawn_single_response_bytes_capturing("200 OK", artifact.clone());
    let api_base = format!("http://127.0.0.1:{api_port}");
    let download_base = format!("http://127.0.0.1:{download_port}");
    let github = format!("owner/repo@{GITHUB_TAG}");
    let envs = [
        ("RELATE_GITHUB_API_URL", api_base.as_str()),
        ("RELATE_RELEASE_DOWNLOAD_URL", download_base.as_str()),
    ];

    let (status, stdout, stderr) = run_cli(
        world
            .opts(&["extension", "install", "greet", "--github", &github])
            .with_envs(&envs),
    );
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");
    assert_eq!(
        stdout.trim(),
        format!(
            "installed github-pin extension greet 1.2.3 from owner/repo@{GITHUB_TAG} \
             (commit {GITHUB_COMMIT_A})"
        )
    );
    assert_eq!(api.join().len(), 2);
    assert_eq!(download.join().len(), 1);
    let trust = world
        .home
        .path()
        .join("relate")
        .join("extensions-trust.json");
    assert!(trust.is_file(), "the trust store lives in the config dir");

    // The recorded pin is inspectable offline.
    let (status, stdout, stderr) = run(&world, &["extension", "list", "--trust"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        stdout.trim(),
        format!("greet owner/repo {GITHUB_TAG} {GITHUB_COMMIT_A} {digest}")
    );

    // remove without --forget-pin keeps the pin: a reinstall is still
    // compared against it.
    let (status, stdout, stderr) = run(&world, &["extension", "remove", "greet"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("removed extension greet"), "{stdout}");
    assert!(trust.is_file());
    let (status, stdout, _) = run(&world, &["extension", "list", "--trust"]);
    assert_eq!(status.code(), Some(0));
    assert!(stdout.contains(GITHUB_COMMIT_A), "{stdout}");

    // The same pin reinstalls silently (the recorded pin matches).
    let (api, api_port) = github_api(GITHUB_COMMIT_A, &digest);
    let (download, download_port) = spawn_single_response_bytes_capturing("200 OK", artifact);
    let api_base = format!("http://127.0.0.1:{api_port}");
    let download_base = format!("http://127.0.0.1:{download_port}");
    let envs = [
        ("RELATE_GITHUB_API_URL", api_base.as_str()),
        ("RELATE_RELEASE_DOWNLOAD_URL", download_base.as_str()),
    ];
    let (status, stdout, stderr) = run_cli(
        world
            .opts(&["extension", "install", "greet", "--github", &github])
            .with_envs(&envs),
    );
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");
    assert_eq!(api.join().len(), 2);
    assert_eq!(download.join().len(), 1);

    // --forget-pin clears the record; the empty store prints no rows.
    let (status, stdout, stderr) = run(&world, &["extension", "remove", "greet", "--forget-pin"]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("forgot the recorded pin for greet"),
        "{stdout}"
    );
    let (status, stdout, _) = run(&world, &["extension", "list", "--trust"]);
    assert_eq!(status.code(), Some(0));
    assert!(stdout.is_empty(), "{stdout}");
}

#[cfg(feature = "extension-install")]
#[test]
fn test_github_reinstall_refuses_a_moved_tag_before_the_fetch() {
    let world = World::new();
    let (artifact, digest) = github_package(&world);
    let (api, api_port) = github_api(GITHUB_COMMIT_A, &digest);
    let (download, download_port) =
        spawn_single_response_bytes_capturing("200 OK", artifact.clone());
    let api_base = format!("http://127.0.0.1:{api_port}");
    let download_base = format!("http://127.0.0.1:{download_port}");
    let github = format!("owner/repo@{GITHUB_TAG}");
    let envs = [
        ("RELATE_GITHUB_API_URL", api_base.as_str()),
        ("RELATE_RELEASE_DOWNLOAD_URL", download_base.as_str()),
    ];
    let (status, _, stderr) = run_cli(
        world
            .opts(&["extension", "install", "greet", "--github", &github])
            .with_envs(&envs),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(api.join().len(), 2);
    assert_eq!(download.join().len(), 1);

    // The tag moves between the two installs: the second refuses before
    // the asset request (the capture server stays at zero).
    let moved = github_package(&world);
    let (api, api_port) = github_api(GITHUB_COMMIT_B, &moved.1);
    let (sink, download_port) = spawn_capture_server("200 OK", "unexpected asset bytes");
    let api_base = format!("http://127.0.0.1:{api_port}");
    let download_base = format!("http://127.0.0.1:{download_port}");
    let envs = [
        ("RELATE_GITHUB_API_URL", api_base.as_str()),
        ("RELATE_RELEASE_DOWNLOAD_URL", download_base.as_str()),
    ];
    let (status, stdout, stderr) = run_cli(
        world
            .opts(&["extension", "install", "greet", "--github", &github])
            .with_envs(&envs),
    );
    assert_eq!(status.code(), Some(1), "{stdout}\n{stderr}");
    assert_eq!(api.join().len(), 2, "the metadata resolution still runs");
    assert_eq!(
        sink.request_count(),
        0,
        "the moved pin refuses before the asset fetch"
    );
    assert!(
        stderr.contains(&format!("commit {GITHUB_COMMIT_A}"))
            && stderr.contains(&format!("commit {GITHUB_COMMIT_B}")),
        "the refusal names both pins: {stderr}"
    );
    assert!(stderr.contains("refusing before any fetch"), "{stderr}");
    assert!(
        stderr.contains("--re-approve") && stderr.contains("--approve"),
        "{stderr}"
    );
}
