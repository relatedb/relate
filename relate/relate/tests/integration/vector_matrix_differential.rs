//! The packed vector matrix (ADR-0705) answers every vector SEARCH it
//! serves exactly as the `Value` scan does: the same ids, in the same
//! order, with bit-identical scores. The oracle is the `Value` scan itself:
//! the same statement inside a transaction that holds an unrelated
//! uncommitted write, which the matrix never serves. A seeded walk of
//! writes — creates, vector rewrites, unrelated property writes, label
//! drops and re-adds, property removals, wrong-length and FLOAT64 vectors,
//! deletes, committed multi-write transactions — checks both paths after
//! every step, filtered and unfiltered, and a reader pinned at an older
//! snapshot keeps its own answer while a newer reader refreshes the matrix
//! past it. The `test-hooks` served count proves the matrix side ran.

use std::collections::BTreeMap;

use relate::{Connection, Database, Value};
use relate_executor::take_vector_matrix_serves;
use relate_test_support::rng::Rng;

use crate::common;

const DIM: usize = 16;

/// Each test indexes its own label: the serve count is process-wide and
/// name-keyed (the canonical `SharedCounter` — the facade answers queries
/// on a spawned `QueryWorker` thread, so the bump and the read never
/// share one), so the label keeps concurrently running tests' counts
/// separate.
const WALK: &str = "MatrixWalkDoc";
const PIN: &str = "MatrixPinDoc";
const BEHIND: &str = "MatrixBehindDoc";

/// A coordinate exactly representable as f32 (what a FLOAT32 vector
/// stores), in [-1, 1) — the one bespoke part of the stream.
fn coord(rng: &mut Rng) -> f64 {
    f64::from((rng.below(1 << 16) as f32 / 32768.0) - 1.0)
}

fn vector(rng: &mut Rng, dim: usize) -> Value {
    Value::List(
        (0..dim)
            .map(|_| Value::Float(coord(rng)))
            .collect::<Vec<_>>()
            .into(),
    )
}

fn group(rng: &mut Rng) -> Value {
    Value::String(if rng.below(2) == 0 { "a" } else { "b" }.into())
}

struct Statements {
    label: &'static str,
}

impl Statements {
    fn index(&self) -> String {
        format!(
            "CREATE VECTOR INDEX doc_e IF NOT EXISTS FOR (n:{}) ON (n.emb) WITH [n.grp] \
             OPTIONS {{indexConfig: {{`vector.dimensions`: {DIM}, \
             `vector.similarity_function`: 'cosine'}}}}",
            self.label
        )
    }

    fn create(&self) -> String {
        format!(
            "CREATE (:{} {{id: $id, grp: $g, hits: 0, emb: vector($v, {DIM}, FLOAT32)}})",
            self.label
        )
    }

    fn unfiltered(&self) -> String {
        format!(
            "MATCH (n:{}) SEARCH n IN (VECTOR INDEX doc_e FOR $q LIMIT $k) SCORE AS s \
             RETURN n.id AS id, s",
            self.label
        )
    }

    fn filtered(&self) -> String {
        format!(
            "MATCH (n:{}) \
             SEARCH n IN (VECTOR INDEX doc_e FOR $q WHERE n.grp = $g LIMIT $k) SCORE AS s \
             RETURN n.id AS id, s",
            self.label
        )
    }
}

fn open(s: &Statements) -> Database {
    let db = Database::open(":memory:").unwrap();
    db.connect().unwrap().query(&s.index()).unwrap();
    db
}

/// `(id, score bits)` rows, in answer order.
fn answer(c: &mut Connection, statement: &str, p: &BTreeMap<String, Value>) -> Vec<(i64, u64)> {
    c.query_with_params(statement, p.clone())
        .unwrap()
        .rows
        .iter()
        .map(|row| match (&row[0], &row[1]) {
            (Value::Int(id), Value::Float(s)) => (*id, s.to_bits()),
            other => panic!("a SEARCH row yields an int id and a float score, got {other:?}"),
        })
        .collect()
}

/// The `Value` scan's answer at `c`'s current snapshot: an unrelated
/// uncommitted write makes the matrix decline, then the transaction rolls
/// back.
fn oracle(c: &mut Connection, statement: &str, p: &BTreeMap<String, Value>) -> Vec<(i64, u64)> {
    c.begin().unwrap();
    c.query("CREATE (:Scratch)").unwrap();
    let rows = answer(c, statement, p);
    c.rollback().unwrap();
    rows
}

fn assert_paths_agree(s: &Statements, c: &mut Connection, rng: &mut Rng, step: usize) {
    for k in [1i64, 5, 50] {
        let q = vector(&mut *rng, DIM);
        let unfiltered = common::params(&[("q", q.clone()), ("k", Value::Int(k))]);
        assert_eq!(
            answer(c, &s.unfiltered(), &unfiltered),
            oracle(c, &s.unfiltered(), &unfiltered),
            "unfiltered step={step} k={k}"
        );
        let filtered = common::params(&[("q", q), ("k", Value::Int(k)), ("g", group(&mut *rng))]);
        assert_eq!(
            answer(c, &s.filtered(), &filtered),
            oracle(c, &s.filtered(), &filtered),
            "filtered step={step} k={k}"
        );
    }
}

#[test]
fn matrix_answers_match_the_value_scan_through_a_seeded_write_walk() {
    let s = Statements { label: WALK };
    let db = open(&s);
    let mut c = db.connect().unwrap();
    let mut rng = Rng(0x5eed_0fad_1200);
    let mut next_id = 0i64;
    let create = |c: &mut Connection, rng: &mut Rng, next_id: &mut i64| {
        let p = common::params(&[
            ("id", Value::Int(*next_id)),
            ("v", vector(&mut *rng, DIM)),
            ("g", group(&mut *rng)),
        ]);
        c.query_with_params(&s.create(), p).unwrap();
        *next_id += 1;
    };
    for _ in 0..40 {
        create(&mut c, &mut rng, &mut next_id);
    }
    take_vector_matrix_serves(WALK);
    let steps = 150;
    for step in 0..steps {
        let id = Value::Int(rng.below(next_id as u64) as i64);
        let by_id = |id: &Value| common::params(&[("id", id.clone())]);
        match rng.below(12) {
            0 | 1 => create(&mut c, &mut rng, &mut next_id),
            2 | 3 => {
                c.query_with_params(
                    &format!("MATCH (n {{id: $id}}) SET n.emb = vector($v, {DIM}, FLOAT32)"),
                    common::params(&[("id", id), ("v", vector(&mut rng, DIM))]),
                )
                .unwrap();
            }
            4 | 5 => {
                // An unrelated property write: one dirty row, same vector.
                c.query_with_params("MATCH (n {id: $id}) SET n.hits = n.hits + 1", by_id(&id))
                    .unwrap();
            }
            6 => {
                c.query_with_params(
                    &format!("MATCH (n:{WALK} {{id: $id}}) REMOVE n:{WALK}"),
                    by_id(&id),
                )
                .unwrap();
            }
            7 => {
                c.query_with_params(&format!("MATCH (n {{id: $id}}) SET n:{WALK}"), by_id(&id))
                    .unwrap();
            }
            8 => {
                // A wrong length (both paths skip it) or a removed property.
                let statement = if rng.below(2) == 0 {
                    "MATCH (n {id: $id}) SET n.emb = vector($v, 8, FLOAT32)"
                } else {
                    "MATCH (n {id: $id}) REMOVE n.emb"
                };
                c.query_with_params(
                    statement,
                    common::params(&[("id", id), ("v", vector(&mut rng, 8))]),
                )
                .unwrap();
            }
            9 => {
                c.query_with_params("MATCH (n {id: $id}) DETACH DELETE n", by_id(&id))
                    .unwrap();
            }
            10 => {
                // A FLOAT64 vector the matrix cannot pack exactly: it
                // declines while the vector exists. Rewritten right after
                // the check so the walk keeps exercising the matrix.
                c.query_with_params(
                    &format!("MATCH (n {{id: $id}}) SET n.emb = vector($v, {DIM}, FLOAT64)"),
                    common::params(&[("id", id.clone()), ("v", vector(&mut rng, DIM))]),
                )
                .unwrap();
                assert_paths_agree(&s, &mut c, &mut rng, step);
                c.query_with_params(
                    &format!("MATCH (n {{id: $id}}) SET n.emb = vector($v, {DIM}, FLOAT32)"),
                    common::params(&[("id", id), ("v", vector(&mut rng, DIM))]),
                )
                .unwrap();
            }
            _ => {
                // A committed explicit transaction with several writes.
                c.begin().unwrap();
                c.query_with_params(
                    &format!(
                        "MATCH (n {{id: $id}}) SET n.emb = vector($v, {DIM}, FLOAT32), n.hits = 1"
                    ),
                    common::params(&[("id", id), ("v", vector(&mut rng, DIM))]),
                )
                .unwrap();
                create(&mut c, &mut rng, &mut next_id);
                c.commit().unwrap();
            }
        }
        assert_paths_agree(&s, &mut c, &mut rng, step);
    }
    // Six matrix-eligible searches per step; only the FLOAT64 steps decline.
    let served = take_vector_matrix_serves(WALK);
    assert!(
        served >= (steps * 6 / 2) as u64,
        "the matrix served {served} of {} searches",
        steps * 6
    );
}

#[test]
fn a_pinned_reader_keeps_its_snapshot_after_a_newer_reader_refreshes_the_matrix() {
    let s = Statements { label: PIN };
    let db = open(&s);
    let mut writer = db.connect().unwrap();
    let mut pinned = db.connect().unwrap();
    let mut fresh = db.connect().unwrap();
    let mut rng = Rng(0xfeed);
    for id in 0..30 {
        writer
            .query_with_params(
                &s.create(),
                common::params(&[
                    ("id", Value::Int(id)),
                    ("v", vector(&mut rng, DIM)),
                    ("g", group(&mut rng)),
                ]),
            )
            .unwrap();
    }
    let q = common::params(&[("q", vector(&mut rng, DIM)), ("k", Value::Int(10))]);
    take_vector_matrix_serves(PIN);
    // Build the matrix, then pin a reader at this snapshot. A read-only
    // explicit transaction has no pending writes, so the matrix serves it.
    let before = answer(&mut fresh, &s.unfiltered(), &q);
    pinned.begin().unwrap();
    assert_eq!(answer(&mut pinned, &s.unfiltered(), &q), before);
    assert_eq!(
        take_vector_matrix_serves(PIN),
        2,
        "both reads were matrix reads"
    );

    // Rewrite every vector.
    for id in 0..30 {
        writer
            .query_with_params(
                &format!("MATCH (n {{id: $id}}) SET n.emb = vector($v, {DIM}, FLOAT32)"),
                common::params(&[("id", Value::Int(id)), ("v", vector(&mut rng, DIM))]),
            )
            .unwrap();
    }
    // A reader at the new snapshot refreshes the matrix past the pin.
    let after = answer(&mut fresh, &s.unfiltered(), &q);
    assert_eq!(after, oracle(&mut fresh, &s.unfiltered(), &q));
    assert_ne!(after, before, "the rewrite changed the answer");

    // The pinned reader still answers from its own snapshot, through the
    // `Value` scan: the matrix never rewinds.
    take_vector_matrix_serves(PIN);
    assert_eq!(answer(&mut pinned, &s.unfiltered(), &q), before);
    assert_eq!(
        take_vector_matrix_serves(PIN),
        0,
        "the pinned read declined"
    );
    pinned.commit().unwrap();
    assert_eq!(answer(&mut pinned, &s.unfiltered(), &q), after);
}

/// ADR-0705: a first build declines while the store's head is past the
/// reader's snapshot. A write that landed before the slot was registered
/// was never dirty-marked, so without the decline the pinned reader's
/// build at its older snapshot would publish rows stale for every later
/// reader — the fresh search below would miss the carrier committed
/// between the pin and the first search.
#[test]
fn a_first_build_behind_the_head_declines_instead_of_serving_stale_rows() {
    let s = Statements { label: BEHIND };
    let db = open(&s);
    let mut writer = db.connect().unwrap();
    let mut pinned = db.connect().unwrap();
    let mut rng = Rng(0x5ee5);
    for id in 0..30 {
        writer
            .query_with_params(
                &s.create(),
                common::params(&[
                    ("id", Value::Int(id)),
                    ("v", vector(&mut rng, DIM)),
                    ("g", group(&mut rng)),
                ]),
            )
            .unwrap();
    }
    let q = common::params(&[("q", vector(&mut rng, DIM)), ("k", Value::Int(10))]);
    // Pin a reader at the head, then commit one more carrier behind its
    // back — carrying the query's own vector, so it places first for
    // every reader that can see it. Its write hook ran while the matrix
    // registry was still empty, so it was never dirty-marked.
    pinned.begin().unwrap();
    writer
        .query_with_params(
            &s.create(),
            common::params(&[
                ("id", Value::Int(30)),
                ("v", q.get("q").unwrap().clone()),
                ("g", group(&mut rng)),
            ]),
        )
        .unwrap();
    // The pinned search is the first search: it must decline to build
    // behind the head — the `Value` scan answers it.
    let before = answer(&mut pinned, &s.unfiltered(), &q);
    assert_eq!(before.len(), 10);
    assert!(
        !before.iter().any(|(id, _)| *id == 30),
        "the pin predates the new carrier"
    );
    assert_eq!(
        take_vector_matrix_serves(BEHIND),
        0,
        "no matrix served the pinned first search"
    );
    // The fresh reader at the head builds a matrix that carries the new
    // carrier, so no later reader is served the pinned reader's stale
    // rows.
    let after = answer(&mut writer, &s.unfiltered(), &q);
    assert!(
        after.iter().any(|(id, _)| *id == 30),
        "the fresh read sees the new carrier"
    );
    assert_eq!(after, oracle(&mut writer, &s.unfiltered(), &q));
    assert!(
        take_vector_matrix_serves(BEHIND) >= 1,
        "the fresh read was a matrix read"
    );
    pinned.rollback().unwrap();
}
