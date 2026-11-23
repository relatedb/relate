//! PROFILE's expression-memo group (ADR-0654) end to end through the
//! facade: a guarded traversal over a high-fan-in graph reports its stage
//! memo on the operator that ran it, the constructor memos report at the
//! plan root with their kinds, a memo-free statement carries no group, and
//! the wire args, the plan table and the `relate.plan/v1` document carry
//! the same numbers.

mod common;

use common::{connection, open_with_engine, plan, wire_arg};
use relate_model::wire::plan_keys;

const WIDTH: usize = 300;
const LAYERS: usize = 8;

/// One `CREATE` of the layered DAG the executor's memo wiring tests use:
/// `LAYERS` layers of `WIDTH` nodes, each linked to three nodes of the next
/// layer, so a guarded traversal re-reaches the same nodes along many
/// routes and its guard memos pass their warm-up.
fn layered_create() -> String {
    let mut parts = Vec::new();
    for layer in 0..LAYERS {
        for i in 0..WIDTH {
            let labels = match (i + layer) % 6 {
                0 => ":V",
                1 => ":W",
                2 => ":U:V",
                _ => ":U",
            };
            let hub = if layer == 0 && i % 7 == 0 { ":Hub" } else { "" };
            parts.push(format!(
                "(l{layer}n{i}{labels}{hub} {{id: 'l{layer}n{i}'}})"
            ));
        }
    }
    for layer in 0..LAYERS - 1 {
        for i in 0..WIDTH {
            for step in [1, 17, 101] {
                parts.push(format!(
                    "(l{layer}n{i})-[:E]->(l{}n{})",
                    layer + 1,
                    (i * 3 + step) % WIDTH
                ));
            }
        }
    }
    format!("CREATE {}", parts.join(", "))
}

const GUARDED: &str = "MATCH (a:Hub)((m)-[:E]->(n:U)){1,}(y:V) RETURN count(DISTINCT y) AS c";

fn find<'a>(
    node: &'a relate::PlanNode,
    pred: &dyn Fn(&relate::ClauseMetrics) -> bool,
) -> Option<&'a relate::PlanNode> {
    if node.metrics.as_ref().is_some_and(pred) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, pred))
}

fn int(node: &relate::PlanNode, key: &str) -> i64 {
    match wire_arg(node, key) {
        Some(relate::Value::Int(n)) => n,
        other => panic!("{key} is an integer, got {other:?}"),
    }
}

#[test]
fn a_guarded_traversal_carries_the_memo_group_on_its_operator() {
    // The frontier rung's guarded states are the memo's site; forcing it
    // keeps the pin independent of the cost gate's routing.
    let mut conn = open_with_engine("nfa");
    conn.query(&layered_create()).unwrap();
    let plain = conn.query(GUARDED).unwrap();
    let profiled = conn.query(&format!("PROFILE {GUARDED}")).unwrap();
    assert_eq!(profiled.rows, plain.rows, "PROFILE changes no answer");
    let tree = plan(&profiled);
    let owner = find(tree, &|m| m.memo.probes > 0).expect("a stage ran guard memos");
    let memo = owner.metrics.unwrap().memo;
    assert!(memo.hits > 0 && memo.hits <= memo.probes, "{memo:?}");
    assert!(memo.bytes > 0, "{memo:?}");
    // The wire carries the same numbers, presence-only, bytes apart from
    // the row bytes.
    assert_eq!(int(owner, plan_keys::MEMO_PROBES), memo.probes as i64);
    assert_eq!(int(owner, plan_keys::MEMO_HITS), memo.hits as i64);
    assert_eq!(int(owner, plan_keys::MEMO_BYTES), memo.bytes as i64);
    assert_eq!(
        int(owner, plan_keys::MEMORY_BYTES),
        owner.metrics.unwrap().memory_bytes as i64,
        "memo bytes are not memory bytes"
    );
    // The document the surfaces hand out reads the same group.
    let doc = relate::plan_wire_json(&profiled)
        .unwrap()
        .expect("PROFILE carries a plan");
    let mut stack = vec![&doc["root"]];
    let mut seen = 0;
    while let Some(node) = stack.pop() {
        if let Some(group) = node["actual"].get("memo") {
            seen += 1;
            assert_eq!(group["probes"], memo.probes);
            assert_eq!(group["hits"], memo.hits);
            assert_eq!(group["bytes"], memo.bytes);
        }
        stack.extend(node["children"].as_array().into_iter().flatten());
    }
    assert_eq!(seen, 1, "exactly one operator carries the group");
}

#[test]
fn a_memo_free_statement_carries_no_group_and_no_column() {
    let mut conn = connection();
    conn.query("CREATE (:N {id: 1})-[:E]->(:N {id: 2})")
        .unwrap();
    let profiled = conn
        .query("PROFILE MATCH (a:N)-[:E]->(b:N) RETURN count(*) AS c")
        .unwrap();
    let tree = plan(&profiled);
    assert!(find(tree, &|m| m.memo.probes > 0).is_none());
    assert!(wire_arg(tree, plan_keys::MEMO_PROBES).is_none());
    assert!(wire_arg(tree, plan_keys::MEMO_KINDS).is_none());
    let doc = relate::plan_wire_json(&profiled).unwrap().unwrap();
    assert!(doc["root"]["actual"].get("memo").is_none());
}

#[test]
fn repeated_datetime_strings_report_a_datetime_memo_at_the_root() {
    let mut conn = connection();
    let q = "UNWIND range(0, 1999) AS i \
             WITH ['2024-05-05T10:00:00Z', '2023-01-01T09:00:00Z'][i % 2] AS s \
             RETURN count(datetime(s)) AS n";
    let profiled = conn.query(&format!("PROFILE {q}")).unwrap();
    let root = plan(&profiled);
    let memo = root.metrics.unwrap().memo;
    assert!(memo.probes > 0 && memo.hits > 0, "{memo:?}");
    assert_eq!(int(root, plan_keys::MEMO_HITS), memo.hits as i64);
    let Some(relate::Value::List(kinds)) = wire_arg(root, plan_keys::MEMO_KINDS) else {
        panic!("the root names the memo types");
    };
    assert_eq!(
        kinds.to_vec(),
        vec![relate::Value::String("datetime".into())]
    );
    let doc = relate::plan_wire_json(&profiled).unwrap().unwrap();
    assert_eq!(
        doc["root"]["actual"]["memo"]["kinds"],
        serde_json::json!(["datetime"])
    );
    // Only the root carries the kinds.
    for child in &root.children {
        assert!(wire_arg(child, plan_keys::MEMO_KINDS).is_none());
    }
}

#[test]
fn explain_carries_no_memo_group() {
    let mut conn = connection();
    let explained = conn.query(&format!("EXPLAIN {GUARDED}")).unwrap();
    let tree = plan(&explained);
    assert!(wire_arg(tree, plan_keys::MEMO_PROBES).is_none());
}
