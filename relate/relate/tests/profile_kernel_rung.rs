//! LABS-1117: the PROFILE plan tree names the traversal rung that served
//! a `MATCH` clause — the chain kernel with its hop count, the hub-star,
//! the frontier-NFA rung, or the per-row walker — and the wire args carry
//! it on the same operator row. EXPLAIN carries the one static rung fact,
//! the store's `RELATE_PATTERN_ENGINE` mode.
//!
//! Two distinct shapes pin the record as the actual rung, never a
//! constant: a 3-hop fixed chain (the chain kernel) and an unbounded
//! quantifier whose answer is the distinct endpoint set (the frontier).

mod common;

use common::{connection, plan};

/// The operator whose metrics record a rung, at any depth — the clause a
/// kernel or the walker served.
fn served(node: &relate::PlanNode) -> Option<&relate::PlanNode> {
    if node.metrics.as_ref().is_some_and(|m| m.kernel.is_some()) {
        return Some(node);
    }
    node.children.iter().find_map(served)
}

/// One wire arg of a plan node: its serialized `args` map entry.
fn wire_arg(node: &relate::PlanNode, key: &str) -> Option<relate::Value> {
    let relate::Value::Map(map) = node.to_value() else {
        panic!("to_value is a map");
    };
    let Some(relate::Value::Map(args)) = map.get("args") else {
        panic!("args is a map");
    };
    args.get(key).cloned()
}

/// A six-node bidirectional ring: an unbounded quantifier's trail
/// enumeration fans out while its endpoint set stays tiny.
fn ring(conn: &mut relate::Connection) {
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
}

#[test]
fn profile_names_the_chain_kernel_and_its_hops() {
    let mut conn = connection();
    conn.query(relate_test_support::plan_rung::THREE_HOP_CORRIDOR)
        .unwrap();
    let result = conn
        .query(&format!(
            "PROFILE {}",
            relate_test_support::plan_rung::THREE_HOP_MATCH
        ))
        .unwrap();
    let node = served(plan(&result)).expect("a rung served the MATCH clause");
    assert_eq!(
        node.metrics.as_ref().and_then(|m| m.kernel),
        Some(relate::KernelRung::Chain { hops: 3 }),
        "the fixed 3-hop chain runs the chain kernel"
    );
    assert_eq!(
        wire_arg(node, "KernelRung"),
        Some(relate::Value::String("chain".into())),
    );
    assert_eq!(wire_arg(node, "KernelHops"), Some(relate::Value::Int(3)));
}

#[test]
fn profile_names_the_frontier_rung_for_a_varlen_pattern() {
    let mut conn = connection();
    ring(&mut conn);
    let result = conn
        .query(
            "PROFILE MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo) \
             RETURN count(DISTINCT y.id) AS c",
        )
        .unwrap();
    let node = served(plan(&result)).expect("a rung served the MATCH clause");
    assert_eq!(
        node.metrics.as_ref().and_then(|m| m.kernel),
        Some(relate::KernelRung::FrontierNfa),
        "the unbounded distinct-endpoint shape runs the frontier rung"
    );
    assert_eq!(
        wire_arg(node, "KernelRung"),
        Some(relate::Value::String("frontier-nfa".into())),
    );
    assert_eq!(
        wire_arg(node, "KernelHops"),
        None,
        "only the chain rung carries a hop count"
    );
}

/// EXPLAIN cannot name the runtime rung, but it names the one static
/// choice: the store's pattern-engine mode, on the MATCH operator.
/// The first node (pre-order) carrying the pattern-engine arg — the
/// MATCH operator, wherever it sits in the tree.
fn pattern_engine(node: &relate::PlanNode) -> Option<relate::Value> {
    wire_arg(node, "PatternEngine").or_else(|| node.children.iter().find_map(pattern_engine))
}

#[test]
fn explain_reports_the_pattern_engine_rung() {
    let mut conn = connection();
    conn.query("CREATE (:N {id: 0})").unwrap();
    let result = conn
        .query("EXPLAIN MATCH (n:N)-[:E]->(m:N) RETURN m")
        .unwrap();
    assert_eq!(
        pattern_engine(plan(&result)),
        Some(relate::Value::String("auto".into())),
        "EXPLAIN names the store's pattern-engine rung on the MATCH operator"
    );
}

/// A `SEARCH` `MATCH` runs the per-row walker directly
/// (`search_match_clause`), never the ladder, so its record names the
/// walker — the rung that actually served the pattern (LABS-1117).
#[test]
fn a_search_match_reports_the_walker() {
    let mut conn = connection();
    conn.query("CREATE VECTOR INDEX vidx FOR (p:Person) ON p.embed")
        .unwrap();
    conn.query(
        "CREATE (:Person {name: 'a', embed: [1.0, 0.0]}), \
                (:Person {name: 'b', embed: [0.0, 1.0]})",
    )
    .unwrap();
    let result = conn
        .query(
            "PROFILE MATCH (p:Person) \
             SEARCH p IN (VECTOR INDEX vidx FOR [1.0, 0.0] LIMIT 2) \
             RETURN p.name AS name",
        )
        .unwrap();
    let node = served(plan(&result)).expect("the MATCH clause records its rung");
    assert_eq!(
        node.metrics.as_ref().and_then(|m| m.kernel),
        Some(relate::KernelRung::Walker),
        "a SEARCH MATCH walks the pattern with the per-row walker"
    );
    assert_eq!(
        wire_arg(node, "KernelRung"),
        Some(relate::Value::String("walker".into())),
    );
}
