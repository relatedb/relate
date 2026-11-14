//! The `fs-to-graph` first-party importer child, end to end (ADR-0431 §11):
//! the extension crate built and installed as the package it ships as,
//! driven through the real binary's `run` mount — the fs coverage the
//! CLI's compiled-in lab e2e suites owned, moved onto the extension
//! mount with the importer itself. The summary text and starter queries
//! are pinned against the crate that owns them, and the interactive
//! handoff's PTY legs (ADR-0286) ride here too, beside the subject they
//! drive.
//!
//! One wire-forced delta from the compiled-in summary is pinned here
//! deliberately: the store label is "the store" — the child never sees
//! the target (ADR-0430 §6) — and the closing line rides indented as the
//! summary's last starter (the summary record's shape: text, then
//! starters). Every count, phrase and query is the compiled-in
//! importer's own, byte for byte.
//!
//! The fixture tree, the runners, and the store queries come from the
//! shared kit (`tests/support/importers.rs`); the git-to-graph twin
//! subject lives in `e2e_git_to_graph.rs`.

#![cfg(feature = "extensions")]

mod support;

use relate_fs_to_graph::STARTER_QUERIES as FS_STARTERS;
use relate_test_support::write_file;
// The statement cap the staged refusal names — the one constant the
// oversized fixture and its expected refusal text both derive from.
use relate_wire::MAX_STATEMENT_BYTES;

use support::importers::{fixture, fs_counts, run_import, run_import_memory, run_importer};
use support::{query_count, query_row, tempdir};

#[test]
fn fs_to_graph_imports_a_tree_with_edges_content_and_the_summary() {
    let home = tempfile::tempdir().unwrap();
    let root = fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import("fs-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "import failed: {stderr}");

    // The summary line, byte for byte: the compiled-in importer's counts
    // and phrase, the store label the wire forces (the child never sees
    // the target), the "search it with:" lead merged into the one text
    // line the summary record carries — then each starter indented, the
    // queries the crate itself owns.
    assert_eq!(
        stdout,
        format!(
            "imported 2 folders, 3 files (4 CONTAINS edges) into the store — search it with:\n  {}\n",
            FS_STARTERS[0]
        ),
        "the summary and its starter:\n{stdout}"
    );

    // Folders: root + sub. Files: alpha, beta, binary (the dotfile is skipped).
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:Folder) RETURN count(f) AS n"
        ),
        2
    );
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File) RETURN count(f) AS n"
        ),
        3
    );
    // Edges: root->sub, root->alpha, root->binary, sub->beta.
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH ()-[r:CONTAINS]->() RETURN count(r) AS n"
        ),
        4
    );

    // Content is captured for text files...
    assert_eq!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File {name: 'alpha.txt'}) RETURN f.content AS c"
        )["c"],
        serde_json::json!("the wombat sleeps at noon")
    );
    // ...but not for the NUL-containing binary.
    assert!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File {name: 'binary.dat'}) RETURN f.content AS c"
        )["c"]
            .is_null(),
        "binary files must carry no content property"
    );

    // The full-text index resolves a known term to the right file: the
    // post-commit `index` record ran at the import's index-creation point.
    assert_eq!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File) SEARCH f IN (FULLTEXT INDEX file_content FOR 'wombat' LIMIT 10) \
             RETURN f.name AS name"
        )["name"],
        serde_json::json!("alpha.txt")
    );
}

#[test]
fn fs_to_graph_reimport_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let root = fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import("fs-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "import failed: {stderr}");
    let (status, second_stdout, stderr) = run_import("fs-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "re-import failed: {stderr}");
    assert_eq!(
        second_stdout, stdout,
        "a re-import prints the same summary, starters included"
    );

    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:Folder) RETURN count(f) AS n"
        ),
        2
    );
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File) RETURN count(f) AS n"
        ),
        3
    );
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH ()-[r:CONTAINS]->() RETURN count(r) AS n"
        ),
        4
    );
}

/// `-d` is the short form of `--database`: the two spellings import the
/// same tree (ADR-0383). Each spelling writes its own store, so the
/// assertion compares what each one imported rather than a re-import into
/// one database.
#[test]
fn fs_to_graph_short_database_flag_imports_the_same_tree() {
    let home = tempfile::tempdir().unwrap();
    let root = fixture();
    let stores = tempfile::tempdir().unwrap();
    let [short, long] = ["short", "long"].map(|store| stores.path().join(store));

    for (db_flag, db) in [("-d", &short), ("--database", &long)] {
        let (status, _stdout, stderr) = run_importer("fs-to-graph", root.path(), db, db_flag, &[]);
        assert!(status.success(), "{db_flag} import failed: {stderr}");
    }
    for (query, expected) in [
        ("MATCH (f:Folder) RETURN count(f) AS n", 2),
        ("MATCH (f:File) RETURN count(f) AS n", 3),
        ("MATCH ()-[r:CONTAINS]->() RETURN count(r) AS n", 4),
    ] {
        let short_count = query_count(home.path(), &short, &[], query);
        let long_count = query_count(home.path(), &long, &[], query);
        assert_eq!(
            short_count, expected,
            "-d imported the wrong graph: {query}"
        );
        assert_eq!(
            short_count, long_count,
            "-d and --database must be interchangeable: {query}"
        );
    }
}

#[test]
fn include_hidden_adds_dotfiles() {
    let home = tempfile::tempdir().unwrap();
    let root = fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, _stdout, stderr) =
        run_import("fs-to-graph", root.path(), &db_path, &["--include-hidden"]);
    assert!(status.success(), "import failed: {stderr}");

    // The dotfile joins the three visible files.
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File) RETURN count(f) AS n"
        ),
        4
    );
    assert!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File {name: '.hidden.txt'}) RETURN f.name AS name"
        )["name"]
            .as_str()
            .is_some(),
        "the dotfile must be imported with --include-hidden"
    );
}

#[test]
fn max_content_bytes_drops_oversized_content() {
    let home = tempfile::tempdir().unwrap();
    let root = fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, _stdout, stderr) = run_import(
        "fs-to-graph",
        root.path(),
        &db_path,
        &["--max-content-bytes", "5"],
    );
    assert!(status.success(), "import failed: {stderr}");

    // "the wombat sleeps at noon" is larger than 5 bytes: metadata only.
    assert!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File {name: 'alpha.txt'}) RETURN f.content AS c"
        )["c"]
            .is_null(),
        "files over --max-content-bytes must carry no content property"
    );
}

#[test]
fn ignore_patterns_prune_matching_dirs_and_files() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    write_file(&root.path().join("alpha.txt"), b"alpha");
    write_file(&root.path().join("sub/beta.txt"), b"beta");
    write_file(
        &root.path().join("node_modules/left-pad/index.js"),
        b"module.exports = 1;",
    );
    write_file(&root.path().join("target/debug/build.o"), &[0u8, 1, 2]);
    write_file(&root.path().join("debug.log"), b"log");
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import(
        "fs-to-graph",
        root.path(),
        &db_path,
        &[
            "--ignore-patterns",
            "node_modules/",
            "--ignore-patterns",
            "target/",
            "--ignore-patterns",
            "debug.log",
        ],
    );
    assert!(status.success(), "import failed: {stderr}");
    // Ignored entries are absent from the summary counts too — the child's
    // own walk produced them.
    assert!(
        stdout.contains("imported 2 folders, 2 files (3 CONTAINS edges)"),
        "stdout: {stdout}"
    );

    // Folders: root + sub; node_modules/ and target/ are pruned entirely.
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:Folder) RETURN count(f) AS n"
        ),
        2
    );
    // Files: alpha + beta; debug.log and the pruned dirs' files are absent.
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File) RETURN count(f) AS n"
        ),
        2
    );
    // Edges: root->sub, root->alpha, sub->beta; none into the pruned dirs.
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH ()-[r:CONTAINS]->() RETURN count(r) AS n"
        ),
        3
    );

    // No node carries a pruned name.
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (n) WHERE n.name IN ['node_modules', 'target', 'debug.log', 'index.js', 'build.o'] \
             RETURN count(n) AS n"
        ),
        0
    );
}

/// The empty-pattern refusal moved into the child (ADR-0429 §5: the
/// host's parse sees the manifest's kinds only): the importer's own
/// wording, printed on the child's stderr and exit 1.
#[test]
fn an_empty_ignore_pattern_is_the_childs_refusal() {
    let root = fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import(
        "fs-to-graph",
        root.path(),
        &db_path,
        &["--ignore-patterns", "/"],
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr.trim(),
        "Error: the extension child exited with 1: invalid --ignore-patterns value: '/' is empty",
        "stderr: {stderr}"
    );
}

#[test]
fn reimport_drops_stale_content_when_a_file_becomes_oversized() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");
    let file = root.path().join("grow.txt");
    write_file(&file, b"tiny");

    // The first import captures the small text.
    let (status, _stdout, stderr) = run_import("fs-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "import failed: {stderr}");
    assert_eq!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File {name: 'grow.txt'}) RETURN f.content AS c"
        )["c"],
        serde_json::json!("tiny")
    );

    // The file grows past the cap between imports. The re-import must clear
    // the now-stale content property, not leave the first import's value.
    write_file(&file, b"this text is far too long for a five byte cap");
    let (status, _stdout, stderr) = run_import(
        "fs-to-graph",
        root.path(),
        &db_path,
        &["--max-content-bytes", "5"],
    );
    assert!(status.success(), "re-import failed: {stderr}");

    let row = query_row(
        home.path(),
        &db_path,
        &[],
        "MATCH (f:File {name: 'grow.txt'}) RETURN f.content AS c, keys(f) AS keys",
    );
    assert!(
        row["c"].is_null(),
        "a file that became oversized must carry no content"
    );
    assert!(
        !row["keys"]
            .as_array()
            .expect("keys(f) is a list")
            .iter()
            .any(|k| k == "content"),
        "the content property must be removed, not merely null: {row}"
    );
}

/// Per-batch failure semantics (ADR-0602, ADR-0392 §3's shape for the fs
/// importer): a run that fails after a phase's calls committed leaves that
/// phase's graph in the store — the folders phase lands before the files
/// phase refuses — and a re-run over the partially imported store completes
/// it to a clean import's counts.
///
/// The staged failure is real and deterministic: a `--max-content-bytes`
/// above the 8 MiB statement cap makes one file's row too large for any
/// single `write_cypher` call, so the child's chunker refuses it (a pinned
/// refusal) after the folders phase's call has already committed under the
/// per-batch framing. The completion re-run drops back under the statement
/// cap, so the oversized file imports metadata-only and every row fits.
#[test]
fn a_failed_import_keeps_committed_batches_and_a_rerun_completes() {
    let home = tempdir();
    let root = tempdir();
    write_file(&root.path().join("alpha.txt"), b"alpha");
    // One file whose row cannot fit a single `write_cypher` call: its
    // size is the wire kit's statement cap plus a MiB of margin, so the
    // boundary under test reads from the one constant the refusal names —
    // and the flag below hands the child the file's own size, so the
    // content is captured (the row built) rather than filtered.
    let huge_bytes = MAX_STATEMENT_BYTES + 1024 * 1024;
    write_file(
        &root.path().join("huge.txt"),
        &vec![b'x'; huge_bytes as usize],
    );
    let cap = huge_bytes.to_string();
    let db = tempdir();
    let db_path = db.path().join("graph");

    // One row that cannot fit a call: the import fails with the child's
    // pinned refusal, exit 1.
    let (status, stdout, stderr) = run_import(
        "fs-to-graph",
        root.path(),
        &db_path,
        &["--max-content-bytes", cap.as_str()],
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr.trim(),
        format!(
            "Error: the extension child exited with 1: a single row exceeds the \
             {MAX_STATEMENT_BYTES}-byte statement cap"
        ),
        "stderr: {stderr}"
    );

    // The folders phase committed before the files phase refused: the
    // walked root is in the store, nothing else is — no files, no
    // `CONTAINS` edges. Under whole-import this state was [0, 0, 0] — the
    // whole session rolled back.
    assert_eq!(
        fs_counts(home.path(), &db_path),
        [1, 0, 0],
        "the committed folders phase stays after the failed run"
    );

    // A re-run over the partially imported store completes it: MERGE by
    // path keeps the committed root, adds both files (the oversized one
    // metadata-only under the default cap) and both CONTAINS edges.
    let (status, _stdout, stderr) = run_import("fs-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "re-run failed: {stderr}");
    assert_eq!(
        fs_counts(home.path(), &db_path),
        [1, 2, 2],
        "the completing re-run reaches a clean import's counts"
    );
    assert!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File {name: 'huge.txt'}) RETURN f.size_bytes AS s"
        )["s"]
            .as_i64()
            .is_some(),
        "the oversized file imports metadata-only on the completing re-run"
    );
}

/// The default `:memory:` target on a non-TTY exits after the summary —
/// no shell, no store left behind (ADR-0286's non-interactive half). The
/// run happens in the asserted cwd (the kit's `run_import_memory` takes
/// it), so the leaves-no-store-behind assertion is live.
#[test]
fn fs_to_graph_memory_default_exits_after_the_summary() {
    let root = fixture();
    let cwd = tempdir();
    let (status, stdout, stderr) = run_import_memory("fs-to-graph", root.path(), &[], cwd.path());
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.starts_with("imported 2 folders, 3 files (4 CONTAINS edges) into the store"),
        "{stdout}"
    );
    assert!(
        std::fs::read_dir(cwd.path()).unwrap().count() == 0,
        "the in-memory run must leave no store behind"
    );
}

// --- the PTY legs (unix): the interactive handoff (ADR-0286) ---

/// Drive `run <importer> ...` on a real PTY through the run-import driver,
/// with the first-party packages installed under the shared root: `mode`
/// is `exit` (an explicit `--database` finishes the import and exits) or
/// `prompt` (the default in-memory run opens the shell).
#[cfg(unix)]
fn run_pty_import(mode: &str, args: &[&str]) {
    let (_, xdg) = support::importers::installed_root();
    let binary = support::binary_path_str();
    let mut argv = vec![mode, binary.as_str(), xdg];
    argv.extend_from_slice(args);
    support::assert_pty_driver_ok("pty_run_import.py", &argv);
}

/// LABS-166: an explicit `--database` on a TTY finishes the import and exits.
/// The database is still there to query afterwards.
#[cfg(unix)]
#[test]
fn explicit_db_on_a_tty_exits_without_opening_the_shell() {
    let home = tempfile::tempdir().unwrap();
    let root = fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    run_pty_import(
        "exit",
        &[
            "run",
            "fs-to-graph",
            root.path().to_str().unwrap(),
            "--database",
            db_path.to_str().unwrap(),
        ],
    );

    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (f:File) RETURN count(f) AS n"
        ),
        3
    );
}

/// The default in-memory import still opens the shell on a TTY, which is
/// the only way to query a graph that will not be checkpointed.
#[cfg(all(unix, feature = "repl"))]
#[test]
fn omitted_db_on_a_tty_opens_the_shell() {
    let root = fixture();
    run_pty_import(
        "prompt",
        &["run", "fs-to-graph", root.path().to_str().unwrap()],
    );
}
