#![cfg(feature = "extensions")]
//! The git question catalog is executable: every ```cypher block in
//! `references/run-git-to-graph.md` must run against one imported fixture with
//! fixture values for all six documented parameters, exit 0, and return at
//! least one row (ADR-0376's catalog contract) — imported through the
//! first-party extension children now (ADR-0431 §11), the `run` mount the
//! packages install under.

use crate::support;

use std::path::{Path, PathBuf};

use relate_test_support::git::{iso_days_ago, Repo};
use relate_test_support::write_file;
use support::importers::run_import;
use support::{query_row, query_rows, seed_database};

/// The catalog ships beside the binary's sources, so `include_str!` fails the
/// build if the file moves or disappears. The path is relative to this test
/// file: `tests/../src/skill/bundle/references/`.
const CATALOG: &str = include_str!("../../src/skill/bundle/references/run-git-to-graph.md");

/// The only parameters a catalog query may reference (ADR-0376).
const ALLOWED_PARAMS: &[&str] = &["path", "paths", "prefix", "email", "term", "days"];

/// The fixture's `$days` window: every recent commit is inside it.
const FIXTURE_DAYS: u64 = 30;

/// Every ```cypher block in `markdown`, in order, without the fences.
///
/// The text before the first `` ```cypher `` fence is skipped by
/// `split`; each remaining segment runs from its fence to the next
/// ``` (which must exist — an unterminated block is a broken catalog).
fn cypher_blocks(markdown: &str) -> Vec<String> {
    markdown
        .split("```cypher")
        .skip(1)
        .map(|body| {
            let end = body
                .find("```")
                .expect("unterminated ```cypher block in the catalog");
            // The newline that ended the opening fence and the one that
            // starts the closing fence are framing, not content.
            body[..end].trim_matches('\n').to_string()
        })
        .collect()
}

/// The one ```cypher block under the catalog question with this heading.
fn catalog_query(heading: &str) -> String {
    let marker = format!("### {heading}");
    assert_eq!(
        CATALOG.matches(&marker).count(),
        1,
        "the catalog must hold exactly one question {heading:?}"
    );
    let question = CATALOG
        .split_once(&marker)
        .expect("the marker count above found the question")
        .1;
    let question = question.split("\n### ").next().expect("a question body");
    let mut blocks = cypher_blocks(&format!("### {heading}{question}"));
    assert_eq!(
        blocks.len(),
        1,
        "question {heading:?} must hold exactly one cypher block"
    );
    blocks.pop().expect("the one cypher block")
}

/// The parameter names a query references (`$name`), in first-seen order.
fn parameters(query: &str) -> Vec<String> {
    let chars: Vec<char> = query.chars().collect();
    let mut names = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        assert!(end > start, "a lone `$` in a query:\n{query}");
        names.push(chars[start..end].iter().collect());
        index = end;
    }
    names
}

/// `root` plus `parts`, rendered the way both imports store `File.path`.
fn stored_path(root: &Path, parts: &[&str]) -> String {
    parts
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part))
        .to_string_lossy()
        .into_owned()
}

/// One imported fixture graph plus the parameter values every catalog block
/// is run with.
struct Fixture {
    home: tempfile::TempDir,
    db: PathBuf,
    /// Kept alive for the repository's lifetime; not read after import.
    _root: tempfile::TempDir,
    /// Kept alive so git's fixture config outlives the test; not read after
    /// the commits are written.
    _repo: Repo,
    path: String,
    paths: Vec<String>,
    prefix: String,
    email: String,
    term: String,
}

impl Fixture {
    /// Every `--param name=value` argument, one per documented parameter.
    fn params(&self) -> Vec<String> {
        vec![
            format!("path={}", json(&self.path)),
            format!("paths={}", json(&self.paths)),
            format!("prefix={}", json(&self.prefix)),
            format!("email={}", json(&self.email)),
            format!("term={}", json(&self.term)),
            format!("days={FIXTURE_DAYS}"),
        ]
    }
}

/// One JSON-encoded parameter value; a `String` needs its quotes.
fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).expect("a parameter value serializes to JSON")
}

/// Run one query against the fixture database with the six fixture params,
/// keeping only the first row's `column`.
fn scalar(fixture: &Fixture, query: &str, column: &str) -> serde_json::Value {
    query_row(fixture.home.path(), &fixture.db, &fixture.params(), query)[column].clone()
}

/// Build the fixture history the catalog questions need: three authors, dates
/// spanning more than a year, a `fix:` commit, a file deleted in history, an
/// untracked file, and one commit over the default `--max-changeset` of 50.
fn build_fixture() -> Fixture {
    let home = tempfile::tempdir().expect("fixture home tempdir");
    let root = tempfile::tempdir().expect("fixture repository tempdir");
    let repo = Repo::init(root.path());

    let core = stored_path(root.path(), &["src", "core.rs"]);
    let core_test = stored_path(root.path(), &["src", "core_test.rs"]);
    let deleted = stored_path(root.path(), &["legacy", "old.rs"]);
    let prefix = format!(
        "{}{}",
        stored_path(root.path(), &["src"]),
        std::path::MAIN_SEPARATOR
    );

    // 400 days ago: every file the later commits build on.
    repo.write("src/core.rs", b"fn core() {}\n");
    repo.write("src/core_test.rs", b"fn core_test() {}\n");
    repo.write("docs/guide.md", b"# Guide\n\nshared notes\n");
    repo.write("legacy/old.rs", b"fn old() {}\n");
    repo.write("src/solo.rs", b"fn solo() {}\n");
    repo.commit_as(
        "Ada Lovelace",
        "ada@example.com",
        &iso_days_ago(400),
        "initial layout",
    );

    // 40 days ago: the coupling pair moves together for the first time.
    repo.write("src/core.rs", b"fn core() { /* v2 */ }\n");
    repo.write("src/core_test.rs", b"fn core_test() { /* v2 */ }\n");
    repo.write("docs/guide.md", b"# Guide\n\nshared notes v2\n");
    repo.commit_as(
        "Ada Lovelace",
        "ada@example.com",
        &iso_days_ago(40),
        "refactor core",
    );

    // 20 days ago: a second author on the coupling pair.
    repo.write("src/core.rs", b"fn core() { /* v3 */ }\n");
    repo.write("src/core_test.rs", b"fn core_test() { /* v3 */ }\n");
    repo.commit_as(
        "Grace Hopper",
        "grace@example.com",
        &iso_days_ago(20),
        "polish core",
    );

    // 15 days ago: a third author, sharing one file with the others.
    repo.write("docs/guide.md", b"# Guide\n\nshared notes v3\n");
    repo.write("src/core.rs", b"fn core() { /* v4 */ }\n");
    repo.commit_as(
        "Bob Kahn",
        "bob@example.com",
        &iso_days_ago(15),
        "docs and core",
    );

    // 10 days ago: the `fix:` commit the starter full-text query finds.
    repo.write("src/fix.rs", b"fn fix() { /* wombat */ }\n");
    repo.write("docs/guide.md", b"# Guide\n\nshared notes v4\n");
    repo.commit_as(
        "Grace Hopper",
        "grace@example.com",
        &iso_days_ago(10),
        "fix: wombat overflow",
    );

    // 5 days ago: a second fix, touching the single-author file too.
    repo.write("src/core.rs", b"fn core() { /* v5 */ }\n");
    repo.write("src/core_test.rs", b"fn core_test() { /* v5 */ }\n");
    repo.write("src/solo.rs", b"fn solo() { /* v2 */ }\n");
    repo.commit_as(
        "Ada Lovelace",
        "ada@example.com",
        &iso_days_ago(5),
        "fix: core edge case",
    );

    // 4 days ago: delete the history-only file and add a 52-path changeset
    // (51 new files plus the deletion), one over the default cap of 50.
    repo.remove("legacy/old.rs");
    for index in 0..51 {
        repo.write(
            &format!("bulk/file_{index:02}.txt"),
            format!("bulk {index}\n").as_bytes(),
        );
    }
    repo.commit_as(
        "Grace Hopper",
        "grace@example.com",
        &iso_days_ago(4),
        "bulk import",
    );

    // 3 days ago: one more docs change so the guide has three authors.
    repo.write("docs/guide.md", b"# Guide\n\nshared notes v5\n");
    repo.commit_as(
        "Bob Kahn",
        "bob@example.com",
        &iso_days_ago(3),
        "docs: wombat notes",
    );

    // Untracked: on disk, never committed; `$term` appears in its content so
    // the content full-text question is non-vacuous.
    write_file(
        &root.path().join("scratch").join("notes.txt"),
        b"fix notes: the wombat escaped\n",
    );

    let db = home.path().join("graph");
    let (status, _stdout, stderr) = run_import("fs-to-graph", root.path(), &db, &[]);
    assert!(status.success(), "fs-to-graph import failed: {stderr}");
    let (status, _stdout, stderr) = run_import("git-to-graph", root.path(), &db, &[]);
    assert!(status.success(), "git-to-graph import failed: {stderr}");

    Fixture {
        home,
        db,
        _root: root,
        _repo: repo,
        path: core,
        paths: vec![core_test, deleted],
        prefix,
        email: "ada@example.com".to_string(),
        term: "fix".to_string(),
    }
}

/// The catalog has at least 20 questions, each a `###` heading phrased as a
/// question with exactly one ```cypher block, and every required section has
/// its minimum.
#[test]
fn the_catalog_structure_holds() {
    let blocks = cypher_blocks(CATALOG);
    assert!(
        blocks.len() >= 20,
        "the catalog has {} cypher blocks; at least 20 questions are required",
        blocks.len()
    );

    let mut questions: Vec<&str> = CATALOG.split("\n### ").collect();
    let intro = questions.remove(0);
    assert!(
        !intro.contains("```cypher"),
        "the catalog intro must not hold a cypher block"
    );
    assert_eq!(
        questions.len(),
        blocks.len(),
        "every question needs exactly one ```cypher block"
    );
    for question in &questions {
        let heading = question.lines().next().unwrap_or_default();
        assert!(
            heading.trim_end().ends_with('?'),
            "question heading is not a question: {heading}"
        );
        assert_eq!(
            question.matches("```cypher").count(),
            1,
            "question must hold exactly one cypher block: {heading}"
        );
    }

    let sections: Vec<&str> = CATALOG.split("\n## ").skip(1).collect();
    let section_questions = |title: &str| {
        sections
            .iter()
            .find(|section| {
                section
                    .lines()
                    .next()
                    .is_some_and(|line| line.contains(title))
            })
            .map(|section| section.matches("\n### ").count())
            .unwrap_or(0)
    };
    for (title, minimum) in [
        ("Coupling", 2),
        ("Hotspots and churn", 2),
        ("Ownership and bus factor", 2),
        ("Reviewers and experts", 2),
        ("Time", 2),
        ("Commit messages", 2),
        ("People and collaboration", 2),
        ("Composing with fs-to-graph", 3),
    ] {
        assert!(
            section_questions(title) >= minimum,
            "section {title:?} has {} questions, needs at least {minimum}",
            section_questions(title)
        );
    }
}

/// The fixture really carries the history the catalog is meant to answer
/// questions about — a silent fixture regression would otherwise leave a
/// question vacuous without failing.
#[test]
fn the_fixture_covers_every_question() {
    let fixture = build_fixture();
    assert_eq!(
        scalar(&fixture, "MATCH (p:Person) RETURN count(p) AS n", "n"),
        serde_json::json!(3),
        "fixture must have three authors"
    );
    let span = scalar(
        &fixture,
        "MATCH (c:Commit) WITH min(c.authored_at) AS first, max(c.authored_at) AS last \
         RETURN duration.inDays(first, last).days AS n",
        "n",
    );
    assert!(
        span.as_i64().is_some_and(|days| days > 365),
        "fixture commit dates must span more than a year, got {span}"
    );
    assert_eq!(
        scalar(
            &fixture,
            "MATCH (c:Commit) RETURN max(c.files_changed) AS n",
            "n"
        ),
        serde_json::json!(52),
        "fixture must contain a commit over the default --max-changeset of 50"
    );
    assert_eq!(
        scalar(
            &fixture,
            "MATCH (c:Commit) WHERE c.subject STARTS WITH 'fix:' RETURN count(c) AS n",
            "n"
        ),
        serde_json::json!(2),
        "fixture must contain fix: commits"
    );
    assert_eq!(
        scalar(
            &fixture,
            "MATCH (f:File) WHERE NOT EXISTS { MATCH (:Folder)-[:CONTAINS]->(f) } \
             AND EXISTS { MATCH (:Commit)-[:TOUCHED]->(f) } RETURN count(f) AS n",
            "n"
        ),
        serde_json::json!(1),
        "fixture must contain one file deleted in history"
    );
    assert_eq!(
        scalar(
            &fixture,
            "MATCH (f:File) WHERE f.content IS NOT NULL \
             AND NOT EXISTS { MATCH (:Commit)-[:TOUCHED]->(f) } RETURN count(f) AS n",
            "n"
        ),
        serde_json::json!(1),
        "fixture must contain one untracked file with content"
    );
}

/// Every catalog block exits 0 with at least one row when run with fixture
/// values, and every parameter it uses is one the catalog documents.
#[test]
fn every_catalog_question_runs_and_returns_rows() {
    let fixture = build_fixture();
    let blocks = cypher_blocks(CATALOG);
    assert!(
        blocks.len() >= 20,
        "the catalog has only {} blocks",
        blocks.len()
    );
    for (index, query) in blocks.iter().enumerate() {
        for name in parameters(query) {
            assert!(
                ALLOWED_PARAMS.contains(&name.as_str()),
                "block {} uses undocumented parameter ${name}:\n{query}",
                index + 1
            );
        }
        let rows = query_rows(fixture.home.path(), &fixture.db, &fixture.params(), query);
        assert!(
            !rows.is_empty(),
            "catalog block {} returned no rows:\n{query}",
            index + 1
        );
    }
}

/// A directory's sole-owner question must key on people, not display names:
/// two `:Person` nodes with the same `name` but different `email` who both
/// touched one file must not make that file one person's. The query is read
/// from the catalog itself, so the shipped text is what runs.
#[test]
fn the_directory_sole_owner_query_counts_people_not_names() {
    let home = tempfile::tempdir().expect("sole-owner home tempdir");
    let db = home.path().join("graph");
    let shared = "/repo/src/shared.rs";
    let prefix = "/repo/src/";

    seed_database(
        home.path(),
        &db,
        &[],
        &format!(
            "CREATE (f:File {{path: '{shared}'}}) \
             CREATE (:Person {{name: 'Sam Rivera', email: 'sam@example.com'}}) \
             -[:AUTHORED]->(:Commit {{sha: 'c1'}})-[:TOUCHED]->(f)"
        ),
    );
    seed_database(
        home.path(),
        &db,
        &[],
        &format!(
            "MATCH (f:File {{path: '{shared}'}}) \
             CREATE (:Person {{name: 'Sam Rivera', email: 'sam.other@example.com'}}) \
             -[:AUTHORED]->(:Commit {{sha: 'c2'}})-[:TOUCHED]->(f)"
        ),
    );

    // The seed is the bug's shape: two people with one display name, one file.
    let shape = query_row(
        home.path(),
        &db,
        &[],
        "MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File) \
         RETURN count(DISTINCT p) AS people, count(DISTINCT f) AS files",
    );
    assert_eq!(
        (shape["people"].clone(), shape["files"].clone()),
        (serde_json::json!(2), serde_json::json!(1)),
        "the seed must be two people sharing a name on one file: {shape}"
    );

    let query = catalog_query("Which files in a directory are one person's?");
    let rows = query_rows(
        home.path(),
        &db,
        &[format!("prefix={}", json(&prefix))],
        &query,
    );
    assert!(
        rows.is_empty(),
        "the only file in the directory has two owners who share a display \
         name, so it is not one person's; the catalog query returned {rows:?}:\n{query}"
    );
}

/// Whether `text` states `value` as a whole number: a raw `contains`
/// would let `50` match inside `500000` and hide a wrong pairing.
fn states_number(text: &str, value: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == value)
}

/// The shipped summary's starter queries against the catalog: the
/// docs↔code tie the child crate used to assert, kept on the surface that
/// owns both the bundle and the child's public constants (a first-party
/// child never reads a surface's files, ADR-0434/R7).
#[test]
fn every_starter_query_appears_verbatim_in_the_question_catalog() {
    for query in relate_git_to_graph::STARTER_QUERIES {
        assert!(
            CATALOG.contains(query),
            "the question catalog no longer carries this starter query: {query}"
        );
    }
    // The summary's closing pointer names the catalog by its shipped path.
    assert!(
        CATALOG.contains("git-to-graph"),
        "the catalog must still be the git-to-graph question catalog"
    );
}

/// The catalog is the product document that states the ADR-0377 bounds;
/// this pins those statements to the constants with a token-boundary
/// check — a raw `contains` would let `500000` match inside `5000000` —
/// so a cap change cannot leave a stale document behind.
#[test]
fn the_catalog_states_the_numbers_the_constants_hold() {
    for number in [
        relate_git_to_graph::MAX_LOG_BYTES.to_string(),
        relate_git_to_graph::MAX_CO_CHANGED_PAIRS.to_string(),
        relate_git_to_graph::MAX_CHANGESET.to_string(),
        relate_git_to_graph::DEFAULT_MAX_CHANGESET.to_string(),
    ] {
        assert!(
            states_number(CATALOG, &number),
            "the question catalog no longer states {number}: update it \
             with the constant, never a number the code does not hold"
        );
    }
}
