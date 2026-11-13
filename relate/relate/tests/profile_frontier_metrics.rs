//! The frontier-NFA rung's PROFILE metrics (ADR-0478 §3, ADR-0516): the
//! run counters, the NFA rung's own facts (cells, the re-arrival rule's
//! spent work and memo, the admission asks by verdict), the budget
//! fraction, and the run-time decline's typed reason — on the record
//! and on the wire args of the same operator row, for a served run and
//! a declined one. Split from `profile_metrics` (the per-operator metric
//! suite) to keep both under the file-size bound.

mod common;

use common::{connection, plan, wire_arg};
use relate_model::wire::plan_keys;

/// The first operator node of the tree whose metrics satisfy `pred` —
/// the one traversal every finder below shares (the `tree_hops` idiom's
/// predicate form), the node itself so its metrics and its wire args
/// read the same row.
fn find_operator<'a>(
    node: &'a relate::PlanNode,
    pred: &dyn Fn(&relate::ClauseMetrics) -> bool,
) -> Option<&'a relate::PlanNode> {
    if node.metrics.as_ref().is_some_and(pred) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_operator(child, pred))
}

/// The one operator node the frontier family ran: the clause whose
/// `frontier.levels > 0` marks the engagement — `None` on a tree the
/// family declined everywhere.
fn frontier_run(node: &relate::PlanNode) -> Option<&relate::PlanNode> {
    find_operator(node, &|m| m.frontier.levels > 0)
}

/// The frontier-NFA rung's own PROFILE facts on a SERVED run
/// (ADR-0478 §3, ADR-0516): the operator the family served reports the
/// run's counters — the BFS trio, the (node, state) cells, the
/// re-arrival rule's work spent (the budget fraction's numerator), the
/// memo's hits and misses, and the admission asks by verdict — pinned
/// exactly on the 6-ring's unbounded distinct-endpoint run: every
/// endpoint's first arrival proves by its representative chain (5
/// proofs) except the seed's own re-arrival (one search, proven), the
/// rule spends 2 of its million-unit budget, and nothing declines. The
/// wire args carry the same facts on the same row.
#[test]
fn profile_reports_the_frontier_rung_counters_on_a_served_run() {
    let mut conn = connection();
    let mut create = String::from("CREATE ");
    for i in 0..6 {
        create.push_str(&format!("(n{i}:Geo {{id:'n{i}'}}), "));
    }
    for i in 0..6 {
        let j = (i + 1) % 6;
        create.push_str(&format!("(n{i})-[:LINK]->(n{j}), (n{j})-[:LINK]->(n{i}), "));
    }
    create.truncate(create.len() - 2);
    conn.query(&create).unwrap();
    let served = conn
        .query("PROFILE MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo) RETURN count(DISTINCT y.id) AS c")
        .unwrap();
    assert_eq!(served.rows.len(), 1, "the distinct-endpoint count");
    let tree = plan(&served);
    let run = frontier_run(tree).expect("the frontier served the ring");
    let counters = &run.metrics.expect("the served operator's metrics").frontier;
    assert_eq!(counters.edge_visits, 24, "the ring's incident-edge visits");
    assert_eq!(counters.levels, 5, "the levels the BFS completed");
    assert_eq!(counters.endpoints, 6, "the endpoints the run accepted");
    assert_eq!(counters.cells, 6, "the (node, state) cells the run created");
    assert_eq!(counters.rearrival_work, 2, "the rule's spent work");
    assert_eq!(counters.memo_hits, 0, "no ask hit the verdict memo twice");
    assert_eq!(
        counters.memo_misses, 1,
        "the seed's re-arrival ran the search"
    );
    assert_eq!(
        counters.representative_proofs, 5,
        "every other endpoint proved by representative chain"
    );
    assert_eq!(counters.search_proven, 1, "the one search found the trail");
    assert_eq!(counters.search_disproven, 0, "nothing was disproved");
    assert_eq!(
        counters.declined_exhausted, 0,
        "no ask was cut by the budget"
    );
    assert_eq!(
        counters.budget_fraction(),
        2.0 / relate::REARRIVAL_WORK_BUDGET as f64,
        "the fraction is the spent work over the budget constant"
    );
    assert_eq!(
        run.metrics
            .expect("the served operator's metrics")
            .frontier_decline
            .map(|reason| reason.as_str()),
        None,
        "a served run declines nothing"
    );
    // The wire args on the same operator row: the typed values the
    // surfaces read — the fraction as the one float metric, the verdict
    // counts as integers, no decline reason beside a served run.
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_BUDGET_FRACTION),
        Some(relate::Value::Float(
            2.0 / relate::REARRIVAL_WORK_BUDGET as f64
        )),
        "the budget fraction rides the args"
    );
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_MEMO_MISSES),
        Some(relate::Value::Int(1)),
        "the memo misses ride the args"
    );
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_REPRESENTATIVE_PROOFS),
        Some(relate::Value::Int(5)),
        "the representative proofs ride the args"
    );
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_CELLS),
        Some(relate::Value::Int(6)),
        "the cells ride the args"
    );
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_DECLINE_REASON),
        None,
        "a served run carries no decline reason"
    );
}

/// The frontier-NFA rung's PROFILE facts on a DECLINED run (ADR-0516):
/// the LABS-952 clique shape at N = 28 — under `*4..4` every endpoint is
/// a below-min candidate, and the hopeless candidates' searches exhaust
/// the re-arrival rule's shared budget, so the rung declines mid-clause.
/// The declined run still reports its counters (it still walked): the
/// budget drained to exactly its constant (fraction `1.0`), the verdict
/// tally names the proven, disproven and cut asks, and the typed
/// decline reason rides the record and the wire args beside them —
/// while the statement still answers exactly (the DFS's 29).
#[test]
fn profile_reports_the_frontier_rung_counters_on_a_declined_run() {
    let mut conn = connection();
    conn.query("CREATE (:User {id:'s'}), (:User {id:'a'}), (:User {id:'b'}), (:User {id:'u'})")
        .unwrap();
    conn.query("UNWIND range(0, 27) AS i CREATE (:User {id:'c' + toString(i)})")
        .unwrap();
    conn.query(
        "MATCH (s:User {id:'s'}), (a:User {id:'a'}), (b:User {id:'b'}), (u:User {id:'u'}) \
         CREATE (s)-[:R]->(a), (a)-[:R]->(s), (s)-[:R]->(b), (b)-[:R]->(u)",
    )
    .unwrap();
    conn.query("MATCH (s:User {id:'s'}), (c:User) WHERE c.id STARTS WITH 'c' CREATE (s)-[:R]->(c)")
        .unwrap();
    conn.query(
        "MATCH (x:User), (y:User) \
         WHERE x.id STARTS WITH 'c' AND y.id STARTS WITH 'c' AND x.id < y.id \
         CREATE (x)-[:R]->(y), (y)-[:R]->(x)",
    )
    .unwrap();
    let declined = conn
        .query("PROFILE MATCH (s:User {id:'s'})-[:R*4..4]->(n:User) RETURN count(DISTINCT n) AS c")
        .unwrap();
    assert_eq!(
        declined.rows,
        vec![vec![relate::Value::Int(29)]],
        "the DFS's exact answer after the fall-through"
    );
    let tree = plan(&declined);
    let run = frontier_run(tree).expect("the frontier ran before it declined");
    let metrics = run.metrics.expect("the declined operator's metrics");
    let counters = &metrics.frontier;
    assert_eq!(counters.edge_visits, 788, "the clique's edge visits");
    assert_eq!(counters.levels, 4, "the levels the BFS completed");
    assert_eq!(
        counters.endpoints, 28,
        "the endpoints accepted before the cut"
    );
    assert_eq!(
        counters.cells, 32,
        "the (node, state) cells the run created"
    );
    assert_eq!(
        counters.rearrival_work,
        relate::REARRIVAL_WORK_BUDGET,
        "the hopeless search drained the budget to exactly its constant"
    );
    assert_eq!(counters.memo_hits, 0, "no ask hit the verdict memo twice");
    assert_eq!(
        counters.memo_misses, 32,
        "every candidate asked its own search"
    );
    assert_eq!(
        counters.representative_proofs, 0,
        "no in-bounds arrival proved by chain"
    );
    assert_eq!(
        counters.search_proven, 28,
        "the clique candidates' trails were proven"
    );
    assert_eq!(
        counters.search_disproven, 1,
        "one hopeless candidate searched to exhaustion"
    );
    assert_eq!(
        counters.declined_exhausted, 3,
        "the rest were cut by the drained budget"
    );
    assert_eq!(
        counters.budget_fraction(),
        1.0,
        "the drained budget is the whole budget"
    );
    assert_eq!(
        metrics.frontier_decline.map(|reason| reason.as_str()),
        Some("rearrival-budget"),
        "the typed run-time decline rides the record"
    );
    // The wire args on the same operator row: the drained-budget
    // fraction and the typed decline reason beside the counters.
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_BUDGET_FRACTION),
        Some(relate::Value::Float(1.0)),
        "the drained budget rides the args"
    );
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_DECLINE_REASON),
        Some(relate::Value::String("rearrival-budget".into())),
        "the typed decline reason rides the args"
    );
    assert_eq!(
        wire_arg(run, plan_keys::FRONTIER_DECLINED_EXHAUSTED),
        Some(relate::Value::Int(3)),
        "the cut asks ride the args"
    );
}

/// The first operator node whose record names a frontier decline — the
/// plan-time half (LABS-1437) leaves the counters zero, so the
/// engagement finder cannot locate it.
fn declined_node(node: &relate::PlanNode) -> Option<&relate::PlanNode> {
    find_operator(node, &|m| m.frontier_decline.is_some())
}

/// LABS-1437: a clause the frontier's plan-time gates keep off the rung
/// names the gate on the record and the wire args of the walker's row —
/// the sqlite-gate's co-change and k-hop reach shapes get distinct,
/// typed reasons (a bounded untyped hop clears the NFA compile now and
/// declines on the cost gate instead — LABS-1434; a statement that needs
/// every row declines the answer-shape gate), the counters stay zero
/// because the rung never ran, and the answer is the walker's.
#[test]
fn profile_names_the_plan_time_decline_on_a_walker_served_run() {
    let mut conn = connection();
    conn.query(
        "CREATE (a:Prompt:File {id:0}), (b:File {id:1}), (c:File {id:2}), \
         (a)-[:TOUCHES]->(b), (b)-[:TOUCHES]->(c), (c)-[:TOUCHES]->(a)",
    )
    .unwrap();
    for (query, reason) in [
        (
            "PROFILE MATCH (p:Prompt)-[*1..3]-(n) WHERE n <> p \
             WITH DISTINCT p, n RETURN count(*) AS pairs",
            "cost",
        ),
        (
            "PROFILE MATCH (a:File)-[:TOUCHES*1..2]-(b:File) \
             RETURN a.id AS a, b.id AS b",
            "full-enumeration",
        ),
        (
            "PROFILE MATCH (a:File)-[:TOUCHES*1..2]-(b:File) RETURN count(*) AS c",
            "path-count",
        ),
    ] {
        let result = conn.query(query).unwrap();
        let tree = plan(&result);
        let node = declined_node(tree).unwrap_or_else(|| panic!("a decline is recorded: {query}"));
        let metrics = node.metrics.expect("the walker's operator metrics");
        assert_eq!(
            metrics.frontier_decline.map(|reason| reason.as_str()),
            Some(reason),
            "the typed plan-time reason rides the record: {query}"
        );
        assert_eq!(
            metrics.frontier.levels, 0,
            "a plan-time decline never ran the rung: {query}"
        );
        assert_eq!(
            wire_arg(node, plan_keys::FRONTIER_DECLINE_REASON),
            Some(relate::Value::String(reason.into())),
            "the typed reason rides the wire args: {query}"
        );
        assert_eq!(
            wire_arg(node, plan_keys::FRONTIER_EDGES),
            None,
            "no run counters beside a plan-time reason: {query}"
        );
    }
}
