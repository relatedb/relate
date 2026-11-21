//! ADR-0641: a `LOAD CSV` seeding a per-row-independent tail runs
//! batch-major inside one transaction — the generator's records are pulled
//! in bounded batches and the tail runs once per batch, so the inter-clause
//! boundary never holds the whole load.
//!
//! The discriminator is one shared workspace budget and two outcomes at it:
//! the admitted `MATCH* CREATE` shape answers (its live charge is one batch
//! of records plus the transaction's redo), while the shapes the classifier
//! declines keep the holding path and refuse — the boundary plus the redo
//! overflows the budget. The refused shapes are the classifier's own
//! declines, so a regression that sent the admitted shape down the holding
//! path would refuse too and fail the answering test: engagement of the
//! batched driver is pinned behaviourally, not by a counter.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use relate::{Database, OpenOptions, Value};

/// The workspace budget every test shares, measured to sit between the two
/// costs of a `RECORDS`-record load: the batched shape answers from ~24 MiB
/// up (its live charge is one batch of records plus the transaction's redo),
/// while the held inter-clause boundary — the redo plus every record's
/// composed row — refuses past 64 MiB. 32 MiB is clear of both edges.
const BUDGET: u64 = 32 << 20;

/// Records per load: enough that the held boundary is a multiple of the
/// budget.
const RECORDS: i64 = 40_000;

/// Users the CSV's endpoints index into.
const USERS: i64 = 500;

/// The bulk-load statement: a generator seeding `MATCH* CREATE`, the
/// disjoint-label shape the classifier admits.
const LOAD: &str = "LOAD CSV WITH HEADERS FROM $src AS row \
                    MATCH (a:User {id: row.src}) \
                    MATCH (b:User {id: row.dst}) \
                    CREATE (a)-[:REL]->(b)";

fn open(root: &Path) -> relate::Connection {
    let db = Database::open_with(
        ":memory:",
        OpenOptions {
            load_csv_dir: Some(root.to_path_buf()),
            load_csv_max_bytes: Some(64 << 20),
            concurrent_logical_workspace_limit: Some(BUDGET),
            mode: relate::OpenMode::Writable,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    db.connect().unwrap()
}

/// A fresh connection seeded with `USERS` indexed users and a `RECORDS`-row
/// CSV, beside that CSV's URL. Each row carries a 1 KiB `pad`, so a composed
/// row's width is the CSV record's — the pokec load's real shape, where the
/// held boundary is the record payload and dwarfs the transaction's redo.
fn seeded(dir: &Path) -> (relate::Connection, String) {
    let mut c = open(dir);
    common::run(
        &mut c,
        &format!(
            "UNWIND range(0, {}) AS i CREATE (:User {{id: toString(i)}})",
            USERS - 1
        ),
    );
    common::run(&mut c, "CREATE INDEX user_id FOR (u:User) ON (u.id)");
    let path = dir.join("rels.csv");
    let pad = "x".repeat(1024);
    let mut text = String::from("src,dst,pad\n");
    for r in 0..RECORDS {
        text.push_str(&format!("{},{},{}\n", r % USERS, (r * 7 + 1) % USERS, pad));
    }
    fs::write(&path, text).unwrap();
    let src = common::local_file_url(&path);
    (c, src)
}

/// The one parameter every statement here binds: the CSV source URL.
fn params(src: &str) -> BTreeMap<String, Value> {
    common::params(&[("src", Value::String(src.into()))])
}

fn run_load(c: &mut relate::Connection, src: &str) -> Result<relate::QueryResult, relate::Error> {
    c.query_with_params(LOAD, params(src))
}

/// Assert the statement refuses under the shared budget with the workspace
/// charge — the holding path's signature.
fn assert_refused(c: &mut relate::Connection, src: &str, query: &str) {
    let error = c
        .query_with_params(query, params(src))
        .expect_err("the holding path refuses under the shared budget");
    assert!(
        error
            .to_string()
            .contains("concurrent-logical-workspace-limit"),
        "the refusal is the workspace charge, not another error: {error}"
    );
}

/// Assert `query` is admitted and creates `expected` nodes labelled
/// `label` — the admitted-tail twin of [`assert_refused`].
fn assert_admitted(c: &mut relate::Connection, src: &str, query: &str, label: &str, expected: i64) {
    c.query_with_params(query, params(src))
        .expect("the admitted tail answers under the shared budget");
    let count = common::first_column(&common::run(
        c,
        &format!("MATCH (w:{label}) RETURN count(w)"),
    ));
    assert_eq!(count, vec![Value::Int(expected)]);
}

fn rel_count(c: &mut relate::Connection) -> Vec<Value> {
    common::first_column(&common::run(c, "MATCH ()-[r:REL]->() RETURN count(r)"))
}

/// The batched load answers under the shared budget and creates exactly its
/// record count in relationships. The two refusal tests below run the same
/// family under the same budget and refuse — so this is the discriminator:
/// the boundary is never held, only one batch of records and the redo are.
#[test]
fn a_bulk_load_answers_under_a_budget_the_held_path_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, src) = seeded(dir.path());
    run_load(&mut c, &src).expect("the batched load answers under the shared budget");
    assert_eq!(rel_count(&mut c), vec![Value::Int(RECORDS)]);
}

/// A terminal `RETURN` is ineligible — the page is the whole point of
/// holding — so the statement keeps the ordinary path and its charge, which
/// refuses under the shared budget.
#[test]
fn a_terminal_return_keeps_the_holding_path() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, src) = seeded(dir.path());
    assert_refused(&mut c, &src, &format!("{LOAD} RETURN row.src AS src"));
}

/// A `CREATE` whose label the read patterns also match is ineligible — a
/// later batch's read must not see an earlier batch's write — so it keeps
/// the ordinary path and refuses under the shared budget. The `CREATE`
/// carries no property, so the canonical admission does not decline it
/// first: the disjointness gate is what refuses it.
#[test]
fn a_create_matching_the_read_label_keeps_the_holding_path() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, src) = seeded(dir.path());
    let query = "LOAD CSV WITH HEADERS FROM $src AS row \
                 MATCH (a:User {id: row.src}) \
                 CREATE (:User)";
    assert_refused(&mut c, &src, query);
}

/// A `CREATE` whose label no read pattern names is admitted — the
/// disjointness gate is a boundary, not a blanket refusal.
#[test]
fn a_disjoint_label_create_is_admitted() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, src) = seeded(dir.path());
    assert_admitted(
        &mut c,
        &src,
        "LOAD CSV WITH HEADERS FROM $src AS row \
         MATCH (a:User {id: row.src}) \
         CREATE (:Widget)",
        "Widget",
        RECORDS,
    );
}

/// A zero-`MATCH` tail is admitted: with no read pattern there is nothing a
/// later batch's read could see.
#[test]
fn a_zero_match_tail_is_admitted() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, src) = seeded(dir.path());
    assert_admitted(
        &mut c,
        &src,
        "LOAD CSV WITH HEADERS FROM $src AS row CREATE (:Widget)",
        "Widget",
        RECORDS,
    );
}
