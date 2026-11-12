//! The streamed clause run (`match_chain`): a `MATCH` pushes its rows into
//! the stages after it — plain `MATCH` clauses and projection-only `WITH`
//! clauses — in bounded batches instead of handing over a retained vector.
//! Every statement runs three ways — as written (chained), with an
//! `UNWIND [0]` between the first two clauses (a barrier clause the chain
//! never covers, so the first boundary takes the held path), and on the
//! walker only — and the answers must agree. The memory tests pin the point
//! of the chain: what an earlier clause produced is never retained, so a
//! limit the held path crosses is not crossed, and the workspace peak does
//! not grow with the intermediate row count.

use std::cell::Cell;
use std::collections::BTreeMap;

use relate_core::{CapacityKind, Error, Value};
use relate_front::parse_and_lower;
use relate_ir::Clause;

use super::match_chain::CHAIN_BATCH;
use super::match_clause::{charge_finished_container, run_match_operator_ladder};
use super::pattern_policies::RowFold;
use super::test_support::{
    build, error_of, exec_cypher, run_metered, same_rows, try_exec_cypher, walk_only, Built,
};
use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::row::Row;

/// The ring `i -> i+1` with the chord `i -> i+7` over `total` nodes.
fn ring_rels(total: usize) -> Vec<(usize, usize, &'static str)> {
    (0..total)
        .flat_map(|i| [(i, (i + 1) % total, "E"), (i, (i + 7) % total, "E")])
        .collect()
}

/// `n` nodes on a ring (`i -> i+1`) with a chord (`i -> i+7`); every fifth
/// node is a `:P`, the rest `:N`. Ids are the indexes.
fn ring(n: usize) -> Built {
    let labels: Vec<&[&str]> = (0..n)
        .map(|i| if i % 5 == 0 { &["P"][..] } else { &["N"][..] })
        .collect();
    build(&labels, &ring_rels(n))
}

/// The statement as written (chained) and with a barrier between its first
/// two clauses (held).
fn chained_and_held(first: &str, second: &str, rest: &str) -> (String, String) {
    (
        format!("{first} {second} {rest}"),
        format!("{first} {HELD} {second} {rest}"),
    )
}

/// `first` and `second` are the two `MATCH` clauses, `rest` what follows.
/// Chained as written, held with a barrier between them, and walked.
fn assert_agrees(first: &str, second: &str, rest: &str) {
    let (chained, held) = chained_and_held(first, second, rest);
    let auto = ring(40);
    let walk = walk_only(ring(40));
    let expected = exec_cypher(&auto.graph, &held);
    assert!(!expected.rows.is_empty(), "{chained}: the fixture matches");
    let got = exec_cypher(&auto.graph, &chained);
    assert!(same_rows(&got, &expected), "{chained}: chained vs held");
    let walked = exec_cypher(&walk.graph, &chained);
    assert!(same_rows(&walked, &expected), "{chained}: walk vs held");
}

/// The same three-way agreement for a result whose row order is part of
/// what a consumer sees: the chain keeps the held path's order exactly.
fn assert_same_order(first: &str, second: &str, rest: &str) {
    let (chained, held) = chained_and_held(first, second, rest);
    let built = ring(40);
    let expected = exec_cypher(&built.graph, &held);
    assert!(!expected.rows.is_empty(), "{chained}: the fixture matches");
    let got = exec_cypher(&built.graph, &chained);
    assert_eq!(got.columns, expected.columns, "{chained}");
    assert_eq!(got.rows, expected.rows, "{chained}: row order");
}

const CROSS: &str = "MATCH (p:P), (n:N) WHERE n.id <> p.id";

/// A clause the chain never covers: one row in, the same row out.
const HELD: &str = "UNWIND [0] AS held";

#[test]
fn counts_over_a_cross_product_then_a_variable_length_match() {
    assert_agrees(
        CROSS,
        "MATCH (p)-[:E*1..2]->(m)",
        "RETURN count(*) AS c, count(DISTINCT m) AS d",
    );
}

#[test]
fn shortest_path_per_pair_after_a_cross_product() {
    assert_agrees(
        CROSS,
        "MATCH path = shortestPath((p)-[*1..3]-(n))",
        "RETURN count(*) AS pairs, sum(length(path)) AS hops",
    );
}

#[test]
fn group_keys_over_the_second_match() {
    assert_agrees(
        CROSS,
        "MATCH (p)-[:E*1..2]->(m)",
        "RETURN p.id AS p, count(*) AS c",
    );
}

#[test]
fn a_non_aggregating_projection_after_the_second_match() {
    assert_agrees(
        CROSS,
        "MATCH (p)-[:E*1..2]->(m)",
        "RETURN p.id AS p, n.id AS n, m.id AS m",
    );
}

#[test]
fn a_with_between_the_second_match_and_the_aggregate() {
    assert_agrees(
        CROSS,
        "MATCH (p)-[:E*1..2]->(m)",
        "WITH p, m RETURN count(*) AS c",
    );
}

#[test]
fn bindings_survive_for_later_clauses() {
    assert_agrees(
        "MATCH (a:P)",
        "MATCH (a)-[:E*1..2]->(b)",
        "MATCH (b)-[:E*1..2]->(c) RETURN count(*) AS c, count(DISTINCT c) AS d",
    );
}

#[test]
fn a_where_on_the_second_match() {
    assert_agrees(
        CROSS,
        "MATCH (p)-[:E*1..3]->(m) WHERE m.id > n.id",
        "RETURN count(*) AS c, sum(m.id) AS s",
    );
}

/// A projection-only `WITH` is a stage: it maps each row and streams.
#[test]
fn a_projection_only_with_streams_into_the_aggregate() {
    assert_agrees(CROSS, "WITH p, n", "RETURN count(*) AS c");
    assert_agrees(
        CROSS,
        "WITH p.id AS pid, n.id AS nid",
        "RETURN sum(pid * nid) AS s, count(DISTINCT nid) AS d",
    );
}

#[test]
fn a_with_filter_streams() {
    assert_agrees(
        CROSS,
        "WITH p, n WHERE p.id < n.id",
        "RETURN count(*) AS c, sum(n.id) AS s",
    );
}

#[test]
fn stages_alternate_between_matches_and_withs() {
    assert_agrees(
        CROSS,
        "WITH p, n WHERE p.id < n.id MATCH (p)-[:E*1..2]->(m) WITH p AS p, n AS n, m AS m",
        "MATCH (m)-[:E*1..2]->(z) RETURN count(*) AS c, sum(z.id) AS s",
    );
}

#[test]
fn three_matches_in_a_row() {
    assert_agrees(
        "MATCH (a:P)",
        "MATCH (a)-[:E*1..2]->(b) MATCH (b)-[:E*1..2]->(c)",
        "RETURN count(*) AS c, count(DISTINCT c) AS d",
    );
}

/// The breakers — `DISTINCT`, `ORDER BY`, `SKIP`/`LIMIT`, aggregation —
/// keep the barrier and their answers.
#[test]
fn breaking_withs_keep_their_answers() {
    for second in [
        "WITH DISTINCT p, n",
        "WITH p, n ORDER BY n.id, p.id SKIP 3 LIMIT 40",
        "WITH p, n ORDER BY n.id, p.id LIMIT 40",
        "WITH p, count(*) AS n",
    ] {
        assert_agrees(CROSS, second, "RETURN count(*) AS c, sum(p.id) AS s");
    }
}

/// A lone `WITH` ahead of a non-aggregating clause stays on the sink
/// projection, and the rows come out in the held path's order.
#[test]
fn a_trailing_with_keeps_the_sink_projection_and_the_order() {
    assert_same_order(CROSS, "WITH p, n", "RETURN p.id AS p, n.id AS n");
    assert_same_order(
        CROSS,
        "WITH p, n",
        "WITH p.id AS p, n.id AS n RETURN p + n AS s",
    );
}

/// Cypher scopes relationship uniqueness to one clause: the second `MATCH`
/// may walk the very relationships the first one bound.
#[test]
fn a_relationship_may_be_reused_across_the_two_clauses() {
    assert_agrees(
        "MATCH (a:P)-[r:E*1..2]->(b)",
        "MATCH (a)-[s:E*1..2]->(c)",
        "RETURN count(*) AS c",
    );
}

#[test]
fn row_order_matches_the_held_path() {
    assert_same_order(
        CROSS,
        "MATCH (p)-[:E*1..2]->(m)",
        "RETURN p.id AS p, n.id AS n, m.id AS m",
    );
}

/// More first-clause rows than one batch: the batches are consecutive
/// slices, so counts, groups and the row order are the held path's.
#[test]
fn more_rows_than_one_batch() {
    let built = ring(120);
    let first = "MATCH (a:P), (b:N)";
    let rows = exec_cypher(&built.graph, &format!("{first} RETURN count(*) AS c")).rows;
    let Value::Int(produced) = rows[0][0] else {
        panic!("count is an integer");
    };
    assert!(produced as usize > 3 * CHAIN_BATCH, "{produced} rows");
    let tail = "MATCH (a)-[:E*1..2]->(m) RETURN a.id AS a, b.id AS b, m.id AS m";
    let chained = exec_cypher(&built.graph, &format!("{first} {tail}"));
    let held = exec_cypher(&built.graph, &format!("{first} {HELD} {tail}"));
    assert_eq!(chained.rows, held.rows);
    let agg = "MATCH (a)-[:E*1..2]->(m) RETURN a.id % 3 AS k, count(*) AS c";
    let chained = exec_cypher(&built.graph, &format!("{first} {agg}"));
    let held = exec_cypher(&built.graph, &format!("{first} {HELD} {agg}"));
    assert!(same_rows(&chained, &held));
}

#[test]
fn an_empty_first_match_yields_nothing_and_the_keyless_group() {
    let built = ring(20);
    let r = exec_cypher(
        &built.graph,
        "MATCH (a:Nope) MATCH (a)-[:E*1..2]->(c) RETURN count(*) AS c",
    );
    assert_eq!(r.rows, vec![vec![Value::Int(0)]]);
    let r = exec_cypher(
        &built.graph,
        "MATCH (a:Nope) MATCH (a)-[:E*1..2]->(c) RETURN c.id AS c",
    );
    assert!(r.rows.is_empty());
}

/// The shapes the chain declines keep the held path and its answers.
#[test]
fn declined_second_clauses_keep_their_answers() {
    for (second, rest) in [
        // an OPTIONAL second clause keeps its null-bound rows
        (
            "OPTIONAL MATCH (p)-[:NOPE*1..2]->(m)",
            "RETURN count(*) AS c",
        ),
        // a one-hop aggregate fold answers the pair in one pass
        ("MATCH (p)-[:E]->(m)", "RETURN count(*) AS c"),
    ] {
        assert_agrees(CROSS, second, rest);
    }
}

/// A limit cap stops the second clause's walk: the pair keeps the held
/// path, and the capped answer has the held path's row count.
#[test]
fn a_limit_on_the_second_match_keeps_its_row_count() {
    let built = ring(40);
    let second = "MATCH (p)-[:E*1..2]->(m) RETURN m.id AS m LIMIT 5";
    let chained = exec_cypher(&built.graph, &format!("{CROSS} {second}"));
    let held = exec_cypher(&built.graph, &format!("{CROSS} {HELD} {second}"));
    assert_eq!(chained.rows.len(), 5);
    assert_eq!(chained.rows.len(), held.rows.len());
}

/// An error in the second clause surfaces exactly as it does when the first
/// clause's rows are held.
#[test]
fn an_error_in_the_second_match_surfaces() {
    let built = ring(20);
    let second = "MATCH (a)-[:E*1..2]->(c) WHERE 1 / (c.id - c.id) = 0 RETURN count(*) AS c";
    let chained = try_exec_cypher(&built.graph, &format!("MATCH (a:P) {second}"));
    let held = try_exec_cypher(&built.graph, &format!("MATCH (a:P) {HELD} {second}"));
    assert_eq!(error_of(chained), error_of(held));
}

fn refusal<T>(result: relate_core::Result<T>) -> bool {
    matches!(
        result,
        Err(Error::Capacity {
            kind: CapacityKind::Expression,
            ..
        })
    )
}

/// The statement the ticket measured, in miniature: a cross product feeding
/// a per-pair `shortestPath`. A limit far below the cross product's retained
/// footprint refuses the held form and passes the chained one, with the
/// answer the unlimited run gives.
#[test]
fn the_cross_product_is_not_retained_against_the_limit() {
    let gate = "MATCH (p:P), (n) WHERE n <> p \
                MATCH path = shortestPath((p)-[*1..3]-(n)) \
                RETURN count(*) AS pairs, sum(length(path)) AS hops";
    let mut built = ring(200);
    let expected = exec_cypher(&built.graph, gate);
    let held = gate.replace("MATCH path", &format!("{HELD} MATCH path"));
    // 40 prompts x 199 nodes = 7,960 cross-product rows, ~1.2 MiB retained.
    built
        .graph
        .configure_expression_memory_limit(Some(512 * 1024));
    assert!(
        refusal(try_exec_cypher(&built.graph, &held)),
        "the held form crosses the limit"
    );
    let got = try_exec_cypher(&built.graph, gate).expect("the chained form fits");
    assert_eq!(got.rows, expected.rows);
    assert!(matches!(got.rows[0][0], Value::Int(pairs) if pairs > 0));
}

/// The `WITH` form of the same ticket measurement: a cross product, a
/// projection-only `WITH`, then the aggregate.
#[test]
fn a_with_after_the_cross_product_does_not_retain_it_either() {
    let mut built = ring(200);
    built
        .graph
        .configure_expression_memory_limit(Some(512 * 1024));
    let q = |barrier: &str| {
        format!("MATCH (p:P), (n) WHERE n <> p {barrier} WITH p, n RETURN count(*) AS c")
    };
    assert!(refusal(try_exec_cypher(&built.graph, &q(HELD))));
    let got = try_exec_cypher(&built.graph, &q("")).expect("the streamed form fits");
    assert_eq!(got.rows, vec![vec![Value::Int(40 * 199)]]);
}

/// What the second clause emits is still retained (and charged), as it is
/// without the chain: an emitting second clause refuses at the limit too.
#[test]
fn the_second_clauses_own_output_is_still_charged() {
    let mut built = ring(200);
    built
        .graph
        .configure_expression_memory_limit(Some(512 * 1024));
    let q = "MATCH (p:P), (n:N) MATCH (p)-[:E*1..2]->(m) RETURN p.id AS p, n.id AS n, m.id AS m";
    assert!(refusal(try_exec_cypher(&built.graph, q)));
}

/// `prompts` `:P` nodes then `others` `:N` nodes on one ring with a chord:
/// `MATCH (p:P), (n:N)` yields `prompts * others` rows while the first
/// pattern's own output (the prompts) stays the same size.
fn prompts_and_others(prompts: usize, others: usize) -> Built {
    let total = prompts + others;
    let labels: Vec<&[&str]> = (0..total)
        .map(|i| if i < prompts { &["P"][..] } else { &["N"][..] })
        .collect();
    build(&labels, &ring_rels(total))
}

/// The workspace peak of the streamed run does not grow with the
/// intermediate row count (N and 10x N rows within 1.25x), while the held
/// run's peak does — so the check can tell the two apart.
#[test]
fn peak_workspace_is_independent_of_the_intermediate_row_count() {
    let query = |held: bool, with: bool| {
        format!(
            "MATCH (p:P), (n:N) {} {}MATCH (p)-[:E*1..2]->(m) \
             RETURN count(*) AS c, sum(m.id) AS s",
            if held { HELD } else { "" },
            if with { "WITH p, n " } else { "" },
        )
    };
    // 20 prompts x 150 others = 3,000 rows; x 1,500 others = 30,000 (10x).
    let (small, large) = (prompts_and_others(20, 150), prompts_and_others(20, 1_500));
    for with in [false, true] {
        let (small_rows, chained_n) = run_metered(&small.graph, &query(false, with));
        let (large_rows, chained_10n) = run_metered(&large.graph, &query(false, with));
        assert_eq!(small_rows[0][0], Value::Int(20 * 150 * 6));
        assert_eq!(large_rows[0][0], Value::Int(20 * 1_500 * 6));
        let (_, held_n) = run_metered(&small.graph, &query(true, with));
        let (_, held_10n) = run_metered(&large.graph, &query(true, with));
        eprintln!(
            "with={with}: chained peak {chained_n} -> {chained_10n}; held {held_n} -> {held_10n}"
        );
        assert!(
            chained_10n as f64 <= chained_n as f64 * 1.25,
            "chained peak {chained_10n} at 10N vs {chained_n} at N"
        );
        assert!(
            held_10n as f64 > held_n as f64 * 4.0,
            "the held peak grows with the rows: {held_10n} vs {held_n}"
        );
    }
}

/// A rung's finished container headed to a fold that buffers it onward is
/// charged against the limit before it is offered (the chain bounds it as
/// the held path did); the aggregation fold keeps its historical no-charge.
#[test]
fn a_finished_container_is_charged_only_when_the_fold_asks() {
    struct Counting {
        offered: Cell<u64>,
        charges: bool,
    }
    impl RowFold for Counting {
        fn offer(&self, _: &ExecGraph<'_>, _: Row) -> relate_core::Result<()> {
            self.offered.set(self.offered.get() + 1);
            Ok(())
        }
        fn offer_container(
            &self,
            graph: &ExecGraph<'_>,
            rows: Vec<Row>,
        ) -> relate_core::Result<()> {
            if self.charges {
                charge_finished_container(graph, &rows)?;
            }
            rows.into_iter().try_for_each(|row| self.offer(graph, row))
        }
    }

    let mut built = ring(400);
    built
        .graph
        .configure_expression_memory_limit(Some(8 * 1024));
    let query = parse_and_lower("MATCH (n:N) WHERE n.id > 3 RETURN count(*) AS c").unwrap();
    let plan = relate_planner::plan(query, &mut |_| false).unwrap();
    let clauses = &plan.parts[0];
    let Clause::Match {
        match_mode,
        patterns,
        where_clause,
        ..
    } = &clauses[0]
    else {
        panic!("first clause is a MATCH");
    };
    let view = ExecGraph::read(&built.graph);
    let params = BTreeMap::new();
    let run = |fold: &Counting| {
        run_match_operator_ladder(
            ExecCtx::new(&view, &params, None),
            vec![Row::new()],
            false,
            *match_mode,
            patterns,
            where_clause.as_ref(),
            &mut None,
            None,
            &clauses[1..],
            Some(fold),
        )
        .map(|_| ())
    };
    let free = Counting {
        offered: Cell::new(0),
        charges: false,
    };
    run(&free).expect("an aggregation-style fold is not charged");
    // 320 `:N` nodes, minus ids 1..=3 which the filter drops.
    assert_eq!(free.offered.get(), 317);
    let charging = Counting {
        offered: Cell::new(0),
        charges: true,
    };
    assert!(refusal(run(&charging)));
    assert_eq!(charging.offered.get(), 0, "refused before any row landed");
}

/// A cancelled statement stops a stage at its next batch: the per-batch
/// check (beside the operator ladder's own per-row ticks) surfaces the
/// cancellation instead of running the batch.
#[test]
fn a_cancelled_statement_stops_at_the_next_batch() {
    use std::cell::OnceCell;

    use relate_core::CancelToken;

    use super::match_chain::open_match_chain;

    let built = ring(40);
    let query = parse_and_lower(
        "MATCH (a:P) MATCH (a)-[:E*1..2]->(m) RETURN count(*) AS c, sum(m.id) AS s",
    )
    .unwrap();
    let plan = relate_planner::plan(query, &mut |_| false).unwrap();
    let clauses = &plan.parts[0];
    let token = CancelToken::new();
    let view = ExecGraph::read(&built.graph).with_cancel(Some(&token));
    let params = BTreeMap::new();
    let slot = OnceCell::new();
    let chain = open_match_chain(ExecCtx::new(&view, &params, None), clauses, &slot, None)
        .unwrap()
        .expect("the second MATCH streams");
    for _ in 0..CHAIN_BATCH - 1 {
        chain.offer(&view, Row::new()).unwrap();
    }
    token.cancel();
    let error = chain.offer(&view, Row::new()).unwrap_err();
    assert!(error.to_string().contains("query cancelled"), "{error}");
}
