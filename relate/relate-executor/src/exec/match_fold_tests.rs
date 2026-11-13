//! The streamed `MATCH` (the aggregation-push sink): a `MATCH` whose consumer
//! is an aggregating `WITH`/`RETURN` folds its rows as they are emitted
//! instead of collecting them. Every statement runs three ways — folded, on
//! the walker only (`RELATE_PATTERN_ENGINE=walk`), and held (a `WITH *`
//! between the `MATCH` and the aggregation defeats the fold, so the clause
//! collects its rows as it always did) — and the three must agree.

use relate_core::Value;

use crate::pattern_engine::PatternEngine;

use super::test_support::{build, exec_cypher, profiled, same_rows, walk_only, Built};

/// A hub (node 0) over `fan` children, each with two grandchildren, plus a
/// back edge from the first grandchild to the hub so paths revisit it.
fn hub(fan: usize) -> Built {
    let total = 1 + fan + 2 * fan;
    let labels = vec![&["N"][..]; total];
    let mut rels = Vec::new();
    for c in 0..fan {
        let child = 1 + c;
        rels.push((0, child, "E"));
        for g in 0..2 {
            rels.push((child, 1 + fan + 2 * c + g, "E"));
        }
    }
    rels.push((1 + fan, 0, "E"));
    build(&labels, &rels)
}

/// `matched` is the `MATCH` text, `rest` the aggregating `RETURN`: folded as
/// written, held with a `WITH *` between them.
fn assert_agrees(matched: &str, rest: &str) {
    let folded = format!("{matched} {rest}");
    let held = format!("{matched} WITH * {rest}");
    let auto = hub(6);
    let walk = walk_only(hub(6));
    let expected = exec_cypher(&auto.graph, &held);
    assert!(!expected.rows.is_empty(), "{folded}: the fixture matches");
    let got = exec_cypher(&auto.graph, &folded);
    assert!(same_rows(&got, &expected), "{folded}: folded vs held");
    let walked = exec_cypher(&walk.graph, &folded);
    assert!(same_rows(&walked, &expected), "{folded}: walk vs held");
}

#[test]
fn count_star_over_a_variable_length_match() {
    assert_agrees("MATCH (h:N {id: 0})-[:E*1..2]->(e)", "RETURN count(*) AS c");
}

#[test]
fn count_distinct_end_over_a_variable_length_match() {
    assert_agrees(
        "MATCH (h:N {id: 0})-[:E*1..2]->(e)",
        "RETURN count(DISTINCT e) AS c",
    );
}

#[test]
fn sum_over_a_variable_length_match() {
    assert_agrees(
        "MATCH (h:N {id: 0})-[:E*1..2]->(e)",
        "RETURN sum(e.id) AS s",
    );
}

#[test]
fn group_key_over_a_variable_length_match() {
    assert_agrees(
        "MATCH (h:N {id: 0})-[:E*1..2]->(e)",
        "RETURN e.id % 3 AS k, count(*) AS c",
    );
}

#[test]
fn where_on_the_match() {
    assert_agrees(
        "MATCH (h:N {id: 0})-[:E*1..2]->(e) WHERE e.id > 3",
        "RETURN count(*) AS c, sum(e.id) AS s",
    );
}

#[test]
fn multi_pattern_match() {
    assert_agrees(
        "MATCH (h:N {id: 0})-[:E]->(a), (h)-[:E]->(b)",
        "RETURN count(*) AS c, count(DISTINCT a) AS d",
    );
}

#[test]
fn with_consumer_feeds_the_next_clause() {
    assert_agrees(
        "MATCH (h:N {id: 0})-[:E*1..2]->(e)",
        "WITH count(*) AS c RETURN c + 1 AS next",
    );
}

#[test]
fn an_empty_match_still_yields_the_keyless_group() {
    let built = hub(3);
    let r = exec_cypher(
        &built.graph,
        "MATCH (h:N {id: 99})-[:E*1..2]->(e) RETURN count(*) AS c",
    );
    assert_eq!(r.rows, vec![vec![Value::Int(0)]]);
}

/// PROFILE reports the folded clause's row count as the rows it emitted —
/// the count the held clause reports — while holding none of them.
#[test]
fn profile_counts_the_rows_the_fold_took() {
    let built = hub(6);
    let matched = "MATCH (h:N {id: 0})-[:E*1..2]->(e)";
    let folded = profiled(&built.graph, &format!("{matched} RETURN count(*) AS c"));
    let held = profiled(
        &built.graph,
        &format!("{matched} WITH * RETURN count(*) AS c"),
    );
    assert!(held[0].rows > 0 && held[0].memory_bytes > 0);
    assert_eq!(folded[0].rows, held[0].rows, "the clause's emitted rows");
    assert_eq!(folded[0].memory_bytes, 0, "the fold holds no rows");
}

/// An `OPTIONAL MATCH` is never folded: its null-bound row for an input
/// with no match must reach the aggregation.
#[test]
fn optional_match_without_a_match_still_counts_the_null_row() {
    let built = hub(3);
    let r = exec_cypher(
        &built.graph,
        "MATCH (h:N {id: 99}) OPTIONAL MATCH (h)-[:E]->(b) RETURN count(*) AS c",
    );
    assert_eq!(r.rows, vec![vec![Value::Int(0)]], "no input row at all");
    let r = exec_cypher(
        &built.graph,
        "MATCH (h:N {id: 0}) OPTIONAL MATCH (h)-[:NOPE]->(b) RETURN count(*) AS c",
    );
    assert_eq!(r.rows, vec![vec![Value::Int(1)]]);
    let r = exec_cypher(
        &built.graph,
        "MATCH (h:N {id: 0}) OPTIONAL MATCH (h)-[:NOPE]->(b) WITH count(*) AS c RETURN c",
    );
    assert_eq!(r.rows, vec![vec![Value::Int(1)]]);
}

/// Inputs `P` with 1, 2 and no `R` children (ids 0, 1, 2), and a hub `H`
/// (id 16) with 10: the shapes an `OPTIONAL MATCH` feeding an aggregate
/// must tell apart (a null row per unmatched input, none per matched one).
fn optional_graph() -> Built {
    let mut labels = vec![&["P"][..]; 3];
    labels.extend(vec![&["C"][..]; 13]);
    labels.push(&["H"][..]);
    labels.extend(vec![&["C"][..]; 10]);
    let mut rels = vec![(0, 3, "R"), (1, 4, "R"), (1, 5, "R")];
    rels.extend((17..27).map(|child| (16, child, "R")));
    build(&labels, &rels)
}

/// Run `cypher` on every pattern engine, asserting each agrees with the
/// default routing, and return the default's rows (sorted).
fn on_every_engine(cypher: &str) -> Vec<Vec<Value>> {
    let run = |engine: PatternEngine| {
        let mut built = optional_graph();
        built.graph.configure_pattern_engine(engine);
        exec_cypher(&built.graph, cypher)
    };
    let reference = PatternEngine::default();
    let baseline = run(reference);
    for engine in PatternEngine::ALL
        .into_iter()
        .filter(|&engine| engine != reference)
    {
        assert!(
            same_rows(&run(engine), &baseline),
            "{cypher}: {engine:?} vs default"
        );
    }
    let mut rows = baseline.rows;
    rows.sort_by_cached_key(|row| format!("{row:?}"));
    rows
}

fn ints_row(cells: &[i64]) -> Vec<Value> {
    cells.iter().copied().map(Value::Int).collect()
}

#[test]
fn optional_match_of_one_two_and_ten_rows_aggregates_each() {
    for (input, matched) in [(0, 1), (1, 2), (16, 10)] {
        let rows = on_every_engine(&format!(
            "MATCH (p {{id: {input}}}) OPTIONAL MATCH (p)-[:R]->(c) \
             RETURN count(*) AS rows, count(c) AS matched"
        ));
        assert_eq!(rows, vec![ints_row(&[matched, matched])], "input {input}");
    }
}

#[test]
fn optional_match_adds_a_null_row_per_unmatched_input_only() {
    // Three inputs, two matching (1 + 2 children): four rows, three matches.
    let rows = on_every_engine(
        "MATCH (p:P) OPTIONAL MATCH (p)-[:R]->(c) \
         RETURN count(*) AS rows, count(c) AS matched",
    );
    assert_eq!(rows, vec![ints_row(&[4, 3])]);
}

#[test]
fn aggregates_over_the_null_row_follow_their_null_semantics() {
    // Mixed inputs: the null row is skipped by every non-star aggregate.
    let rows = on_every_engine(
        "MATCH (p:P) OPTIONAL MATCH (p)-[:R]->(c) \
         RETURN count(*) AS rows, count(c) AS matched, sum(c.id) AS total, \
                min(c.id) AS lo, max(c.id) AS hi, size(collect(c.id)) AS kept, \
                count(DISTINCT c) AS distinct_matched",
    );
    assert_eq!(rows, vec![ints_row(&[4, 3, 12, 3, 5, 3, 3])]);
    // Only the null row: count(*) counts it, the rest see nothing.
    let rows = on_every_engine(
        "MATCH (p {id: 2}) OPTIONAL MATCH (p)-[:R]->(c) \
         RETURN count(*) AS rows, count(c) AS matched, sum(c.id) AS total, \
                min(c.id) AS lo, max(c.id) AS hi, size(collect(c.id)) AS kept, \
                count(DISTINCT c) AS distinct_matched",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::Int(1),
            Value::Int(0),
            Value::Int(0),
            Value::Null,
            Value::Null,
            Value::Int(0),
            Value::Int(0),
        ]]
    );
}

#[test]
fn group_by_an_input_variable_mixes_matched_and_unmatched_groups() {
    let rows = on_every_engine(
        "MATCH (p:P) OPTIONAL MATCH (p)-[:R]->(c) \
         RETURN p.id AS p, count(*) AS rows, count(c) AS matched",
    );
    assert_eq!(
        rows,
        vec![
            ints_row(&[0, 1, 1]),
            ints_row(&[1, 2, 2]),
            ints_row(&[2, 1, 0])
        ]
    );
}

/// For the inputs that do match, the optional form answers exactly what the
/// plain `MATCH` does.
#[test]
fn matched_groups_equal_the_plain_match() {
    let built = optional_graph();
    let optional = exec_cypher(
        &built.graph,
        "MATCH (p:P) WHERE p.id IN [0, 1] OPTIONAL MATCH (p)-[:R]->(c) \
         RETURN p.id AS p, count(*) AS rows, sum(c.id) AS total",
    );
    let plain = exec_cypher(
        &built.graph,
        "MATCH (p:P) WHERE p.id IN [0, 1] MATCH (p)-[:R]->(c) \
         RETURN p.id AS p, count(*) AS rows, sum(c.id) AS total",
    );
    assert!(same_rows(&optional, &plain));
}

#[test]
fn optional_match_with_matches_counts_like_the_plain_match() {
    let built = hub(3);
    let plain = exec_cypher(
        &built.graph,
        "MATCH (h:N {id: 0}) MATCH (h)-[:E*1..2]->(e) RETURN count(*) AS c",
    );
    let optional = exec_cypher(
        &built.graph,
        "MATCH (h:N {id: 0}) OPTIONAL MATCH (h)-[:E*1..2]->(e) RETURN count(*) AS c",
    );
    assert!(same_rows(&optional, &plain));
}

/// The distinct-endpoint rung can decline on a later seed after an earlier
/// seed was served; a fold it was handed must then have seen no row, or the
/// walker that re-runs the clause offers the same rows again.
#[test]
fn a_declining_frontier_rung_offers_the_fold_nothing() {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use relate_front::parse_and_lower;
    use relate_ir::Clause;

    use super::kernels::frontier_run::try_frontier_rows;
    use super::kernels::with_frontier_table_budget;
    use super::pattern_policies::RowFold;
    use crate::ctx::ExecCtx;
    use crate::overlay::ExecGraph;
    use crate::row::Row;

    struct Counting(Cell<u64>);
    impl RowFold for Counting {
        fn offer(&self, _: &ExecGraph<'_>, _: Row) -> crate::exec::Result<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }

    // Seed 0 reaches one node; seed 2 heads a long chain whose tables
    // outgrow the held ceiling.
    let total = 400;
    let labels = vec![&["N"][..]; total];
    let mut rels = vec![(0, 1, "T")];
    rels.extend((2..total - 1).map(|i| (i, i + 1, "T")));
    let built = build(&labels, &rels);
    let query =
        parse_and_lower("MATCH (x:N)-[:T*1..]->(y:N) RETURN count(DISTINCT y) AS c").unwrap();
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
    let fold = Counting(Cell::new(0));
    let answer = with_frontier_table_budget(2048, || {
        try_frontier_rows(
            ExecCtx::new(&view, &params),
            &[Row::new()],
            false,
            *match_mode,
            patterns,
            where_clause.as_ref(),
            &clauses[1..],
            None,
            Some(&fold),
        )
        .unwrap()
        .served()
    });
    assert!(answer.is_none(), "the later seed's run declines");
    assert_eq!(fold.0.get(), 0, "a declined rung offered the fold rows");
}

/// The pairs the rung parks for an attached fold are output-cardinality
/// memory: they are billed to a retaining container, so the per-value cap
/// refuses with a typed capacity error instead of growing silently.
#[test]
fn parked_fold_pairs_are_charged_against_the_expression_limit() {
    use std::collections::BTreeMap;

    use relate_front::parse_and_lower;
    use relate_ir::Clause;

    use super::kernels::frontier_run::try_frontier_rows;
    use super::pattern_policies::RowFold;
    use crate::ctx::ExecCtx;
    use crate::overlay::ExecGraph;
    use crate::row::Row;

    struct Discard;
    impl RowFold for Discard {
        fn offer(&self, _: &ExecGraph<'_>, _: Row) -> crate::exec::Result<()> {
            Ok(())
        }
    }

    let fan = 500;
    let mut built = hub(fan);
    // Far below the parked pairs' footprint (500 pairs), above any other
    // single allocation the rung makes.
    built.graph.configure_expression_memory_limit(Some(1024));
    let query =
        parse_and_lower("MATCH (x:N {id: 0})-[:E*1..]->(y:N) RETURN count(DISTINCT y) AS c")
            .unwrap();
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
    let err = try_frontier_rows(
        ExecCtx::new(&view, &params),
        &[Row::new()],
        false,
        *match_mode,
        patterns,
        where_clause.as_ref(),
        &clauses[1..],
        None,
        Some(&Discard),
    )
    .map(|_| ())
    .unwrap_err();
    assert!(
        matches!(
            err,
            relate_core::Error::Capacity {
                kind: relate_core::CapacityKind::Expression,
                ..
            }
        ),
        "{err}"
    );
}
