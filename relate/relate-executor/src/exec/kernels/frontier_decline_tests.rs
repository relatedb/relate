//! The PROFILE record's plan-time decline reason (LABS-1437): the typed
//! gate that kept a clause off the frontier rung rides the record of the
//! operator that served it instead (the walker, usually), with the
//! frontier counters left at zero because the rung never ran. The
//! run-time half of the same field lives in `frontier_rider_tests`.

use crate::ctx::KernelRung;
use crate::exec::test_support::{complete_graph, file_triangle, line_graph, profiled, served};
use crate::frontier_decline::{FrontierDecline, FrontierDeclineReason};
use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;

/// The plan-time gates' typed reason rides the record, one distinct
/// reason per distinct gate — a constant would fail all but one.
#[test]
fn profile_names_the_plan_time_decline() {
    let tri = file_triangle();
    let reason = |graph: &Graph, cypher: &str| {
        let records = profiled(graph, cypher);
        let record = served(&records);
        assert_eq!(
            record.frontier.levels, 0,
            "a plan-time decline never ran: {cypher}"
        );
        (record.kernel, record.frontier_decline)
    };
    // The clause's conjunct is outside the closed vocabulary.
    assert_eq!(
        reason(
            &tri.graph,
            "MATCH (a:File)-[:T*1..3]-(b:File) WHERE a.id = b.id \
             WITH DISTINCT a, b RETURN count(*) AS c"
        ),
        (
            Some(KernelRung::Walker),
            Some(FrontierDeclineReason::Plan(FrontierDecline::Conjunct))
        )
    );
    // The consumer needs every row.
    assert_eq!(
        reason(
            &tri.graph,
            "MATCH (a:File)-[:T*1..3]-(b:File) RETURN a.id AS a, b.id AS b"
        )
        .1,
        Some(FrontierDeclineReason::Plan(
            FrontierDecline::FullEnumeration
        ))
    );
    // The path multiset.
    assert_eq!(
        reason(
            &tri.graph,
            "MATCH (a:File)-[:T*1..3]-(b:File) RETURN count(*) AS c"
        )
        .1,
        Some(FrontierDeclineReason::Plan(FrontierDecline::PathCount))
    );
    // The cost gate: the line is output-bound.
    let line = line_graph();
    assert_eq!(
        reason(
            &line,
            "MATCH (x:Person)-[:KNOWS*1..2]-(y:Person) WHERE x <> y \
             RETURN DISTINCT x, y"
        )
        .1,
        Some(FrontierDeclineReason::Plan(FrontierDecline::Cost))
    );
}

/// A served clause, a clause the family never considers, and a stood-down
/// family record no reason.
#[test]
fn profile_records_no_reason_where_there_is_none() {
    let mut complete = complete_graph();
    let served_query = "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) WHERE x <> y \
                        WITH DISTINCT x, y RETURN count(*) AS c";
    let records = profiled(&complete, served_query);
    assert_eq!(served(&records).kernel, Some(KernelRung::FrontierNfa));
    assert_eq!(served(&records).frontier_decline, None, "a served run");
    // No quantified pattern: the family is not considered.
    let records = profiled(&complete, "MATCH (x:Person)-[:KNOWS]-(y:Person) RETURN x");
    assert_eq!(served(&records).frontier_decline, None, "a plain hop");
    // The walker switch stands the family down before any gate.
    complete.configure_pattern_engine(PatternEngine::Walk);
    let records = profiled(&complete, served_query);
    assert_eq!(served(&records).kernel, Some(KernelRung::Walker));
    assert_eq!(served(&records).frontier_decline, None, "stood down");
}

/// A nested evaluation's own ladder run contributes nothing to the
/// enclosing clause's record: the decline rides the ladder's return value
/// of the profiled clause only. The subquery below declines on the answer
/// shape (`full-enumeration`) inside the nested engine, and no record of
/// the statement names it.
#[test]
fn a_nested_evaluation_does_not_leak_its_decline() {
    let tri = file_triangle();
    let records = profiled(
        &tri.graph,
        "CALL { MATCH (a:File)-[:T*1..3]-(b:File) RETURN a.id AS a, b.id AS b } \
         RETURN count(*) AS c",
    );
    assert!(!records.is_empty(), "the statement is profiled");
    assert!(
        records
            .iter()
            .all(|record| record.frontier_decline.is_none()),
        "no enclosing record names the nested clause's decline"
    );
}
