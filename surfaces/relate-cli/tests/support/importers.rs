//! The first-party importer e2e kit (ADR-0431 §11): the `fs-to-graph` and
//! `git-to-graph` extension crates' children, built and installed as the
//! packages they ship as, driven through the real binary's `run` mount —
//! the fixture-harness pattern the wire fixtures established
//! (`e2e_extension_fixtures.rs`), applied to the crates that replaced the
//! compiled-in importers.
//!
//! One helper builds each crate's child binary (a `cargo build` of the
//! importer crate, its exec artifact read from the build messages — the
//! same target dir the test build warms), one installs both packages
//! under one isolated XDG root from the crates' own committed manifests
//! (the executable patched to the platform file name, the one
//! per-platform fact the packaging step owns), and the runners drive
//! `run <name>` invocations against that root.
//!
//! The subject suites' fixture trees and join assertions live here too
//! (`fixture`, `fs_counts`, `graph_counts`, the joined-import kit,
//! `wide_fixture`, `mktree`), so the per-subject suites (`e2e_fs_to_graph.rs`,
//! `e2e_git_to_graph.rs`) carry their tests and nothing else: one kit,
//! every fixture fact spelled once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::OnceLock;

use serde_json::Value;

use relate_test_support::git::{history_fixture, Repo, JAN_4};
use relate_test_support::write_file;

use super::extensions::{child_file_name, install_child_file, install_package, xdg_root};
use super::{query_count, query_rows, run_cli, CliRunOptions};

/// The committed manifests of the first-party importer packages — the
/// one manifest each crate ships beside its executable's source, included
/// here so a move or edit fails the build, not a silent drift between
/// the tested package and the shipped one. `GIT_MANIFEST` is shared with
/// the walkthrough suite, which installs the package through the real
/// verbs against these bytes.
const FS_MANIFEST: &str =
    include_str!("../../../../extensions/relate-fs-to-graph/package/extension.json");
pub const GIT_MANIFEST: &str =
    include_str!("../../../../extensions/relate-git-to-graph/package/extension.json");

/// One importer package: its mount name, its crate's package name, its
/// bin name, and its committed manifest.
struct Importer {
    action: &'static str,
    package: &'static str,
    bin: &'static str,
    manifest: &'static str,
}

/// The two first-party importers (ADR-0431 §11) — the channel's first
/// content, the e2e's whole subject.
const IMPORTERS: [Importer; 2] = [
    Importer {
        action: "fs-to-graph",
        package: "relate-fs-to-graph",
        bin: "fs-to-graph",
        manifest: FS_MANIFEST,
    },
    Importer {
        action: "git-to-graph",
        package: "relate-git-to-graph",
        bin: "git-to-graph",
        manifest: GIT_MANIFEST,
    },
];

/// Build the `action` importer's child binary and return its path — one
/// `cargo build` of the importer crate per test process, its executable
/// read from the build messages (the exact artifact, whatever the target
/// dir's override). The build rides the same target dir and lockfile
/// the test build itself used, so a warm run is a cached no-op.
/// `pub` for the suites that drive the child through other mounts than
/// the shared installed root (the walkthrough's `--from` install and
/// pack-shaped channel artifact).
pub fn importer_bin(action: &str) -> PathBuf {
    static BINS: OnceLock<BTreeMap<&'static str, PathBuf>> = OnceLock::new();
    let bins = BINS.get_or_init(|| {
        let mut built = BTreeMap::new();
        for importer in IMPORTERS {
            built.insert(importer.action, build_bin(&importer));
        }
        built
    });
    bins.get(action)
        .unwrap_or_else(|| panic!("no first-party importer '{action}'"))
        .clone()
}

/// Build one importer crate's bin: `cargo build -p <package> --bin <bin>`
/// with JSON messages, the executable read back from the build's
/// messages by [`artifact_from_messages`].
fn build_bin(importer: &Importer) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .arg("build")
        .arg("--package")
        .arg(importer.package)
        .arg("--bin")
        .arg(importer.bin)
        .arg("--message-format=json")
        .output()
        .unwrap_or_else(|e| panic!("failed to run cargo for {}: {e}", importer.package));
    assert!(
        output.status.success(),
        "cargo build -p {} failed:\n{}",
        importer.package,
        String::from_utf8_lossy(&output.stderr)
    );
    artifact_from_messages(
        importer.package,
        importer.bin,
        &String::from_utf8_lossy(&output.stdout),
    )
}

/// The executable path from one `cargo build`'s JSON message lines: the
/// `compiler-artifact` record for the bin (the exact artifact, whatever
/// the target dir's override). A line that does not decode as JSON
/// panics naming itself — `--message-format=json` puts only JSON
/// records on stdout, so a malformed line is a broken build, and a
/// silent skip could miss the artifact record and report "no
/// executable" without the cause.
fn artifact_from_messages(package: &str, bin: &str, stdout: &str) -> PathBuf {
    for line in stdout.lines() {
        let message: Value = serde_json::from_str(line).unwrap_or_else(|error| {
            panic!("cargo build -p {package} printed a malformed JSON line {line:?}: {error}")
        });
        if message["reason"] == "compiler-artifact"
            && message["target"]["name"] == bin
            && message["executable"].is_string()
        {
            return PathBuf::from(message["executable"].as_str().expect("the exec path"));
        }
    }
    panic!("cargo build -p {package} printed no executable artifact for {bin}");
}

/// [`artifact_from_messages`]'s contract, self-tested beside the kit that
/// owns it (the `support` module's guard-test pattern): the artifact
/// record resolves to the executable path, a malformed line panics
/// naming itself instead of being silently skipped, and a record-less
/// build names the package and bin it scanned for.
#[test]
fn the_cargo_message_scan_resolves_the_artifact_and_refuses_malformed_lines() {
    let messages = concat!(
        "{\"reason\":\"compiler-message\",\"message\":\"note\"}\n",
        "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"other\"},\"executable\":\"/tmp/other\"}\n",
        "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"fixture\"},\"executable\":\"/tmp/fixture\"}\n",
    );
    assert_eq!(
        artifact_from_messages("fixture-pkg", "fixture", messages),
        PathBuf::from("/tmp/fixture"),
        "the bin's own artifact record resolves, other records skipped"
    );

    // A non-JSON line is a broken build, not a skipped one: the panic
    // names the line.
    let caught = std::panic::catch_unwind(|| {
        artifact_from_messages("fixture-pkg", "fixture", "note: not json\n")
    });
    let payload = caught.expect_err("a malformed line must panic");
    let panic_text = payload
        .downcast_ref::<String>()
        .expect("the panic payload is a formatted string");
    assert!(
        panic_text.contains("malformed JSON line") && panic_text.contains("note: not json"),
        "the panic must name the offending line: {panic_text}"
    );

    let caught =
        std::panic::catch_unwind(|| artifact_from_messages("fixture-pkg", "fixture", "{}\n"));
    let payload = caught.expect_err("a record-less build must panic");
    let panic_text = payload
        .downcast_ref::<String>()
        .expect("the panic payload is a formatted string");
    assert!(
        panic_text.contains("no executable artifact")
            && panic_text.contains("fixture-pkg")
            && panic_text.contains("fixture"),
        "the panic must name the package and bin: {panic_text}"
    );
}

/// The shared install root: both first-party importer packages under one
/// tempdir `$XDG_CONFIG_HOME`, installed once per test process and read
/// by every run after (the registry only reads its packages; the
/// children never write to them). Parallel tests share it safely; a test
/// that needs a broken or divergent install builds its own root.
static INSTALLED: OnceLock<(tempfile::TempDir, String)> = OnceLock::new();

/// The installed importers' XDG root, installing on first use: the build's
/// children and the crates' own committed manifests (the executable field
/// patched to the platform file name the copy lands under — the one
/// per-platform fact the packaging step owns).
pub fn installed_root() -> (&'static Path, &'static str) {
    let (tmp, xdg) = INSTALLED.get_or_init(|| {
        let (tmp, xdg) = xdg_root();
        for importer in IMPORTERS {
            install_importer(tmp.path(), &importer);
        }
        (tmp, xdg)
    });
    (tmp.path(), xdg.as_str())
}

/// Install one importer package under `xdg`: the committed manifest with
/// its executable resolved to the copied bin's platform file name — the
/// name derived BEFORE the install, so the package's `extension.json`
/// is written once, final — and the built child copied beside it, exec
/// bit forced.
fn install_importer(xdg: &Path, importer: &Importer) {
    let mut manifest: Value =
        serde_json::from_str(importer.manifest).expect("the committed manifest decodes");
    let bin = importer_bin(importer.action);
    manifest["executable"] = Value::String(child_file_name(&bin));
    let package = install_package(
        xdg,
        importer.action,
        &serde_json::to_string(&manifest).unwrap(),
    );
    install_child_file(&package, &bin);
}

/// Run `run <action> DIR <db_flag> DB [extra...]` through the real binary
/// with the first-party importers installed under the shared isolated XDG
/// root — the importer e2e's one invocation shape, the old lab suites'
/// `run_lab_with_db_flag` role.
pub fn run_importer(
    action: &str,
    dir: &Path,
    db: &Path,
    db_flag: &str,
    extra: &[&str],
) -> (ExitStatus, String, String) {
    let (_, xdg) = installed_root();
    let dir = dir.to_str().expect("the importer DIR is UTF-8");
    let db = db.to_str().expect("the importer database path is UTF-8");
    let mut args = vec!["run", action, dir, db_flag, db];
    args.extend_from_slice(extra);
    run_cli(CliRunOptions::new(&args).with_envs(&[("XDG_CONFIG_HOME", xdg)]))
}

/// Run `run <action> DIR --database DB [extra...]` — the long store-flag
/// spelling, the form every case but the flag-equivalence tests uses.
pub fn run_import(
    action: &str,
    dir: &Path,
    db: &Path,
    extra: &[&str],
) -> (ExitStatus, String, String) {
    run_importer(action, dir, db, "--database", extra)
}

/// Run `run <action> DIR` on the `:memory:` default, in `in_dir` — the
/// non-TTY leg exits after the summary, leaving no store behind
/// (ADR-0286). The working directory is the caller's asserted cwd: the
/// leaves-no-store-behind check reads that directory, so the run must
/// happen there for the assertion to be live.
pub fn run_import_memory(
    action: &str,
    dir: &Path,
    extra: &[&str],
    in_dir: &Path,
) -> (ExitStatus, String, String) {
    let (_, xdg) = installed_root();
    let dir = dir.to_str().expect("the importer DIR is UTF-8");
    let mut args = vec!["run", action, dir];
    args.extend_from_slice(extra);
    run_cli(
        CliRunOptions::new(&args)
            .with_envs(&[("XDG_CONFIG_HOME", xdg)])
            .in_dir(in_dir),
    )
}

// --- the subject suites' fixture trees and join assertions ---

/// Build a fresh fs-to-graph fixture tree: nested dirs, two text files,
/// a NUL binary, a dotfile.
pub fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write_file(&root.path().join("alpha.txt"), b"the wombat sleeps at noon");
    write_file(&root.path().join("sub/beta.txt"), b"beta file");
    write_file(&root.path().join("binary.dat"), &[0u8, 1, 2, 3, 0, 255]);
    write_file(&root.path().join(".hidden.txt"), b"secret wombat");
    root
}

/// The three graph totals the fs subject pins: folders, files, `CONTAINS`
/// edges.
pub fn fs_counts(home: &Path, db: &Path) -> [u64; 3] {
    [
        query_count(home, db, &[], "MATCH (f:Folder) RETURN count(f) AS n"),
        query_count(home, db, &[], "MATCH (f:File) RETURN count(f) AS n"),
        query_count(
            home,
            db,
            &[],
            "MATCH ()-[r:CONTAINS]->() RETURN count(r) AS n",
        ),
    ]
}

/// The five graph totals the git subject pins: commits, people, files,
/// `TOUCHED` edges, `CO_CHANGED` edges.
pub fn graph_counts(home: &Path, db: &Path) -> [u64; 5] {
    [
        query_count(home, db, &[], "MATCH (c:Commit) RETURN count(c) AS n"),
        query_count(home, db, &[], "MATCH (p:Person) RETURN count(p) AS n"),
        query_count(home, db, &[], "MATCH (f:File) RETURN count(f) AS n"),
        query_count(
            home,
            db,
            &[],
            "MATCH ()-[r:TOUCHED]->() RETURN count(r) AS n",
        ),
        query_count(
            home,
            db,
            &[],
            "MATCH ()-[r:CO_CHANGED]->() RETURN count(r) AS n",
        ),
    ]
}

/// The tracked files the canonical join fixture keeps on disk; both carry
/// text content and an incoming `TOUCHED` edge.
const TRACKED: [&str; 2] = ["tracked.txt", "src/nested.txt"];

/// The file the canonical join fixture deletes in history: a git-only node.
const DELETED: &str = "deleted.txt";

/// The visible file the canonical join fixture leaves untracked: an fs-only
/// node.
const UNTRACKED: &str = "untracked.txt";

/// Import one repository into one database with both importer actions, in
/// the order `actions` names, always passing the same `DIR` string, then
/// assert the join: every tracked on-disk file is exactly one `:File` node
/// with content and at least one incoming `TOUCHED`; the deleted-in-history
/// file is a git-only node and the untracked file an fs-only node, both in
/// the same graph.
///
/// The join is the commit-semantics acceptance on the wire: whichever
/// action lands second writes into a populated store, so its `MERGE`
/// phases (the child cannot read the target — ADR-0430 §6) complete the
/// graph the first import built (ADR-0392).
pub fn assert_joined_import(home: &Path, root: &Path, db: &Path, actions: &[&str]) {
    for action in actions {
        let (status, _stdout, stderr) = run_import(action, root, db, &[]);
        assert!(status.success(), "{action} failed: {stderr}");
    }

    let files = file_graph(home, db);
    let touched = touched_counts(home, db);
    // tracked-on-disk + visible-untracked + deleted-in-history.
    assert_eq!(
        files.len(),
        4,
        "exactly one :File node per tracked, deleted and untracked path: {files:?}"
    );
    for relative in TRACKED {
        let path = root.join(relative).to_string_lossy().into_owned();
        let content = files
            .get(&path)
            .unwrap_or_else(|| panic!("tracked file {path} has no :File node: {files:?}"));
        assert!(
            content.is_some(),
            "tracked file {path} carries no content: {files:?}"
        );
        assert!(
            touched.get(&path).is_some_and(|n| *n > 0),
            "tracked file {path} has no incoming TOUCHED edge: {touched:?}"
        );
    }

    let deleted = root.join(DELETED).to_string_lossy().into_owned();
    assert_eq!(
        files.get(&deleted),
        Some(&None),
        "the deleted-in-history file must be a contentless git-only node: {files:?}"
    );
    assert!(
        touched.get(&deleted).is_some_and(|n| *n > 0),
        "the deleted-in-history file has no incoming TOUCHED edge: {touched:?}"
    );

    let untracked = root.join(UNTRACKED).to_string_lossy().into_owned();
    assert!(
        files.get(&untracked).is_some_and(Option::is_some),
        "the untracked file must be an fs-only node with content: {files:?}"
    );
    assert!(
        !touched.contains_key(&untracked),
        "the untracked file must have no incoming TOUCHED edge"
    );
}

/// Every `:File` node as `path -> content`. A path seen twice fails while the
/// map is built, so "one node per path" is asserted, not assumed.
fn file_graph(home: &Path, db: &Path) -> BTreeMap<String, Option<String>> {
    let mut files = BTreeMap::new();
    for row in query_rows(
        home,
        db,
        &[],
        "MATCH (f:File) RETURN f.path AS path, f.content AS content",
    ) {
        let path = row["path"]
            .as_str()
            .unwrap_or_else(|| panic!("File.path is not a string: {row}"))
            .to_string();
        let content = match &row["content"] {
            serde_json::Value::Null => None,
            serde_json::Value::String(content) => Some(content.clone()),
            other => panic!("File.content is not a string: {other}"),
        };
        assert!(
            files.insert(path.clone(), content).is_none(),
            "duplicate :File node for path {path}"
        );
    }
    files
}

/// Every path with at least one incoming `TOUCHED` edge, and its edge count.
fn touched_counts(home: &Path, db: &Path) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for row in query_rows(
        home,
        db,
        &[],
        "MATCH (:Commit)-[r:TOUCHED]->(f:File) RETURN f.path AS path, count(r) AS n",
    ) {
        let path = row["path"]
            .as_str()
            .unwrap_or_else(|| panic!("File.path is not a string: {row}"))
            .to_string();
        let n = row["n"]
            .as_u64()
            .unwrap_or_else(|| panic!("TOUCHED count is not an integer: {row}"));
        counts.insert(path, n);
    }
    counts
}

/// The history fixture plus one commit touching three files that no other
/// commit touches, so a `CO_CHANGED` pair from it is unambiguous.
pub fn wide_fixture() -> (tempfile::TempDir, Repo) {
    let (dir, repo) = history_fixture();
    repo.write("triple/one.txt", b"one");
    repo.write("triple/two.txt", b"two");
    repo.write("triple/three.txt", b"three");
    repo.commit_as(
        "Ada Newest",
        "Ada@Example.COM",
        JAN_4,
        "four: a wide change",
    );
    (dir, repo)
}

/// Run `git mktree` in `repo` with `entries` on stdin and return the tree id.
pub fn mktree(repo: &Repo, entries: &str) -> String {
    repo.git_stdin(&["mktree"], entries.as_bytes())
}
