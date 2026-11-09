//! `relate info --database PATH [--json]` end-to-end (ADR-0165, ADR-0200): the cold
//! store-metadata read, plus the `.info` dot-command session diagnostic that
//! shares its store-block wording. The `info` command compiles only with the
//! `info` feature (`full` and `agent` bundles), so its tests are gated
//! `#[cfg(feature = "info")]`; the `.info` dot-command compiles in every
//! build (ungated in main.rs), so its tests are ungated and keep running in
//! the `minimal` bundle. Parquet-specific expectations are gated on
//! `all(feature = "info", feature = "parquet")` — they drive the `info`
//! command against a Parquet store, so they need both (`agent` has `info`
//! without `parquet`).

mod support;

#[cfg(feature = "info")]
use support::seed_database;
use support::{run_cli_in_home, run_cli_stdin_in_home, tempdir};

#[cfg(feature = "info")]
const SEED: &str = "CREATE (:Person {id: 1, name: 'ada'})";

// --- `relate info` command (feature `info`) ---

#[cfg(feature = "info")]
#[test]
fn test_help_lists_info() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli_in_home(&["info", "--help"], home.path());
    assert!(status.success(), "info --help failed: {stderr}");
    assert!(
        stdout.contains("relate info --database PATH [--json]"),
        "expected the info synopsis in info --help, got: {stdout}"
    );
    assert!(
        stdout.contains("--json"),
        "expected --json in the info help, got: {stdout}"
    );
}

#[cfg(feature = "info")]
#[test]
fn test_info_requires_a_database() {
    let home = tempdir();
    for args in [vec!["info"], vec!["info", "--json"]] {
        let (status, _stdout, stderr) = run_cli_in_home(&args, home.path());
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains("info requires --database"),
            "{args:?}: expected a missing-DATABASE usage error, got: {stderr}"
        );
    }
}

#[cfg(feature = "info")]
#[test]
fn test_info_rejects_duplicated_json_flag() {
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli_in_home(
        &["info", "--database", "./db", "--json", "--json"],
        home.path(),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("only once"),
        "expected a duplicated-flag usage error, got: {stderr}"
    );
}

#[cfg(feature = "info")]
#[test]
fn test_info_rejects_unknown_flag() {
    let home = tempdir();
    let (status, _stdout, stderr) =
        run_cli_in_home(&["info", "--database", "./db", "--bogus"], home.path());
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("unexpected argument to info: --bogus"),
        "expected an unknown-flag usage error, got: {stderr}"
    );
}

#[cfg(feature = "info")]
#[test]
fn test_info_rejects_extra_positional() {
    let home = tempdir();
    let (status, _stdout, stderr) =
        run_cli_in_home(&["info", "--database", "./a", "./b"], home.path());
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("unexpected argument to info: ./b"),
        "expected an extra-positional usage error, got: {stderr}"
    );
}

#[cfg(feature = "info")]
#[test]
fn test_info_rejects_memory() {
    let home = tempdir();
    let (status, _stdout, stderr) =
        run_cli_in_home(&["info", "--database", ":memory:"], home.path());
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains(":memory:"),
        "expected a :memory: usage error, got: {stderr}"
    );
}

#[cfg(feature = "info")]
#[test]
fn test_info_rejects_a_query_and_an_access_flag() {
    let home = tempdir();
    for args in [
        vec!["info", "db", "RETURN 1"],
        vec!["-c", "RETURN 1", "info", "db"],
        vec!["--read-only", "info", "db"],
    ] {
        let (status, _stdout, stderr) = run_cli_in_home(&args, home.path());
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("info"), "{args:?}: {stderr}");
    }
}

#[cfg(feature = "info")]
#[test]
fn test_info_on_a_missing_database_is_a_runtime_error() {
    let home = tempdir();
    let missing = home.path().join("nope");
    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", missing.to_str().unwrap()],
        home.path(),
    );
    assert_eq!(status.code(), Some(1), "stdout: {stdout}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("Failed to read store info"),
        "expected the runtime error, got: {stderr}"
    );
}

/// `info` passes `--database` verbatim to `store_info_uri`, which owns the URI
/// classification: a `file://` URI reads the store exactly like the bare
/// path — and like `.info` on a `file://` session (pinned below).
#[cfg(feature = "info")]
#[test]
fn test_info_accepts_a_file_uri() {
    let home = tempdir();
    let db = home.path().join("db");
    seed_database(home.path(), &db, &["--snapshot-format", "json"], SEED);

    let uri = format!("file://{}", db.display());
    let (status, stdout, stderr) = run_cli_in_home(&["info", "--database", &uri], home.path());
    assert!(status.success(), "info over a file:// URI failed: {stderr}");
    assert!(stdout.contains("layout: native directory"), "{stdout}");
    assert!(stdout.contains("codec: json"), "{stdout}");
    assert!(stdout.contains("checkpoint LSN: "), "{stdout}");
}

/// JSON-codec expectations shared by every `info` build: the store reports
/// the codec envelope only (ADR-0165 Decision 5).
#[cfg(feature = "info")]
#[test]
fn test_info_json_codec_store_reports_the_envelope_only() {
    let home = tempdir();
    let db = home.path().join("db");
    seed_database(home.path(), &db, &["--snapshot-format", "json"], SEED);

    let (status, stdout, stderr) =
        run_cli_in_home(&["info", "--database", db.to_str().unwrap()], home.path());
    assert!(status.success(), "info failed: {stderr}");
    assert!(stdout.contains("layout: native directory"), "{stdout}");
    assert!(stdout.contains("codec: json"), "{stdout}");
    assert!(stdout.contains("format version: unavailable"), "{stdout}");
    assert!(stdout.contains("checkpoint LSN: "), "{stdout}");
    // The shared store-block wording; the `.info` tests below pin the same
    // labels for the dot-command's store block.
    assert!(stdout.contains("commit timestamp: "), "{stdout}");
    assert!(stdout.contains("total bytes: "), "{stdout}");
    assert!(
        stdout.contains("wal segments past checkpoint: "),
        "{stdout}"
    );
    assert!(stdout.contains("nodes: unavailable"), "{stdout}");
    assert!(stdout.contains("relationships: unavailable"), "{stdout}");
    assert!(stdout.contains("indexes: unavailable"), "{stdout}");
    assert!(stdout.contains("constraints: unavailable"), "{stdout}");
}

#[cfg(feature = "info")]
#[test]
fn test_info_json_output_on_a_json_codec_store() {
    let home = tempdir();
    let db = home.path().join("db");
    seed_database(home.path(), &db, &["--snapshot-format", "json"], SEED);

    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", db.to_str().unwrap(), "--json"],
        home.path(),
    );
    assert!(status.success(), "info --json failed: {stderr}");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("bad JSON {stdout:?}: {e}"));
    assert_eq!(value["layout"], "native_dir");
    assert_eq!(value["superblock_version"], serde_json::Value::Null);
    assert_eq!(value["codec"], "json");
    assert_eq!(value["format_version"], serde_json::Value::Null);
    assert!(value["checkpoint_lsn"].is_u64(), "{value}");
    assert!(value["commit_ts"].is_u64(), "{value}");
    assert!(value["total_bytes"].as_u64().unwrap() > 0, "{value}");
    assert!(value["wal_segments"].is_u64(), "{value}");
    assert_eq!(value["node_count"], serde_json::Value::Null);
    assert_eq!(value["rel_count"], serde_json::Value::Null);
    assert_eq!(value["indexes"], serde_json::Value::Null);
    assert_eq!(value["constraints"], serde_json::Value::Null);
}

#[cfg(feature = "info")]
#[test]
fn test_info_reports_a_bundle_layout() {
    let home = tempdir();
    let db = home.path().join("db");
    let bundle = home.path().join("backup.rl");
    seed_database(home.path(), &db, &["--snapshot-format", "json"], SEED);
    let (status, _stdout, stderr) = run_cli_in_home(
        &["clone", db.to_str().unwrap(), bundle.to_str().unwrap()],
        home.path(),
    );
    assert!(status.success(), "clone to a bundle failed: {stderr}");

    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", bundle.to_str().unwrap()],
        home.path(),
    );
    assert!(status.success(), "info on a bundle failed: {stderr}");
    assert!(stdout.contains("layout: .rl bundle"), "{stdout}");
    assert!(stdout.contains("superblock version: "), "{stdout}");

    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", bundle.to_str().unwrap(), "--json"],
        home.path(),
    );
    assert!(status.success(), "info --json on a bundle failed: {stderr}");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("bad JSON {stdout:?}: {e}"));
    assert_eq!(value["layout"], "bundle");
    assert!(value["superblock_version"].is_u64(), "{value}");
}

/// A store seeded with a known shape — 3 nodes, 2 relationships, one
/// explicit index, one UNIQUE constraint — reports those exact values
/// through `info`, and the same assertions hold against a `.rl` bundle copy
/// of the store (ADR-0165 Decision 1: the bundle carries the same manifest).
#[cfg(all(feature = "info", feature = "parquet"))]
#[test]
fn test_info_seeded_store_reports_the_same_manifest_as_native_dir_and_bundle() {
    let home = tempdir();
    let db = home.path().join("db");
    let bundle = home.path().join("backup.rl");
    seed_cataloged_store(home.path(), &db);

    let (status, stdout, stderr) =
        run_cli_in_home(&["info", "--database", db.to_str().unwrap()], home.path());
    assert!(status.success(), "info failed: {stderr}");
    assert_seeded_table(&stdout, "native directory", 3, 2);
    assert!(!stdout.contains("superblock version"), "{stdout}");

    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", db.to_str().unwrap(), "--json"],
        home.path(),
    );
    assert!(status.success(), "info --json failed: {stderr}");
    let value = assert_seeded_json(&stdout, "native_dir", 3, 2);
    assert_eq!(value["superblock_version"], serde_json::Value::Null);

    let (status, _stdout, stderr) = run_cli_in_home(
        &["clone", db.to_str().unwrap(), bundle.to_str().unwrap()],
        home.path(),
    );
    assert!(status.success(), "clone to a bundle failed: {stderr}");

    // The `.rl` bundle copy reports the same seeded manifest.
    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", bundle.to_str().unwrap()],
        home.path(),
    );
    assert!(status.success(), "info on a bundle failed: {stderr}");
    assert_seeded_table(&stdout, ".rl bundle", 3, 2);
    assert!(stdout.contains("superblock version: "), "{stdout}");

    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", bundle.to_str().unwrap(), "--json"],
        home.path(),
    );
    assert!(status.success(), "info --json on a bundle failed: {stderr}");
    let value = assert_seeded_json(&stdout, "bundle", 3, 2);
    assert!(value["superblock_version"].is_u64(), "{value}");
}

/// Seed a Parquet-codec store with a known shape — 3 nodes, 2
/// relationships, plus the index/constraint catalog — through the CLI
/// binary; the exit checkpoint pins the shape into the manifest.
#[cfg(all(feature = "info", feature = "parquet"))]
fn seed_cataloged_store(home: &std::path::Path, db: &std::path::Path) {
    let (status, _stdout, stderr) = run_cli_in_home(
        &[
            "--snapshot-format",
            "parquet",
            "--database",
            db.to_str().unwrap(),
            "CREATE (a:Person {id: 1, name: 'ada'})-[:KNOWS]->(b:Person {id: 2, name: 'bob'}); \
             CREATE (c:Person {id: 3, name: 'cid'}); \
             MATCH (a:Person {id: 2}), (c:Person {id: 3}) CREATE (a)-[:KNOWS]->(c)",
        ],
        home,
    );
    assert!(status.success(), "seed failed: {stderr}");
    seed_catalog(home, db);
}

/// Seed the index/constraint catalog — the explicit `person_name` index
/// plus the `person_id_unique` UNIQUE constraint — through the CLI binary;
/// the exit checkpoint pins it into the manifest.
#[cfg(all(feature = "info", feature = "parquet"))]
fn seed_catalog(home: &std::path::Path, db: &std::path::Path) {
    let (status, _stdout, stderr) = run_cli_in_home(
        &[
            "--database",
            db.to_str().unwrap(),
            "CREATE INDEX person_name FOR (n:Person) ON (n.name); \
             CREATE CONSTRAINT person_id_unique FOR (n:Person) REQUIRE n.id IS UNIQUE",
        ],
        home,
    );
    assert!(status.success(), "catalog seed failed: {stderr}");
}

/// The assertions every layout of a seeded store must satisfy in the human
/// table: exact checkpoint-time counts labeled with the checkpoint LSN, and
/// the index/constraint catalog listed by name. The `person_name` /
/// `person_id_unique` catalog contract is pinned here, once.
#[cfg(all(feature = "info", feature = "parquet"))]
fn assert_seeded_table(stdout: &str, layout: &str, nodes: u64, rels: u64) {
    assert!(stdout.contains(&format!("layout: {layout}")), "{stdout}");
    assert!(stdout.contains("codec: parquet"), "{stdout}");
    assert!(stdout.contains("format version: 5"), "{stdout}");
    assert!(stdout.contains("checkpoint LSN: "), "{stdout}");
    assert!(stdout.contains("commit timestamp: "), "{stdout}");
    let (nodes_lsn, nodes_got) = labeled_count(stdout, "nodes").expect("nodes line");
    assert_eq!(
        nodes_got, nodes,
        "checkpoint-time node count, got: {stdout}"
    );
    let (rels_lsn, rels_got) = labeled_count(stdout, "relationships").expect("relationships line");
    assert_eq!(rels_got, rels, "checkpoint-time rel count, got: {stdout}");
    assert_eq!(
        nodes_lsn, rels_lsn,
        "both counts carry the same checkpoint LSN label: {stdout}"
    );
    // A UNIQUE constraint auto-creates its backing index, so the catalog
    // holds two indexes: the constraint's and the explicit one.
    assert!(stdout.contains("indexes: 2"), "{stdout}");
    assert!(stdout.contains("person_name: :Person(name)"), "{stdout}");
    assert!(stdout.contains("person_id_unique: :Person(id)"), "{stdout}");
    assert!(stdout.contains("constraints: 1"), "{stdout}");
    assert!(
        stdout.contains("person_id_unique: unique node :Person(id)"),
        "{stdout}"
    );
}

/// The `key (at checkpoint LSN N): V` line of the human table, as `(N, V)`.
#[cfg(all(feature = "info", feature = "parquet"))]
fn labeled_count(stdout: &str, key: &str) -> Option<(u64, u64)> {
    stdout.lines().find_map(|line| {
        let rest = line.strip_prefix(format!("{key} (at checkpoint LSN ").as_str())?;
        let (lsn, count) = rest.split_once("): ")?;
        Some((lsn.parse().ok()?, count.trim().parse().ok()?))
    })
}

/// The assertions every layout of a seeded store must satisfy in the
/// `--json` object; the catalog contract is pinned here, once. Returns the
/// parsed value for layout-specific checks.
#[cfg(all(feature = "info", feature = "parquet"))]
fn assert_seeded_json(stdout: &str, layout: &str, nodes: u64, rels: u64) -> serde_json::Value {
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("bad JSON {stdout:?}: {e}"));
    assert_eq!(value["layout"], layout, "{value}");
    assert_eq!(value["codec"], "parquet", "{value}");
    assert_eq!(value["format_version"], 5, "{value}");
    assert!(value["checkpoint_lsn"].is_u64(), "{value}");
    assert_eq!(value["node_count"], nodes, "{value}");
    assert_eq!(value["rel_count"], rels, "{value}");
    assert!(value["commit_ts"].is_u64(), "{value}");
    assert!(value["total_bytes"].as_u64().unwrap() > 0, "{value}");
    assert!(value["wal_segments"].is_u64(), "{value}");
    // A UNIQUE constraint auto-creates its backing index, so the catalog
    // holds two indexes: the constraint's and the explicit one.
    let indexes = value["indexes"].as_array().unwrap();
    assert_eq!(indexes.len(), 2, "{value}");
    assert!(
        indexes.contains(&serde_json::json!({
            "name": "person_name",
            "label": "Person",
            "property": "name",
        })),
        "{value}"
    );
    assert!(
        indexes.contains(&serde_json::json!({
            "name": "person_id_unique",
            "label": "Person",
            "property": "id#RANGE#person_id_unique",
        })),
        "{value}"
    );
    assert_eq!(
        value["constraints"],
        serde_json::json!([{
            "name": "person_id_unique",
            "kind": "unique",
            "label": "Person",
            "relationship": false,
            "properties": ["id"],
            "property_type": null,
            "enforced_label": null,
        }]),
        "{value}"
    );
    value
}

/// Parquet-codec stores carry the full manifest: format version,
/// checkpoint-time counts, and the index/constraint catalog.
#[cfg(all(feature = "info", feature = "parquet"))]
#[test]
fn test_info_parquet_store_reports_the_full_manifest() {
    let home = tempdir();
    let db = home.path().join("db");
    seed_database(home.path(), &db, &["--snapshot-format", "parquet"], SEED);
    seed_catalog(home.path(), &db);

    let (status, stdout, stderr) =
        run_cli_in_home(&["info", "--database", db.to_str().unwrap()], home.path());
    assert!(status.success(), "info failed: {stderr}");
    assert_seeded_table(&stdout, "native directory", 1, 0);
}

#[cfg(all(feature = "info", feature = "parquet"))]
#[test]
fn test_info_json_output_on_a_parquet_store() {
    let home = tempdir();
    let db = home.path().join("db");
    seed_database(home.path(), &db, &["--snapshot-format", "parquet"], SEED);
    seed_catalog(home.path(), &db);

    let (status, stdout, stderr) = run_cli_in_home(
        &["info", "--database", db.to_str().unwrap(), "--json"],
        home.path(),
    );
    assert!(status.success(), "info --json failed: {stderr}");
    assert_seeded_json(&stdout, "native_dir", 1, 0);
}

// --- `.info` dot-command (ADR-0165) ---
//
// `.info` compiles in every build (ungated in main.rs), so these tests are
// ungated and run in the `minimal` bundle too — gating them on the `info`
// feature would silently drop that coverage. The store block shares its
// wording with `relate info` (store_info_render::InfoReport); the
// command tests above pin the same labels.

/// The value half of a `.info` `key: value` line (`key` includes the colon).
fn info_value(stdout: &str, key: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(key).map(str::trim).map(str::to_string))
}

#[test]
fn test_dot_info_memory_store_metadata_not_applicable() {
    let home = tempdir();
    let script = "CREATE (:Person {name: 'Alice'});\nCREATE INDEX person_name FOR (n:Person) ON (n.name);\n.info\n";
    let (status, stdout, stderr) =
        run_cli_stdin_in_home(&["query"], home.path(), script.as_bytes());
    assert!(status.success(), ".info on :memory: failed: {stderr}");
    let store = info_value(&stdout, "store:").expect("store line");
    assert!(
        store.contains("not applicable"),
        ":memory: must mark store metadata not applicable, got: {store}"
    );
    assert_eq!(
        info_value(&stdout, "nodes:").as_deref(),
        Some("1"),
        "live node count, got: {stdout}"
    );
    assert_eq!(
        info_value(&stdout, "relationships:").as_deref(),
        Some("0"),
        "live relationship count, got: {stdout}"
    );
    // The catalog comes from SHOW INDEXES / SHOW CONSTRAINTS on the open
    // connection, so it includes DDL from earlier in the same session.
    assert!(
        stdout.contains("indexes:"),
        "catalog heading, got: {stdout}"
    );
    assert!(
        stdout.contains("constraints:"),
        "catalog heading, got: {stdout}"
    );
    assert!(
        stdout.contains("person_name"),
        "SHOW INDEXES must list the session-created index, got: {stdout}"
    );
    // No on-disk store: no layout/codec/checkpoint lines.
    assert!(
        info_value(&stdout, "layout:").is_none(),
        ":memory: must not print store metadata, got: {stdout}"
    );
}

#[test]
fn test_dot_info_persistent_store_prints_manifest_and_live_counts() {
    let home = tempdir();
    let db_path = home.path().join("info_db");
    let db = db_path.to_str().unwrap();

    // Populate and index in one invocation; the exit checkpoint writes the
    // manifest the second invocation's `.info` reads cold.
    let (status, stdout, stderr) = run_cli_in_home(
        &[
            "--database",
            db,
            "CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'}); \
             CREATE INDEX person_name FOR (n:Person) ON (n.name);",
        ],
        home.path(),
    );
    assert!(status.success(), "populate failed: {stdout} {stderr}");

    let (status, stdout, stderr) =
        run_cli_stdin_in_home(&["query", "--database", db], home.path(), b".info\n");
    assert!(status.success(), ".info failed: {stderr}");
    assert_eq!(
        info_value(&stdout, "layout:").as_deref(),
        Some("native directory"),
        "store layout, got: {stdout}"
    );
    assert!(
        info_value(&stdout, "codec:").is_some(),
        "snapshot codec line, got: {stdout}"
    );
    assert!(
        info_value(&stdout, "format version:").is_some(),
        "format version line, got: {stdout}"
    );
    // The store block shares its wording with `relate info`
    // (store_info_render::InfoReport); the command tests above pin the same
    // labels.
    assert!(
        info_value(&stdout, "total bytes:").is_some_and(|v| v.parse::<u64>().is_ok()),
        "total size line, got: {stdout}"
    );
    assert!(
        info_value(&stdout, "checkpoint LSN:").is_some(),
        "checkpoint LSN line, got: {stdout}"
    );
    assert!(
        info_value(&stdout, "commit timestamp:").is_some_and(|v| v.parse::<u64>().is_ok()),
        "commit timestamp line, got: {stdout}"
    );
    assert!(
        info_value(&stdout, "wal segments past checkpoint:")
            .is_some_and(|v| v.parse::<u64>().is_ok()),
        "WAL segments line, got: {stdout}"
    );
    // Exact live counts via the open connection, not checkpoint-time sums.
    assert_eq!(
        info_value(&stdout, "nodes:").as_deref(),
        Some("2"),
        "live node count, got: {stdout}"
    );
    assert_eq!(
        info_value(&stdout, "relationships:").as_deref(),
        Some("1"),
        "live relationship count, got: {stdout}"
    );
    assert!(
        stdout.contains("person_name"),
        "catalog must list the persisted index, got: {stdout}"
    );
}

#[test]
fn test_dot_info_persistent_store_before_first_checkpoint() {
    // A store created by this very session has no manifest.json yet; `.info`
    // reports that state instead of erroring, and counts stay live/exact.
    let home = tempdir();
    let db_path = home.path().join("fresh_db");
    let db = db_path.to_str().unwrap();
    let script = "CREATE (:T);\n.info\n";
    let (status, stdout, stderr) =
        run_cli_stdin_in_home(&["query", "--database", db], home.path(), script.as_bytes());
    assert!(status.success(), ".info on fresh store failed: {stderr}");
    let snapshot = info_value(&stdout, "snapshot:").expect("snapshot line");
    assert!(
        snapshot.contains("no checkpoint"),
        "fresh store must report the missing checkpoint, got: {snapshot}"
    );
    assert_eq!(
        info_value(&stdout, "nodes:").as_deref(),
        Some("1"),
        "live count includes the un-checkpointed write, got: {stdout}"
    );
}

#[test]
fn test_dot_info_file_uri_session_reads_the_store_block() {
    // A `file://` URI opens fine (parse_uri_inner strips the scheme); `.info`'s
    // cold store read must strip it the same way instead of reporting the URI
    // as a missing path.
    let home = tempdir();
    let db_path = home.path().join("uri_db");
    let db = db_path.to_str().unwrap();
    let (status, stdout, stderr) = run_cli_in_home(&["--database", db, "CREATE (:T)"], home.path());
    assert!(status.success(), "populate failed: {stdout} {stderr}");

    let uri = format!("file://{db}");
    let (status, stdout, stderr) =
        run_cli_stdin_in_home(&["query", "--database", &uri], home.path(), b".info\n");
    assert!(
        status.success(),
        ".info over a file:// URI failed: {stderr}"
    );
    assert_eq!(
        info_value(&stdout, "layout:").as_deref(),
        Some("native directory"),
        "store block must read beside the file:// session, got: {stdout}"
    );
    assert!(
        info_value(&stdout, "checkpoint LSN:").is_some(),
        "checkpoint LSN line, got: {stdout}"
    );
}

#[test]
fn test_dot_info_session_lists_index_and_constraint_rows() {
    // The catalog half of `.info` is SHOW INDEXES / SHOW CONSTRAINTS through
    // the open connection, so rows created earlier in the same session show
    // up; a UNIQUE constraint also lists its auto-created backing index.
    let home = tempdir();
    let script =
        "CREATE (:Person {id: 1, name: 'ada'})-[:KNOWS]->(:Person {id: 2, name: 'bob'});\n\
                  CREATE INDEX person_name FOR (n:Person) ON (n.name);\n\
                  CREATE CONSTRAINT person_id_unique FOR (n:Person) REQUIRE n.id IS UNIQUE;\n\
                  .info\n";
    let (status, stdout, stderr) =
        run_cli_stdin_in_home(&["query"], home.path(), script.as_bytes());
    assert!(status.success(), ".info failed: {stderr}");
    assert_eq!(
        info_value(&stdout, "nodes:").as_deref(),
        Some("2"),
        "live node count, got: {stdout}"
    );
    assert_eq!(
        info_value(&stdout, "relationships:").as_deref(),
        Some("1"),
        "live relationship count, got: {stdout}"
    );
    // Piped sessions render the catalog rows as JSONL (ADR-0107).
    let after_indexes = stdout.split_once("indexes:\n").expect("indexes heading").1;
    let (indexes_section, constraints_section) = after_indexes
        .split_once("constraints:\n")
        .expect("constraints heading");
    assert!(
        indexes_section.contains(r#""name":"person_name""#),
        "SHOW INDEXES row for the session-created index, got: {stdout}"
    );
    assert!(
        indexes_section.contains(r#""name":"person_id_unique""#),
        "SHOW INDEXES row for the constraint's backing index, got: {stdout}"
    );
    assert!(
        constraints_section.contains(r#""name":"person_id_unique""#),
        "SHOW CONSTRAINTS row for the session-created constraint, got: {stdout}"
    );
    assert!(
        constraints_section.contains(r#""type":"NODE_PROPERTY_UNIQUENESS""#),
        "constraint row kind, got: {stdout}"
    );
}

#[test]
fn test_dot_help_lists_info() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli_stdin_in_home(&["query"], home.path(), b".help\n");
    assert!(status.success(), ".help failed: {stderr}");
    assert!(
        stdout.contains(".info"),
        ".help must list .info, got: {stdout}"
    );
}
