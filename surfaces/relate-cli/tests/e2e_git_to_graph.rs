//! The `git-to-graph` first-party importer child, end to end (ADR-0431
//! §11): the extension crate built and installed as the package it ships
//! as, driven through the real binary's `run` mount — the git coverage
//! the CLI's compiled-in lab e2e suites owned, moved onto the extension
//! mount with the importer itself. The summary text and starter queries
//! are pinned against the crate that owns them, and the commit semantics
//! the acceptance carries are pinned through the joined imports: the
//! child always `MERGE`s (it cannot read the target's node count,
//! ADR-0430 §6), so the populated-store completion the compiled-in
//! importer's re-run gave is the joined-import tests' subject.
//!
//! Two wire-forced deltas from the compiled-in summary are pinned here
//! deliberately: the store label is "the store" — the child never sees
//! the target (ADR-0430 §6) — and the closing catalog pointer rides
//! indented as the summary's last starter (the summary record's shape:
//! text, then starters). Every count, phrase and query is the
//! compiled-in importer's own, byte for byte.
//!
//! The history fixtures, the graph totals, the joined-import kit, and
//! the runners come from the shared kit (`tests/support/importers.rs`);
//! the fs-to-graph twin subject lives in `e2e_fs_to_graph.rs`.

#![cfg(feature = "extensions")]

mod support;

use relate_git_to_graph::{REFERENCE_LINE, STARTER_QUERIES as GIT_STARTERS};
use relate_test_support::git::{history_fixture, joined_fixture, Repo, JAN_4};
use relate_test_support::write_file;

use support::importers::{
    assert_joined_import, graph_counts, mktree, run_import, run_importer, wide_fixture,
};
use support::{query_count, query_row, query_rows};

/// The import summary's stdout, byte for byte: the counts sentence the
/// child reports — `imported <counts> into the store — try:` — then every
/// starter query and the catalog pointer, each two-space indented as the
/// summary record's shape carries them (ADR-0447). One builder, so every
/// summary-pinning test in this suite shares one spelling of the
/// rendering.
fn expected_import_stdout(counts: &str) -> String {
    let mut expected = format!("imported {counts} into the store — try:\n");
    for starter in GIT_STARTERS.iter().chain(std::iter::once(&REFERENCE_LINE)) {
        expected.push_str("  ");
        expected.push_str(starter);
        expected.push('\n');
    }
    expected
}

#[test]
fn git_to_graph_imports_the_history_and_prints_the_starter_queries() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import("git-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "git import failed: {stderr}");

    // The summary, byte for byte: the compiled-in importer's counts and
    // phrases, the wire-forced store label and starter placement, and the
    // queries plus the catalog pointer the crate itself owns.
    assert_eq!(
        stdout,
        expected_import_stdout("3 commits, 2 people, 4 files (6 TOUCHED, 3 CO_CHANGED edges)"),
        "the summary and its starters"
    );

    // The printed advice must be true: every starter query runs on the
    // graph the summary just described and finds a row.
    for query in GIT_STARTERS {
        assert!(
            !query_rows(home.path(), &db_path, &[], query).is_empty(),
            "starter query returned no rows: {query}"
        );
    }

    assert_eq!(graph_counts(home.path(), &db_path), [3, 2, 4, 6, 3]);
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH ()-[r:AUTHORED]->() RETURN count(r) AS n"
        ),
        3
    );
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH ()-[r:CO_CHANGED]->() RETURN sum(r.count) AS n"
        ),
        3
    );
    // Every CO_CHANGED pair comes from a single two-file commit.
    let edge_counts = query_rows(
        home.path(),
        &db_path,
        &[],
        "MATCH ()-[r:CO_CHANGED]->() RETURN r.count AS n",
    );
    assert_eq!(edge_counts.len(), 3, "pairs: {edge_counts:?}");
    for edge in &edge_counts {
        assert_eq!(edge["n"], 1, "one change per pair: {edge_counts:?}");
    }

    // The newest commit decides the Person.name, and the two case spellings
    // of Ada's email are one person (ADR-0376).
    assert_eq!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (p:Person {email: 'ada@example.com'}) RETURN p.name AS n"
        )["n"],
        "Ada Newest"
    );
    assert_eq!(
        query_row(
            home.path(),
            &db_path,
            &[],
            "MATCH (c:Commit {subject: 'three: extend a'}) \
             RETURN c.authored_at = datetime('2026-01-03T00:00:00Z') AS n"
        )["n"],
        true
    );
}

#[test]
fn git_to_graph_reimport_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import("git-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "git import failed: {stderr}");
    let counts_before = graph_counts(home.path(), &db_path);
    assert_eq!(counts_before, [3, 2, 4, 6, 3]);
    let sum_before = query_count(
        home.path(),
        &db_path,
        &[],
        "MATCH ()-[r:CO_CHANGED]->() RETURN sum(r.count) AS n",
    );

    // The same import again must recompute the same graph, never add a
    // second node or increment an edge count (ADR-0376): the child's
    // `MERGE` statements complete the already-imported store.
    let (status, second_stdout, stderr) = run_import("git-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "re-import failed: {stderr}");
    assert_eq!(second_stdout, stdout, "a re-import prints the same summary");

    assert_eq!(graph_counts(home.path(), &db_path), counts_before);
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH ()-[r:CO_CHANGED]->() RETURN sum(r.count) AS n"
        ),
        sum_before
    );
}

/// The ADR-0597 era shape: `--until` alone reads a window that need not
/// end at HEAD, through the real mount — the manifest row, the host's
/// parse and the child's envelope all carry the flag. The fixture's three
/// commits sit at JAN_1, JAN_2 and JAN_3 (committer dates), so an era cut
/// at JAN_2 keeps the two older commits and cannot be spelled with
/// `--since` and `--max-commits` alone.
#[test]
fn an_until_window_imports_an_era_that_stops_before_head() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import(
        "git-to-graph",
        root.path(),
        &db_path,
        &["--until", "2026-01-02T00:00:00Z"],
    );
    assert!(status.success(), "era import failed: {stderr}");
    assert_eq!(
        stdout,
        expected_import_stdout("2 commits, 2 people, 3 files (4 TOUCHED, 2 CO_CHANGED edges)"),
        "the era summary and its starters"
    );
    assert_eq!(graph_counts(home.path(), &db_path), [2, 2, 3, 4, 2]);
    assert_eq!(
        query_rows(
            home.path(),
            &db_path,
            &[],
            "MATCH (c:Commit) RETURN c.subject AS subject ORDER BY subject"
        )
        .iter()
        .map(|row| row["subject"].as_str().expect("subject is a string"))
        .collect::<Vec<_>>(),
        [
            "fix: repair the shared module",
            "one: add the shared module"
        ],
        "the era stops before the newest commit"
    );
}

/// The ADR-0597 closed range: `--since A --until B` reads exactly the
/// slice between the bounds, reproducible forever — the same command reads
/// the same commits however much history lands after B.
#[test]
fn a_closed_since_until_range_imports_only_the_middle_slice() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import(
        "git-to-graph",
        root.path(),
        &db_path,
        &[
            "--since",
            "2026-01-01T00:00:01Z",
            "--until",
            "2026-01-02T00:00:01Z",
        ],
    );
    assert!(status.success(), "closed-range import failed: {stderr}");
    assert_eq!(
        stdout,
        expected_import_stdout("1 commits, 1 people, 2 files (2 TOUCHED, 1 CO_CHANGED edges)"),
        "the closed-range summary and its starters"
    );
    assert_eq!(graph_counts(home.path(), &db_path), [1, 1, 2, 2, 1]);
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (c:Commit {subject: 'fix: repair the shared module'}) RETURN count(c) AS n"
        ),
        1,
        "the closed range keeps exactly the middle commit"
    );
}

/// `-d` is the short form of `--database`: both spellings import the same
/// history (ADR-0383).
#[test]
fn git_to_graph_short_database_flag_imports_the_same_history() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let stores = tempfile::tempdir().unwrap();
    let [short, long] = ["short", "long"].map(|store| stores.path().join(store));

    for (db_flag, db) in [("-d", &short), ("--database", &long)] {
        let (status, _stdout, stderr) = run_importer("git-to-graph", root.path(), db, db_flag, &[]);
        assert!(status.success(), "{db_flag} import failed: {stderr}");
    }
    let short_counts = graph_counts(home.path(), &short);
    assert_eq!(short_counts, [3, 2, 4, 6, 3], "-d imported the wrong graph");
    assert_eq!(
        short_counts,
        graph_counts(home.path(), &long),
        "-d and --database must be interchangeable"
    );
}

#[test]
fn a_no_ff_merge_commit_is_absent_and_contributes_no_touches() {
    let home = tempfile::tempdir().unwrap();
    let (root, repo) = history_fixture();

    // One single-file commit on a side branch, merged with `--no-ff` so the
    // fixture really has a merge commit. ADR-0376 reads HEAD with
    // `--no-merges`: the merge contributes no `:Commit` and no `TOUCHED`,
    // while the side commit itself imports through the change it carries.
    repo.git(&["checkout", "-q", "-b", "side"]);
    repo.write("src/side.rs", b"side");
    repo.commit_as("Bob", "bob@example.com", JAN_4, "four: add the side file");
    repo.git(&["checkout", "-q", "main"]);
    let merge = repo.merge("side", "merge the side branch");
    assert_eq!(
        repo.git(&["log", "--merges", "--format=%H"]),
        merge,
        "the fixture has exactly one merge commit"
    );
    assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "5");

    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");
    let (status, _stdout, stderr) = run_import("git-to-graph", root.path(), &db_path, &[]);
    assert!(status.success(), "git import failed: {stderr}");

    // Three base commits plus the side commit; the merge is absent. Files and
    // TOUCHED count the side commit's one new file once, and the merge's own
    // changeset contributes nothing.
    assert_eq!(graph_counts(home.path(), &db_path), [4, 2, 5, 7, 3]);
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            &format!("MATCH (c:Commit {{sha: '{merge}'}}) RETURN count(c) AS n"),
        ),
        0,
        "the merge sha must not be a :Commit"
    );
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            &format!("MATCH (:Commit {{sha: '{merge}'}})-[r:TOUCHED]->() RETURN count(r) AS n"),
        ),
        0,
        "the merge must contribute no TOUCHED edge"
    );
    let subjects = query_rows(
        home.path(),
        &db_path,
        &[],
        "MATCH (c:Commit) RETURN c.subject AS subject ORDER BY subject",
    );
    let subjects: Vec<&str> = subjects
        .iter()
        .map(|row| row["subject"].as_str().expect("subject is a string"))
        .collect();
    assert_eq!(
        subjects,
        [
            "fix: repair the shared module",
            "four: add the side file",
            "one: add the shared module",
            "three: extend a",
        ],
        "exactly the non-merge commits"
    );
}

#[test]
fn fs_then_git_import_joins_every_tracked_file() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = joined_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    assert_joined_import(
        home.path(),
        root.path(),
        &db_path,
        &["fs-to-graph", "git-to-graph"],
    );
}

#[test]
fn git_then_fs_import_joins_every_tracked_file() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = joined_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    assert_joined_import(
        home.path(),
        root.path(),
        &db_path,
        &["git-to-graph", "fs-to-graph"],
    );
}

#[test]
fn max_commits_imports_only_the_newest_commit() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import(
        "git-to-graph",
        root.path(),
        &db_path,
        &["--max-commits", "1"],
    );
    assert!(status.success(), "git import failed: {stderr}");
    let first = stdout.lines().next().unwrap_or_default();
    assert!(
        first.starts_with(
            "imported 1 commits, 1 people, 2 files (2 TOUCHED, 1 CO_CHANGED edges) into"
        ),
        "stdout: {stdout}"
    );
    assert_eq!(
        graph_counts(home.path(), &db_path),
        [1, 1, 2, 2, 1],
        "only the newest commit's graph"
    );
    let subjects = query_rows(
        home.path(),
        &db_path,
        &[],
        "MATCH (c:Commit) RETURN c.subject AS subject",
    );
    assert_eq!(subjects.len(), 1);
    assert_eq!(subjects[0]["subject"], "three: extend a");
}

#[test]
fn since_imports_only_commits_after_the_date() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    // Strictly between the JAN_1 and JAN_2 commits: only JAN_2 and JAN_3 are
    // newer, and git filters on the committer date `commit_as` set.
    let (status, stdout, stderr) = run_import(
        "git-to-graph",
        root.path(),
        &db_path,
        &["--since", "2026-01-01T12:00:00Z"],
    );
    assert!(status.success(), "git import failed: {stderr}");
    let first = stdout.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("imported 2 commits, 2 people,"),
        "stdout: {stdout}"
    );

    let result = query_rows(
        home.path(),
        &db_path,
        &[],
        "MATCH (c:Commit) RETURN c.subject AS subject ORDER BY subject",
    );
    let subjects: Vec<&str> = result
        .iter()
        .map(|row| row["subject"].as_str().expect("subject is a string"))
        .collect();
    assert_eq!(
        subjects,
        ["fix: repair the shared module", "three: extend a"]
    );
}

#[test]
fn max_changeset_skips_pairs_from_a_three_file_commit() {
    let home = tempfile::tempdir().unwrap();
    let (root, repo) = wide_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");
    let wide_sha = repo.head();

    let (status, _stdout, stderr) = run_import(
        "git-to-graph",
        root.path(),
        &db_path,
        &["--max-changeset", "2"],
    );
    assert!(status.success(), "git import failed: {stderr}");

    // The wide commit is still imported, with all three TOUCHED edges...
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            "MATCH (c:Commit) RETURN count(c) AS n"
        ),
        4
    );
    assert_eq!(
        query_count(
            home.path(),
            &db_path,
            &[],
            &format!(
                "MATCH (c:Commit {{sha: '{wide_sha}'}})-[:TOUCHED]->(f:File) RETURN count(f) AS n"
            ),
        ),
        3
    );

    // ...but contributes no CO_CHANGED pair: only the three earlier 2-file
    // commits' pairs exist, and none joins two of the wide commit's files.
    let pairs = query_rows(
        home.path(),
        &db_path,
        &[],
        "MATCH (a:File)-[r:CO_CHANGED]->(b:File) RETURN a.path AS a, b.path AS b",
    );
    assert_eq!(pairs.len(), 3, "pairs: {pairs:?}");
    let wide: Vec<String> = ["one.txt", "two.txt", "three.txt"]
        .iter()
        .map(|name| {
            root.path()
                .join("triple")
                .join(name)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for pair in &pairs {
        let a = pair["a"].as_str().unwrap();
        let b = pair["b"].as_str().unwrap();
        let from_wide_commit =
            wide.iter().any(|path| path == a) && wide.iter().any(|path| path == b);
        assert!(
            !from_wide_commit,
            "a commit over --max-changeset must not pair its files: {a} -> {b}"
        );
    }
}

#[test]
fn missing_directory_exits_one() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, _stdout, stderr) = run_import("git-to-graph", &missing, &db_path, &[]);
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("not a directory"), "stderr: {stderr}");
}

/// A path that exists but is a regular file: the exact refusal line, exit 1,
/// and nothing on stdout.
#[test]
fn a_non_directory_path_exits_one_with_the_exact_error_and_no_stdout() {
    let file = tempfile::tempdir().unwrap();
    let file_path = file.path().join("not-a-dir");
    write_file(&file_path, b"file");
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    let (status, stdout, stderr) = run_import("git-to-graph", &file_path, &db_path, &[]);
    assert_eq!(status.code(), Some(1), "stdout: {stdout} stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr.trim(),
        format!(
            "Error: the extension child exited with 1: not a directory: {}",
            file_path.display()
        )
    );
}

#[test]
fn plain_directory_reports_not_a_git_repository() {
    let root = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    // A plain (non-repository) directory: git exits non-zero and its stderr
    // rides the child's own stderr under the CLI's `Error:` line with exit 1
    // (ADR-0376). A failed import prints nothing on stdout.
    let (status, stdout, stderr) = run_import("git-to-graph", root.path(), &db_path, &[]);
    assert_eq!(status.code(), Some(1), "stdout: {stdout} stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.starts_with("Error: "), "stderr: {stderr}");
    assert!(stderr.contains("not a git repository"), "stderr: {stderr}");
    assert!(stderr.contains("fatal:"), "stderr: {stderr}");
}

#[test]
fn a_crafted_tree_path_exits_one_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repo = Repo::init(root.path());
    repo.write("a.txt", b"a");
    let base = repo.commit("base");
    let rev = format!("{base}:a.txt");
    let blob = repo.git(&["rev-parse", &rev]);
    // `git mktree` accepts `..` as a tree entry name; the commit built from
    // that tree therefore carries a path no ordinary checkout can produce.
    let tree = mktree(&repo, &format!("100644 blob {blob}\t..\n"));
    let crafted = repo.git(&["commit-tree", &tree, "-p", &base, "-m", "crafted"]);
    repo.git(&["update-ref", "refs/heads/main", &crafted]);

    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");
    let (status, stdout, stderr) = run_import("git-to-graph", root.path(), &db_path, &[]);
    assert_eq!(status.code(), Some(1), "stdout: {stdout} stderr: {stderr}");
    assert_eq!(
        stderr.trim(),
        format!(
            "Error: the extension child exited with 1: malformed git path \"..\" \
             in commit {crafted}: component \"..\" is not allowed"
        ),
        "stdout: {stdout}"
    );
    assert_eq!(
        query_count(home.path(), &db_path, &[], "MATCH (n) RETURN count(n) AS n"),
        0,
        "a refused path must leave the store empty"
    );
}

/// The value-range refusals the host's kind-checked parse cannot see
/// (ADR-0429 §5): the child prints them on its stderr and exits 1, with the
/// importer's own wording — the same texts the compiled-in importer's usage
/// errors carried.
#[test]
fn value_ranges_are_the_childs_refusals() {
    let home = tempfile::tempdir().unwrap();
    let (root, _repo) = history_fixture();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");

    for (extra, expected) in [
        (
            &["--max-changeset", "1001"][..],
            "Error: the extension child exited with 1: \
             invalid --max-changeset: '1001' is above the maximum 1000",
        ),
        (
            &["--max-commits", "-1"][..],
            "Error: the extension child exited with 1: \
             invalid --max-commits: '-1' is not a positive integer",
        ),
    ] {
        let (status, stdout, stderr) = run_import("git-to-graph", root.path(), &db_path, extra);
        assert_eq!(status.code(), Some(1), "{extra:?}: {stderr}");
        assert!(stdout.is_empty(), "{extra:?}: stdout: {stdout}");
        assert_eq!(stderr.trim(), expected, "{extra:?}");
        assert_eq!(
            query_count(home.path(), &db_path, &[], "MATCH (n) RETURN count(n) AS n"),
            0,
            "a refused import must leave the store empty: {extra:?}"
        );
    }
}
