//! ADR-0682: the terminal `RETURN` hands its batches to the caller's
//! consumer.
//!
//! `Connection::query_streaming` runs a statement with a batch consumer:
//! the columns once, then bounded batches, and the summary after the last.
//! A map-each-row terminal streams — two million rows answer under a pool
//! that cannot hold them — while the materialized shapes (a `UNION`, a
//! breaker, a write, `EXPLAIN`, a catalog command) replay through the same
//! consumer and answer exactly what `query` answers. A consumer answering
//! `Stop` ends the walk and cancels the statement.

mod common;

use relate::{BatchFlow, CancelToken, StreamEvent, Value};

/// The workspace budget the streaming pin runs under: generous for a few
/// batches, impossible for the two-million-row answer collected.
const BUDGET: u64 = 16 << 20;

/// Collect a `query_streaming` answer: the announced columns and every
/// delivered row, beside the summary the call returns.
fn collect(
    conn: &mut relate::Connection,
    statement: &str,
) -> (Vec<String>, Vec<Vec<Value>>, relate::QueryResult) {
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    let summary = conn
        .query_streaming(statement, Default::default(), CancelToken::new(), |event| {
            match event {
                StreamEvent::Columns(announced) => columns = announced,
                StreamEvent::Batch(batch) => rows.extend(batch),
            }
            Ok(BatchFlow::Continue)
        })
        .unwrap();
    (columns, rows, summary)
}

/// Two million generated rows stream to the consumer under the 16 MiB pool
/// that the collected answer could never fit: the terminal pulls the next
/// batch only after the consumer returned from the previous one, so the
/// statement's peak is a few batches, not the answer. This is the
/// ADR-0682 memory proof — the consumer retains nothing, so nothing
/// input-proportional is ever held.
#[test]
fn a_streaming_terminal_answers_two_million_rows_under_a_small_pool() {
    let mut c = common::workspace_capped(BUDGET);
    let mut columns = Vec::new();
    let mut seen = 0usize;
    let mut batches = 0usize;
    let summary = c
        .query_streaming(
            "UNWIND range(1, 2_000_000) AS i RETURN i",
            Default::default(),
            CancelToken::new(),
            |event| {
                match event {
                    StreamEvent::Columns(announced) => columns = announced,
                    StreamEvent::Batch(batch) => {
                        batches += 1;
                        seen += batch.len();
                    }
                }
                Ok(BatchFlow::Continue)
            },
        )
        .unwrap();
    assert_eq!(columns, ["i"]);
    assert_eq!(seen, 2_000_000);
    assert!(batches > 1, "the answer arrived in bounded batches");
    assert!(summary.rows.is_empty(), "the consumer has the rows");
    assert_eq!(summary.header.columns, ["i"]);
    assert_eq!(summary.header.column_types, ["INTEGER"]);
}

/// The materialized shapes replay through the consumer and answer exactly
/// what `query` answers: a `UNION ALL`, a distinct `UNION`, an unlimited
/// `ORDER BY`, an aggregation and a `USE`-routed statement.
#[test]
fn the_materialized_shapes_answer_what_query_answers() {
    let mut c = common::connection();
    common::run(
        &mut c,
        "UNWIND range(1, 100) AS i CREATE (:N {i: i, m: i % 7})",
    );
    let statements = [
        "MATCH (n:N) RETURN n.i AS i UNION ALL MATCH (n:N) RETURN n.i + 1000 AS i",
        "MATCH (n:N) RETURN n.m AS m UNION MATCH (n:N) RETURN n.m AS m",
        "MATCH (n:N) RETURN n.i AS i ORDER BY i DESC",
        "MATCH (n:N) RETURN n.m AS m, count(n) AS c ORDER BY m",
        // A `USE` lands: its namespace is known only from the result.
        "USE neo4j MATCH (n:N) RETURN n.i AS i",
    ];
    for statement in statements {
        let expected = common::run(&mut c, statement);
        let (columns, rows, summary) = collect(&mut c, statement);
        assert_eq!(columns, expected.header.columns, "{statement}");
        assert_eq!(rows, expected.rows, "{statement}");
        assert!(summary.rows.is_empty(), "{statement}");
        assert_eq!(summary.header.column_types, expected.header.column_types);
    }
}

/// A consumer answering `Stop` ends the walk and cancels the statement:
/// the error is the cancellation contract's, the generator produced
/// nothing further, and the connection stays usable.
#[test]
fn a_stop_answer_ends_the_walk_and_cancels_the_statement() {
    let mut c = common::connection();
    let mut delivered = 0usize;
    let error = c
        .query_streaming(
            "UNWIND range(1, 1_000_000) AS i RETURN i",
            Default::default(),
            CancelToken::new(),
            |event| {
                if let StreamEvent::Batch(batch) = event {
                    delivered += batch.len();
                    return Ok(BatchFlow::Stop);
                }
                Ok(BatchFlow::Continue)
            },
        )
        .expect_err("a stopped consumer cancels the statement");
    assert_eq!(error.to_string(), "query cancelled");
    assert!(delivered <= 1024, "the walk ended at the first batch");
    let after = common::run(&mut c, "RETURN 1 AS one");
    assert_eq!(after.rows, vec![vec![Value::Int(1)]]);
}

/// A write, an `EXPLAIN` and a catalog command keep the materialized path:
/// the consumer still sees columns then batches, and the statement's
/// effects and plan land exactly as `query`'s.
#[test]
fn writes_explain_and_catalog_commands_replay_through_the_consumer() {
    let mut c = common::connection();

    // A write with a RETURN: the rows replay after the run; the write
    // applied exactly as `query`'s.
    let (columns, rows, summary) = collect(&mut c, "CREATE (:W {i: 1}) RETURN 1 AS one");
    assert_eq!(columns, ["one"]);
    assert_eq!(rows, vec![vec![Value::Int(1)]]);
    assert_eq!(summary.stats.nodes_created, 1);
    assert!(summary.bookmark.is_some(), "the write committed");
    let stored = common::run(&mut c, "MATCH (w:W) RETURN w.i AS i");
    assert_eq!(stored.rows, vec![vec![Value::Int(1)]]);

    // EXPLAIN produces no rows and carries the plan, as `query`'s does.
    let expected = common::run(&mut c, "EXPLAIN MATCH (n:W) RETURN n");
    let (columns, rows, summary) = collect(&mut c, "EXPLAIN MATCH (n:W) RETURN n");
    assert_eq!(columns, expected.header.columns);
    assert!(rows.is_empty());
    assert!(summary.plan.is_some());

    // A catalog command: same rows as `query`.
    common::run(&mut c, "CREATE INDEX w_i FOR (w:W) ON (w.i)");
    let expected = common::run(&mut c, "SHOW INDEXES");
    let (columns, rows, _) = collect(&mut c, "SHOW INDEXES");
    assert_eq!(columns, expected.header.columns);
    assert_eq!(rows, expected.rows);
}
