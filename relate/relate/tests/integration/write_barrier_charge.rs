//! ADR-0660 + ADR-0463: the write barrier streams the output it produces.
//!
//! `SET` reads its driving rows through `RowStream::write_barrier`: the
//! input is pulled in bounded batches, each handed to the write as it
//! arrives, and the rows the write produces are emitted as the barrier's
//! own stream — one batch in flight, charged as it is held. A write whose
//! output outgrows the workspace pool therefore still answers: the pool
//! prices one batch, never the statement's whole output, and the page
//! stops the rows (running every remaining batch on `stop`).
//!
//! One stored node is driven from a generated range, so every driving row
//! transforms to exactly one output row while the write itself costs
//! nothing: `SET m.absent = null` removes nothing (the property is not
//! there). The output below is a few times the pool, so the statement
//! answering under it is the streamed barrier's signature — the same
//! statement refuses when a consumer collects the rows instead (the
//! `materialize` pin beside it).

use crate::common;

use relate::Value;

/// The workspace pool: room for the driving rows one batch at a time, none
/// for the whole output.
const BUDGET: u64 = 8 << 20;
/// The output that crosses [`BUDGET`]: two-slot rows at roughly 150 bytes
/// each, a few times the pool if any one container held them all.
const OUTPUT: i64 = 200_000;
/// The same statement's pool when its output fits.
const ROOMY: u64 = 64 << 20;
/// The collecting consumer's pool: the sort's peak carries the parked
/// `ORDER BY` key and the projection's materialized input beside the rows,
/// so it sits above [`ROOMY`].
const COLLECTING_ROOMY: u64 = 128 << 20;

/// `m` is the one stored row; the range generates the driving rows and the
/// `tail` picks the consumer — a page that streams (`LIMIT 1`) or a sort
/// that collects (`ORDER BY i`) over one otherwise identical statement.
fn statement(tail: &str) -> String {
    format!(
        "MATCH (m:Row) WITH m LIMIT 1 UNWIND range(1, {OUTPUT}) AS i \
         SET m.absent = null RETURN m, i {tail}"
    )
}

fn capped_db(limit: u64) -> relate::Connection {
    let mut connection = common::workspace_capped(limit);
    common::run(&mut connection, "CREATE (:Row {id: 1})");
    connection
}

/// The streamed statement answers its page wherever the pool sits — under
/// the small pool (the barrier's signature: one batch in flight, so a
/// budget far below the total output still answers) and under a roomy one
/// (the answer is the streamed barrier's, not a lucky small-pool path).
fn answers_its_first_row_under(limit: u64) {
    let mut c = capped_db(limit);
    let answered = common::row(&mut c, &statement("LIMIT 1"));
    assert_eq!(answered[1], Value::Int(1), "the page answers its first row");
}

#[test]
fn a_set_whose_output_crosses_the_pool_still_answers() {
    answers_its_first_row_under(BUDGET);
}

#[test]
fn the_same_set_answers_under_a_larger_pool() {
    answers_its_first_row_under(ROOMY);
}

/// The same write with a collecting consumer (an unlimited `ORDER BY`)
/// refuses under the small pool: the collector's container is the whole
/// output, charged batch by batch by the same rule, while the statement
/// answers under a roomy one — the streamed page above is what lets the
/// small pool answer at all.
#[test]
fn the_same_write_refuses_when_a_consumer_collects() {
    let collecting = statement("ORDER BY i");
    let mut c = capped_db(BUDGET);
    common::refused_workspace_limit(&mut c, &collecting);
    let mut roomy = capped_db(COLLECTING_ROOMY);
    common::run(&mut roomy, &collecting);
}

/// The page stops the rows, never the writes: a `SET` under a `LIMIT 1`
/// still applies every driving row's write — the last batch's value is
/// what the store ends with.
#[test]
fn a_page_stops_the_rows_not_the_writes() {
    let mut c = capped_db(ROOMY);
    common::run(
        &mut c,
        "MATCH (m:Row) UNWIND range(1, 2500) AS i SET m.last = i RETURN i LIMIT 1",
    );
    let last = common::first_column(&common::run(&mut c, "MATCH (m:Row) RETURN m.last"));
    assert_eq!(last, vec![Value::Int(2500)], "every batch's write ran");
}

/// A read after a streamed write sees the whole clause, not the batches
/// emitted so far: every graph-reading consumer drains its input before
/// it reads — a `MATCH`, a `CALL` body, and a projection whose items read
/// the graph (a `COUNT { … }` subquery takes the graph-sensitive
/// materializing path, never the streamed fold) — so the barrier's
/// streaming never exposes a partially applied clause. Ten creates, then
/// ten driving rows each reading all ten — the materialized answer, all
/// three spellings.
#[test]
fn a_read_after_a_streamed_write_sees_the_whole_clause() {
    // No seeded row: this pin touches only `:A`, so it opens its own
    // connections rather than the file's `:Row` fixture.
    let mut c = common::workspace_capped(ROOMY);
    let match_read = common::first_column(&common::run(
        &mut c,
        "UNWIND range(1, 10) AS i CREATE (:A) WITH i MATCH (a:A) RETURN count(a) AS c",
    ));
    assert_eq!(match_read, vec![Value::Int(100)], "the MATCH drained first");
    let mut call_read = common::workspace_capped(ROOMY);
    let call_count = common::first_column(&common::run(
        &mut call_read,
        "UNWIND range(1, 10) AS i CREATE (:A {i: i}) WITH i \
         CALL { MATCH (a:A) RETURN count(a) AS c } RETURN sum(c) AS c",
    ));
    assert_eq!(
        call_count,
        vec![Value::Int(100)],
        "the CALL body drained first"
    );
    let mut subquery_read = common::workspace_capped(ROOMY);
    let subquery_count = common::first_column(&common::run(
        &mut subquery_read,
        "UNWIND range(1, 10) AS i CREATE (:A) \
         WITH i, COUNT { MATCH (a:A) } AS c RETURN sum(c) AS c",
    ));
    assert_eq!(
        subquery_count,
        vec![Value::Int(100)],
        "the graph-reading item materialized its input"
    );
}

/// DELETE's rels-before-nodes is a whole-clause invariant (ADR-0676): a
/// row's relationship delete may be the one detaching a node a later row
/// deletes, so the arm resolves every row's targets before the first node
/// delete — the spec corpus's Set pin (`DELETE r0, n` over a connected
/// node), which a per-batch split broke with DeleteConnectedNode.
#[test]
fn a_deletes_rels_before_nodes_spans_the_whole_clause() {
    let mut c = common::workspace_capped(ROOMY);
    // The corpus scenario's own three-statement setup, verbatim in shape.
    for setup in [
        "CREATE (:MainNode {id: 'c', version: 0})",
        "UNWIND range(1, 128) AS x CREATE (:TestNode {id: x})",
        "MATCH (c:MainNode {id: 'c'}), (t:TestNode) CREATE (c)-[:TestRelation]->(t)",
    ] {
        common::run(&mut c, setup);
    }
    let answered = common::first_column(&common::run(
        &mut c,
        "MATCH (n) WHERE n.version = 0 SET n.version = n.version + 1          WITH n, n.id AS nodeId MATCH (n)-[r0]-() MATCH (n)-[r1]-()          DELETE r0, n RETURN DISTINCT nodeId AS id",
    ));
    assert_eq!(answered, vec![Value::String("c".into())]);
    let nodes = common::first_column(&common::run(&mut c, "MATCH (n) RETURN count(n)"));
    assert_eq!(nodes, vec![Value::Int(128)], "the TestNodes survive");
    let rels = common::first_column(&common::run(
        &mut c,
        "MATCH ()-[r:TestRelation]->() RETURN count(r)",
    ));
    assert_eq!(rels, vec![Value::Int(0)], "every relationship was deleted");
}
