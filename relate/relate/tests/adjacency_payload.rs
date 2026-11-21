//! The opt-in adjacency payload through the public surface (ADR-0551): a
//! store checkpointed with it answers a Cypher workload exactly like the
//! same store re-folded from the relationship rows.
//!
//! The whole file needs the Parquet codec: the payload only exists beside a
//! Parquet checkpoint.

#![cfg(feature = "parquet")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::rows;
use relate::{Database, OpenOptions, Value};
use relate_storage::{ADJACENCY_PAYLOAD_ENV, ADJACENCY_PAYLOAD_MIN_RELS_ENV};
use relate_test_support::{EnvLock, EnvRestore};

/// Open with the periodic checkpoint off: the test checkpoints explicitly.
fn open(uri: &str) -> Database {
    Database::open_with(
        uri,
        OpenOptions {
            checkpoint_interval: Some(Duration::ZERO),
            ..OpenOptions::default()
        },
    )
    .unwrap()
}

/// The store's own target handle, for the storage layer's manifest and
/// object seams; the handle's writer lease must be released before a
/// `Database` opens the same directory.
fn storage(uri: &str) -> relate_storage::StoreTarget {
    relate_storage::parse_uri_with_options(uri, None, None).unwrap()
}

/// The workload's answers: neighbour counts in each direction, both
/// degrees, a one-hop neighbor list and a two-hop pattern match.
fn answers(db: &Database) -> Vec<Vec<Vec<Value>>> {
    let mut conn = db.connect().unwrap();
    [
        "MATCH (a:N {i: 0})-[r:LINK]->() RETURN count(r) AS n",
        "MATCH (a:N {i: 0})<-[r:LINK]-() RETURN count(r) AS n",
        "MATCH (a:N {i: 0})-[r]-() RETURN count(r) AS n",
        "MATCH (a:N {i: 0})-[r:LINK]->(b) RETURN b.i AS neighbor ORDER BY neighbor",
        "MATCH (a:N)-[:LINK]->(b:N)<-[:LINK]-(c:N) RETURN a.i, b.i, c.i",
    ]
    .into_iter()
    .map(|query| rows(&mut conn, query))
    .collect()
}

/// The workload's graph: three nodes and the relationships both directions
/// of the same pair carry, plus a second rel type.
fn seed(db: &Database) {
    let mut conn = db.connect().unwrap();
    for query in [
        "CREATE (:N {i: 0}), (:N {i: 1}), (:N {i: 2})",
        "MATCH (a:N {i: 0}), (b:N {i: 1}) CREATE (a)-[:LINK {w: 1}]->(b), \
         (b)-[:LINK {w: 2}]->(a), (a)-[:LINK {w: 3}]->(b)",
        "MATCH (a:N {i: 1}), (b:N {i: 2}) CREATE (a)-[:LINK]->(b)",
        "MATCH (a:N {i: 2}), (b:N {i: 0}) CREATE (a)-[:LINK]->(b)",
        "MATCH (a:N {i: 2}), (b:N {i: 0}) CREATE (a)-[:OTHER]->(b)",
    ] {
        conn.query(query).unwrap();
    }
}

/// A store built with the payload opted in answers the workload like the
/// same store with its payload selection dropped (the row re-fold).
#[test]
fn a_payload_installed_open_answers_the_workload_like_the_refold() {
    let _env = EnvLock::acquire();
    let _payload = EnvRestore::new(ADJACENCY_PAYLOAD_ENV);
    let _min_rels = EnvRestore::new(ADJACENCY_PAYLOAD_MIN_RELS_ENV);
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().to_str().unwrap().to_owned();
    std::env::set_var(ADJACENCY_PAYLOAD_ENV, "1");
    std::env::set_var(ADJACENCY_PAYLOAD_MIN_RELS_ENV, "1");
    {
        let db = open(&uri);
        seed(&db);
        db.checkpoint().unwrap();
        db.leak_without_checkpoint();
    }

    // The checkpoint published the payload the next open installs.
    assert!(relate_storage::adjacency_payload_path(&storage(&uri))
        .unwrap()
        .is_some());

    let installed = {
        let db = open(&uri);
        let answers = answers(&db);
        db.leak_without_checkpoint();
        answers
    };

    // Drop the payload selection: the same store now re-folds the rows.
    relate_storage::drop_adjacency_payload_selection(&storage(&uri)).unwrap();
    let refolded = {
        let db = open(&uri);
        let answers = answers(&db);
        db.leak_without_checkpoint();
        answers
    };

    assert_eq!(installed, refolded);
}

/// The number of `LINK` relationships leaving node 0 — a type-resolved walk,
/// so it only counts the adjacency filed under the loading graph's `LINK` id.
fn link_count(db: &Database) -> i64 {
    let mut conn = db.connect().unwrap();
    let rows = rows(
        &mut conn,
        "MATCH (a:N {i: 0})-[r:LINK]->() RETURN count(r) AS n",
    );
    match rows[0][0] {
        Value::Int(count) => count,
        ref other => panic!("expected an integer count, got {other:?}"),
    }
}

/// A store whose writer and loader number `LINK` differently. The writer
/// interns `LINK` when it creates the relationships and only then interns the
/// fresh label `Z`, so the writer numbers `LINK` 1 and `Z` 2. A load interns
/// every node row's labels before any rel row's type (nodes load before
/// relationships), so the loader numbers `Z` 1 and `LINK` 2. A payload keyed
/// by the writer's type id would therefore file the chunks under the loader's
/// `Z` (a label id), and the type-resolved `LINK` walk would find nothing.
fn renumbered_seed(db: &Database) {
    let mut conn = db.connect().unwrap();
    for query in [
        "CREATE (:N {i: 0}), (:N {i: 1})",
        "MATCH (a:N {i: 0}), (b:N {i: 1}) CREATE (a)-[:LINK]->(b), (a)-[:LINK]->(b)",
        // The label the writer interns after `LINK`, and the loader before it.
        "CREATE (:Z {i: 2})",
    ] {
        conn.query(query).unwrap();
    }
}

/// The payload's chunks install under the loading graph's own type ids: a
/// load that renumbers `LINK` past the writer's id still answers every
/// type-resolved walk like the re-fold of the same store.
#[test]
fn a_payload_installs_under_the_loaders_rel_type_ids() {
    let _env = EnvLock::acquire();
    let _payload = EnvRestore::new(ADJACENCY_PAYLOAD_ENV);
    let _min_rels = EnvRestore::new(ADJACENCY_PAYLOAD_MIN_RELS_ENV);
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().to_str().unwrap().to_owned();
    std::env::set_var(ADJACENCY_PAYLOAD_ENV, "1");
    std::env::set_var(ADJACENCY_PAYLOAD_MIN_RELS_ENV, "1");
    {
        let db = open(&uri);
        renumbered_seed(&db);
        db.checkpoint().unwrap();
        db.leak_without_checkpoint();
    }
    assert!(relate_storage::adjacency_payload_path(&storage(&uri))
        .unwrap()
        .is_some());

    let installed = {
        let db = open(&uri);
        let count = link_count(&db);
        db.leak_without_checkpoint();
        count
    };

    relate_storage::drop_adjacency_payload_selection(&storage(&uri)).unwrap();
    let refolded = {
        let db = open(&uri);
        let count = link_count(&db);
        db.leak_without_checkpoint();
        count
    };

    assert_eq!(installed, 2, "the installed open lost the LINK chunks");
    assert_eq!(installed, refolded);
}

const CHILD_MODE: &str = "RELATE_PAYLOAD_CHILD";
const CHILD_PATH: &str = "RELATE_PAYLOAD_PATH";

/// How a child process treats the published payload and reopens the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Leave the payload whole and reopen writable.
    Installed,
    /// Replace the payload object with non-Parquet bytes and reopen
    /// writable; the open must degrade with a warning.
    Degraded,
    /// The same corruption, reopened read-only.
    DegradedReadonly,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Installed, Mode::Degraded, Mode::DegradedReadonly];

    fn label(self) -> &'static str {
        match self {
            Mode::Installed => "installed",
            Mode::Degraded => "degraded",
            Mode::DegradedReadonly => "degraded-readonly",
        }
    }

    fn parse(value: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.label() == value)
    }

    /// The mode's whole behaviour — the payload treatment and the open —
    /// in one exhaustive match.
    fn open(self, uri: &str) -> Database {
        match self {
            Mode::Installed => open(uri),
            Mode::Degraded => {
                corrupt_payload(uri);
                open(uri)
            }
            Mode::DegradedReadonly => {
                corrupt_payload(uri);
                Database::open_read_only(uri).unwrap()
            }
        }
    }
}

/// Replace the store's published payload object with bytes that are not a
/// Parquet file.
fn corrupt_payload(uri: &str) {
    let target = storage(uri);
    let path = relate_storage::adjacency_payload_path(&target)
        .unwrap()
        .expect("the checkpoint published a payload");
    relate_storage::atomic_write(&*target.vfs(), &path, b"not a parquet payload").unwrap();
}

/// The parent's window on one open's stderr: the child re-executes this
/// test binary for one open, so the warning that open emits lands in the
/// captured stream (libtest would swallow it without `--nocapture`).
fn child(path: &Path, mode: Mode) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("adjacency_payload_child")
        .arg("--nocapture")
        .env(CHILD_MODE, mode.label())
        .env(CHILD_PATH, path)
        .env(ADJACENCY_PAYLOAD_ENV, "1")
        .env(ADJACENCY_PAYLOAD_MIN_RELS_ENV, "1")
        .output()
        .unwrap()
}

/// One open in its own process: build the payload store, then reopen it
/// under `Mode`'s treatment.
#[test]
fn adjacency_payload_child() {
    let Some(mode) = std::env::var(CHILD_MODE)
        .ok()
        .and_then(|value| Mode::parse(&value))
    else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(CHILD_PATH).unwrap());
    let uri = dir.to_str().unwrap().to_owned();
    {
        let db = open(&uri);
        seed(&db);
        db.checkpoint().unwrap();
        db.leak_without_checkpoint();
    }
    let db = mode.open(&uri);
    db.leak_without_checkpoint();
}

/// The degrade warning reaches the caller on both shipped open paths: the
/// installed open is silent, and the degraded writable and read-only opens
/// each name the payload they re-folded around.
#[test]
fn the_payload_degrade_warning_reaches_both_open_paths() {
    let dir = tempfile::tempdir().unwrap();
    let stderr = |mode: Mode| {
        let output = child(&dir.path().join(mode.label()), mode);
        assert!(output.status.success(), "{mode:?}: {output:?}");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    assert!(!stderr(Mode::Installed).contains("ignoring adjacency payload"));
    assert!(stderr(Mode::Degraded).contains("ignoring adjacency payload"));
    assert!(stderr(Mode::DegradedReadonly).contains("ignoring adjacency payload"));
}
