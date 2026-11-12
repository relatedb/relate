//! The LABS-552 walkthrough e2e — the story the extension program exists
//! to make true (ADR-0431's opening paragraph): on a fresh default
//! install, `run git-to-graph` fails closed with the install hint,
//! `extension install git-to-graph` installs it, and `run git-to-graph
//! DIR -d store` imports and REPL-handoffs. Every leg drives the real
//! binary against a clean `$XDG_CONFIG_HOME` — the fresh-install machine
//! the ticket's walkthrough names.
//!
//! Three legs, one per acceptance of the walkthrough card:
//!
//! - **The offline variant (ADR-0431 §5)** runs the whole arc through the
//!   real verbs against one clean XDG dir: the hint, the `--from`
//!   install from the first-party package directory (the crate's
//!   committed `package/` manifest plus the built child, the directory
//!   the release pack stage assembles), the list row, the import into an
//!   explicit `-d` store — the non-TTY exit path under the harness
//!   (ADR-0286's non-interactive half; the REPL handoff is the PTY leg) —
//!   and the remove that returns the machine to the fresh state.
//! - **The signed-channel variant (§4)** walks as far as the real binary
//!   permits: the default install fetches the signed index and fails
//!   closed at the signature gate — the pinned release key never
//!   verifies a fixture-key signature, and the ADR pins no key seam, so
//!   the post-gate install of the walkthrough's own package is
//!   unit-driven over the fixture signer's chain
//!   (`extensions/install/tests.rs`, the ADR's stated plan) — and the
//!   release pack's own artifact (the real manifest and the real built
//!   child in the workflow's tarball shape) installs through the
//!   explicit `--url` trust act, the one tier the e2e can drive past the
//!   gate, after which the walkthrough continues on it.
//! - **The help delta** is a golden comparison: the final build's
//!   top-level `--help` against the pre-feature main's captured help,
//!   differing exactly by gaining the `run` and `extension` rows and
//!   losing the `lab` row (LABS-552's "differs only by the two new rows"
//!   plus the retirement's clean break, ADR-0431 §12).

#![cfg(feature = "extensions")]

mod support;

use std::process::ExitStatus;

use relate_test_support::git::history_fixture;
use relate_test_support::loopback::spawn_scripted_responses_capturing;

use support::extensions::{
    extensions_dir, install_child_file, run_in_xdg, xdg_root, RUN_HINT, RUN_USAGE,
};
use support::importers::{importer_bin, GIT_MANIFEST};
// Only the `full`-bundle help-golden test lists the compiled codecs.
#[cfg(feature = "full")]
use support::compiled_codecs;
use support::{query_count, run_cli, tempdir, CliRunOptions as Opts};

/// The channel index fixture (the committed `extensions.json`) and a
/// valid SSHSIG over it made by a fixture key — NOT the pinned release
/// key, so the channel leg stops at verification, deterministically (the
/// same pair `e2e_extensions` drives the generic channel refusals with).
const EXTENSIONS_JSON: &str = include_str!("fixtures/extension/extensions.json");
const TEST_KEY_SIG: &str = include_str!("fixtures/extension/extensions-test-key.sig");

/// The pre-feature main's captured top-level help — the golden the
/// feature's help delta is measured against (LABS-552: "Top-level
/// `--help` differs only by the two new rows"). Captured from the current
/// main build `53ddb29f` — the base this branch rebases onto, which
/// renamed `--memory-limit` to `--logical-graph-size-limit` (#512); the
/// fixture was regenerated after that landing so the delta below is the
/// feature's own. Capture shape: a worktree reset to `origin/main`,
/// `cargo build -p relate-cli --bin relate` with default features
/// (the release binary's feature set), `relate --help` with stdout
/// piped — the harness's own no-TTY shape, so the bytes are the ones this
/// suite's `run_cli` sees. Full-bundle only, beside the comparison that
/// reads it.
#[cfg(feature = "full")]
const PRE_FEATURE_HELP: &str = include_str!("fixtures/walkthrough/pre-feature-top-level-help.txt");

/// The version the git-to-graph crate's committed manifest declares —
/// the value the loud install line reads back from what landed.
fn manifest_version() -> String {
    let manifest: serde_json::Value =
        serde_json::from_str(GIT_MANIFEST).expect("the crate's committed manifest decodes");
    manifest["version"]
        .as_str()
        .expect("the manifest's version is a string")
        .to_string()
}

/// The first-party package directory the offline walkthrough installs
/// from (ADR-0431 §5): the crate's committed `package/` manifest plus
/// the built child under the manifest's own `executable` name — the
/// directory the release pack stage assembles (`release-extensions.yml`)
/// and the offline story's source of truth. The returned TempDir keeps
/// the directory alive for the caller's install — callers bind it as an
/// underscore binding (`let (_source, source) = …`), which holds the dir
/// to the end of the test without a trailing fake keep-alive `let`; the
/// string is the path the `--from` flag names.
fn first_party_package_dir() -> (tempfile::TempDir, String) {
    let stage = tempdir();
    let package = stage.path().join("git-to-graph");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("extension.json"), GIT_MANIFEST).unwrap();
    install_child_file(&package, &importer_bin("git-to-graph"));
    (stage, package.display().to_string())
}

/// Install git-to-graph through the real `--from` verb from the
/// first-party package directory, against `xdg`'s machine.
fn install_from_first_party(xdg: &str, source: &str) -> (ExitStatus, String, String) {
    run_in_xdg(
        &["extension", "install", "git-to-graph", "--from", source],
        xdg,
    )
}

/// The LABS-552 walkthrough on a fresh install, offline variant: one
/// clean XDG dir walks the whole arc. The hint leg is the fail-closed
/// discovery error (exit 2, never a fetch); the install leg lands the
/// accounted set with the loud line; the list leg reads the provenance
/// back; the run leg imports into an explicit store and exits after the
/// summary (the non-TTY half of ADR-0286 — the store is still there to
/// query); the remove leg closes the lifecycle and the hint returns.
#[cfg(feature = "extension-install")]
#[test]
fn the_labs_552_walkthrough_runs_on_a_fresh_install() {
    let (tmp, xdg) = xdg_root();
    let extensions = extensions_dir(tmp.path());

    // Fresh install, nothing yet: the walkthrough's own name fails
    // closed with the pinned discovery text (LABS-552 §4), byte for
    // byte, naming the first-party install path.
    let (status, stdout, stderr) = run_in_xdg(&["run", "git-to-graph"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        format!(
            "Error: no installed action 'git-to-graph' — first-party extension available: \
              relate extension install git-to-graph\n\
              {RUN_USAGE}\n{RUN_HINT}\n"
        )
    );
    assert!(!extensions.join("git-to-graph").exists());

    // The install: the loud line names the package, its version, and
    // the first-party package directory it came from.
    let (_source, source) = first_party_package_dir();
    let (status, stdout, stderr) = install_from_first_party(&xdg, &source);
    assert_eq!(status.code(), Some(0), "{stderr}");
    let version = manifest_version();
    assert_eq!(
        stdout.trim(),
        format!("installed local extension git-to-graph {version} from {source}"),
        "stdout: {stdout}"
    );
    // The installed dir holds exactly the accounted set plus the marker
    // (§4 step 5), and the source directory is left untouched (§5).
    let installed = extensions.join("git-to-graph");
    assert!(installed.join("extension.json").is_file());
    assert!(installed.join("git-to-graph").is_file());
    assert!(installed.join(".relate-provenance.json").is_file());
    let source_dir = std::path::Path::new(&source);
    assert!(source_dir.join("extension.json").is_file());
    assert!(source_dir.join("git-to-graph").is_file());

    // The list reads the provenance back: name, version, tier, source.
    let (status, stdout, stderr) = run_in_xdg(&["extension", "list"], &xdg);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        stdout.trim(),
        format!("git-to-graph {version} local {source}")
    );

    // The run: the import lands in the explicit `-d` store and the
    // non-TTY process exits after the summary (ADR-0286) — the store is
    // the walkthrough's queryable result.
    let (root, _repo) = history_fixture();
    let dir = root.path().to_str().expect("the fixture path is UTF-8");
    let stores = tempdir();
    let store = stores.path().join("graph");
    let store_str = store.to_str().expect("the store path is UTF-8");
    let (status, stdout, stderr) = run_in_xdg(&["run", "git-to-graph", dir, "-d", store_str], &xdg);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.starts_with("imported 3 commits, 2 people, 4 files"),
        "the import summary: {stdout}"
    );
    assert!(
        stderr.is_empty(),
        "the successful walkthrough run prints no error: {stderr}"
    );
    assert_eq!(
        query_count(
            tmp.path(),
            &store,
            &[],
            "MATCH (c:Commit) RETURN count(c) AS n"
        ),
        3,
        "the store the walkthrough imported into must be queryable"
    );

    // The remove closes the lifecycle; the machine is fresh again and
    // the hint is the fail-closed answer once more.
    let (status, stdout, stderr) = run_in_xdg(&["extension", "remove", "git-to-graph"], &xdg);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("removed extension git-to-graph"),
        "stdout: {stdout}"
    );
    assert!(!installed.exists());
    let (status, _stdout, stderr) = run_in_xdg(&["run", "git-to-graph"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.starts_with("Error: no installed action 'git-to-graph'"),
        "stderr: {stderr}"
    );
}

/// The REPL-handoff half of the walkthrough's third step (LABS-552,
/// ADR-0286): on a TTY the default in-memory import opens the shell
/// after the summary — the only way to query a graph that will not be
/// checkpointed. The package installs through the walkthrough's own
/// `--from` verb first, then the PTY driver runs `run git-to-graph`
/// with no `--database` against the same XDG root and pins the
/// `cypher>` prompt (the explicit `-d` leg above pins the exit path:
/// LABS-166's TTY refinement of the ticket's wording).
#[cfg(all(unix, feature = "extension-install", feature = "repl"))]
#[test]
fn the_walkthroughs_repl_handoff_opens_the_shell() {
    let (_tmp, xdg) = xdg_root();
    let (_source, source) = first_party_package_dir();
    let (status, _, stderr) = install_from_first_party(&xdg, &source);
    assert_eq!(status.code(), Some(0), "{stderr}");

    let (root, _repo) = history_fixture();
    let dir = root.path().to_str().expect("the fixture path is UTF-8");
    let binary = support::binary_path_str();
    support::assert_pty_driver_ok(
        "pty_run_import.py",
        &[
            "prompt",
            binary.as_str(),
            xdg.as_str(),
            "run",
            "git-to-graph",
            dir,
        ],
    );
}

/// The walkthrough's channel invocation (the LABS-552 default install:
/// `extension install git-to-graph`, no `--from` — the signed channel)
/// against a loopback channel serving the committed signed-index fixture
/// pair: the fixture-key signature never verifies against the pinned
/// release key, so the install fails closed at the signature gate — the
/// index and its signature are the only requests, nothing is downloaded,
/// nothing lands. The post-gate steps over the walkthrough's own package
/// (the row resolution, the route download, the signed-row checksum, the
/// unpack) are unit-driven in `extensions/install/tests.rs` — the ADR's
/// stated plan, no key seam.
#[cfg(feature = "extension-install")]
#[test]
fn the_walkthroughs_channel_install_fails_closed_at_the_signature_gate() {
    let (fixture, port) = spawn_scripted_responses_capturing(
        "200 OK",
        vec![EXTENSIONS_JSON.to_string(), TEST_KEY_SIG.to_string()],
    );
    let url = format!("http://127.0.0.1:{port}/extensions.json");
    let (tmp, xdg) = xdg_root();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["extension", "install", "git-to-graph"]).with_envs(&[
            ("XDG_CONFIG_HOME", xdg.as_str()),
            ("RELATE_EXTENSIONS_JSON_URL", url.as_str()),
            (
                "RELATE_RELEASE_DOWNLOAD_URL",
                format!("http://127.0.0.1:{port}").as_str(),
            ),
        ]),
    );
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("failed verification against the pinned release key"),
        "stderr: {stderr}"
    );
    let heads = fixture.join();
    assert_eq!(
        heads.len(),
        2,
        "the index and its signature only: {heads:?}"
    );
    assert!(
        !tmp.path()
            .join("relate")
            .join("extensions")
            .join("git-to-graph")
            .exists(),
        "a refused channel install lands nothing"
    );
}

/// The release pack's own artifact and the tier that carries it: the
/// real manifest and the real built child, packed in the workflow's
/// tarball shape (`release-extensions.yml`'s pack stage —
/// `extension.json` plus the executable, ustar+gzip, exec bit). The
/// URL tier takes that artifact only over https (§6's transport gate,
/// the security hardening): the plaintext loopback serve of the real
/// packed tarball refuses before any fetch — no request reaches the
/// server, nothing lands. The artifact's install and the walkthrough's
/// continuation run through the offline `--from` trust act (§5) on the
/// pack stage's package directory — the directory the tarball is packed
/// from — and `run git-to-graph DIR -d store` imports through it: the
/// walkthrough's third step, on the artifact the channel would
/// distribute. (The tarball's own fetch and unpack acceptance is
/// unit-pinned: `extensions/install/tests.rs` drives the walkthrough's
/// packed artifact through the channel's post-gate steps over a loopback
/// route, and the unpacker's own suite pins the closed member contract.
/// The signed-channel tier's install is unit-driven past the signature
/// gate either way.)
#[cfg(feature = "extension-install")]
#[test]
fn the_release_packs_artifact_installs_and_the_walkthrough_continues() {
    let bin = importer_bin("git-to-graph");
    let tarball = support::extensions::pack_extension_tarball(GIT_MANIFEST.as_bytes(), &bin);
    let (fixture, port) =
        relate_test_support::loopback::spawn_single_response_bytes_capturing("200 OK", tarball);
    let version = manifest_version();
    // A non-https, non-loopback URL: the gate's refusal case. (Plain
    // http to the loopback is the one carve-out — the local fixture
    // registry shape — so the refusal lane must not ride a loopback
    // host.)
    let url = format!("http://example.com:{port}/git-to-graph-{version}.tar.gz");
    let (tmp, xdg) = xdg_root();

    // The URL tier's transport gate on the artifact's own bytes: https
    // only (loopback plain http excepted), refused before any fetch (§6).
    let (status, stdout, stderr) = run_in_xdg(
        &["extension", "install", "git-to-graph", "--url", &url],
        &xdg,
    );
    assert_eq!(status.code(), Some(2), "{stdout}\n{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr.trim(),
        format!("Error: extension install: --url accepts only https URLs (or plain http to the loopback interface); '{url}' is neither")
    );
    assert!(
        fixture.join().is_empty(),
        "no request: the gate fires before the fetch"
    );
    assert!(
        !extensions_dir(tmp.path()).join("git-to-graph").exists(),
        "a refused URL install lands nothing"
    );

    // The walkthrough continues on the pack stage's package directory —
    // the directory the tarball is packed from — through the offline
    // trust act (§5).
    let (_source, source) = first_party_package_dir();
    let (status, stdout, stderr) = install_from_first_party(&xdg, &source);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        stdout.trim(),
        format!("installed local extension git-to-graph {version} from {source}"),
        "stdout: {stdout}"
    );

    // The walkthrough continues on the installed package.
    let (root, _repo) = history_fixture();
    let dir = root.path().to_str().expect("the fixture path is UTF-8");
    let stores = tempdir();
    let store = stores.path().join("graph");
    let store_str = store.to_str().expect("the store path is UTF-8");
    let (status, stdout, stderr) = run_in_xdg(&["run", "git-to-graph", dir, "-d", store_str], &xdg);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.starts_with("imported 3 commits, 2 people, 4 files"),
        "the import summary: {stdout}"
    );
    assert_eq!(
        query_count(
            tmp.path(),
            &store,
            &[],
            "MATCH (c:Commit) RETURN count(c) AS n"
        ),
        3,
        "the store the installed package imported into must be queryable"
    );
}

/// A line-wise diff of two help texts: the lines removed from `old` and
/// the lines added in `new`, against their longest common subsequence —
/// the golden comparison's delta, exact wherever the changed lines sit
/// (the help's rows are largely unique, so the alignment is stable).
/// Full-bundle only, beside the comparison that reads it.
#[cfg(feature = "full")]
fn line_diff<'a>(old: &[&'a str], new: &[&'a str]) -> (Vec<&'a str>, Vec<&'a str>) {
    // LCS lengths over suffix pairs: lcs[i][j] is old[i..] vs new[j..].
    let mut lcs = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            removed.push(old[i]);
            i += 1;
        } else {
            added.push(new[j]);
            j += 1;
        }
    }
    removed.extend_from_slice(&old[i..]);
    added.extend_from_slice(&new[j..]);
    (removed, added)
}

/// LABS-552's `--help` acceptance as a golden comparison: the final
/// build's top-level help differs from the pre-feature main's captured
/// help exactly by gaining the `run` and `extension` rows, losing the
/// `lab` row (the retirement's clean break, ADR-0431 §12), gaining the
/// markdown codec in the `--snapshot-format` row (ADR-0406), gaining
/// the `--markdown-body-properties` rows (ADR-0460), re-rendering
/// the `--durability-mode` row to carry its `RELATE_DURABILITY_MODE`
/// default (ADR-0470), gaining the `--database` target-axis
/// continuation and the `--load` entry (LABS-903, ADR-0500 — LABS-872's
/// remote fetch folded into `--load`), and gaining the `--timings` row
/// (ADR-0581) — every other line, byte for byte,
/// is the
/// feature's untouched inheritance, so any other help change this
/// feature made fails here. The full bundle only (the
/// release binary's feature set — the golden's build).
#[cfg(feature = "full")]
#[test]
fn the_top_level_help_differs_from_pre_feature_main_by_exactly_the_feature_rows() {
    let (status, current, stderr) = run_cli(Opts::new(&["--help"]));
    assert!(status.success(), "--help failed: {stderr}");
    assert!(stderr.is_empty(), "stderr must be empty: {stderr}");

    let pre: Vec<&str> = PRE_FEATURE_HELP.lines().collect();
    let now: Vec<&str> = current.lines().collect();
    let (removed, added) = line_diff(&pre, &now);
    // The gained row's value set is the one shared derivation
    // (`support::compiled_codecs`, over `compiled()`'s pinned order): a
    // codec change fails here without a golden re-capture.
    let gained_codec_row = format!(
        "  --snapshot-format F  Snapshot codec ({}); empty directory only",
        compiled_codecs()
    );
    // The full row texts, byte for byte: the golden pins not just the
    // words but the blurbs the rows carry.
    assert_eq!(
        removed,
        vec![
            "  lab            Import a directory tree or a git history into a graph (experimental)",
            "  --snapshot-format F  Snapshot codec (parquet|json); empty directory only",
            "  --durability-mode MODE  WAL durability (full|everysecond|snapshot; default full)",
        ],
        "the pre-feature help must lose exactly the lab, parquet-only snapshot-format, and\
         pre-variable durability-mode rows"
    );
    assert_eq!(
        added,
        vec![
            "  run            Run an installed extension action into a store the host owns",
            "  extension      Scaffold, install, check, list, or remove command extensions",
            "                       The store to open — never a URL: a remote store is a --load",
            "                       source",
            "  --timings     Append the per-statement timing line to --jsonl output (default off)",
            gained_codec_row.as_str(),
            "  --markdown-body-properties LIST  Markdown vault body properties (LIST or",
            "                       Label=LIST; entries separated by ';'; first bare, rest as",
            "                       ## sections; default: body)",
            "  --durability-mode MODE  WAL durability (full|everysecond|snapshot; default full,",
            "                       RELATE_DURABILITY_MODE)",
            "  --load SOURCE        Load a store's content from a .rl path, a directory, or an",
            "                       https URL. With :memory: (the default target) the source is",
            "                       turned into the in-memory store: a read-only open that releases",
            "                       the source after the load (nothing is held open or locked, so",
            "                       it can be deleted or opened writable underneath). With a",
            "                       --database path the source is cloned there and the store opens",
            "                       normally (the target must not exist yet). --read-only=live is",
            "                       refused",
            "                       An https URL is fetched to a private temp copy — a GitHub blob",
            "                       or raw link is rewritten to its raw form; a remote folder store",
            "                       is not supported. Nothing is kept: the memory target releases",
            "                       the copy after the load, a path target removes it after the",
            "                       clone",
        ],
        "the final build's help must gain exactly the run, extension, --database \
         target-axis (LABS-903, ADR-0500), --timings (ADR-0581), \
         markdown-codec snapshot-format, \
         markdown body-properties, durability-mode variable (ADR-0470), and \
         --load (LABS-903, ADR-0500; LABS-872's remote fetch folded into it) rows"
    );
}
