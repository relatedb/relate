//! The v2 plan projection tests: `queryPlan` / `profiledQueryPlan` over the
//! Query API, split from `query_api_tests.rs` — one concern per file.

use super::tests::{db, dispatch, post, txs, OPEN};
use super::*;
use crate::v2_envelope::tests::body_json;

/// LABS-376: the v2 node walks the engine's UNDECORATED wire form — the
/// decorated root's `EstimatedRows` is a Neo4j-facing float that the
/// engine-form accessor rejects (ADR-0345), so handing the decorated value
/// to the walk would panic on any estimated plan.
#[test]
fn an_estimated_plan_serves_over_http() {
    let db = db();
    let reply = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "EXPLAIN MATCH (n:Person) RETURN n.name"}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    let plan = &body["queryPlan"];
    let scan = &plan["children"][0];
    assert_eq!(scan["operatorType"], json!("NodeByLabelScan@relate"));
    // The engine's own integer estimate rides `arguments` — the walk read
    // the undecorated form, so no float reached the strict accessor.
    assert!(
        scan["arguments"]["EstimatedRows"].is_i64() || scan["arguments"]["EstimatedRows"].is_null(),
        "the estimate is the engine's integer form: {scan:?}"
    );
}

/// ADR-0486: an index-backed start narrates the seek over HTTP too —
/// `NodeIndexSeekByRange@relate` in the v2 plan shape, the seek line
/// in `Details` and the posting window in `EstimatedRows`, the same
/// engine wire form every surface reads.
#[test]
fn an_index_backed_start_narrates_the_seek_over_http() {
    let db = db();
    {
        let mut conn = db.connect().unwrap();
        conn.query("CREATE RANGE INDEX user_age FOR (u:User) ON (u.age)")
            .unwrap();
        conn.query("UNWIND range(1, 10) AS i CREATE (:User {age: i})")
            .unwrap();
    }
    let reply = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "EXPLAIN MATCH (n:User) WHERE n.age > 5 RETURN n"}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    let seek = &body["queryPlan"]["children"][0];
    assert_eq!(seek["operatorType"], json!("NodeIndexSeekByRange@relate"));
    assert_eq!(
        seek["arguments"]["Details"],
        json!("RANGE INDEX n:User(age) WHERE age > 5, WHERE filter")
    );
    assert_eq!(seek["arguments"]["EstimatedRows"], json!(5));
}

/// Virtual plan operators: an index-seek-started MATCH's v2 plan lists the
/// fused children, each carrying `FusedInto` (the parent
/// operator's id) in its `arguments` — the engine's own wire args.
#[test]
fn explain_plan_lists_the_fused_children_over_http() {
    let db = db();
    {
        let mut conn = db.connect().unwrap();
        for statement in relate_test_support::fused_seek::FUSED_SEEK_SEED {
            conn.query(statement).unwrap();
        }
    }
    let body = serde_json::json!({
        "statement": format!("EXPLAIN {}", relate_test_support::fused_seek::FUSED_SEEK_MATCH)
    })
    .to_string();
    let reply = dispatch(&db, &post("neo4j", &body), OPEN, &txs());
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    let parent = &body["queryPlan"]["children"][0];
    let parent_args = parent["arguments"]
        .as_object()
        .unwrap_or_else(|| panic!("the fused parent carries an arguments map: {parent:?}"));
    assert!(
        !parent_args.contains_key("FusedInto"),
        "the parent is a real operator: {parent:?}"
    );
    let parent_id = parent_args["Id"].as_i64().expect("parent id");
    let fused = parent["children"].as_array().expect("fused children");
    assert_eq!(fused.len(), 2, "the seek step plus one hop: {parent:?}");
    for child in fused {
        assert_eq!(
            child["arguments"]["FusedInto"].as_i64(),
            Some(parent_id),
            "every fused child names the parent's id: {child:?}"
        );
    }
}

/// LABS-1117: the v2 plan's `arguments` carry the per-clause traversal
/// rung (`KernelRung`, with the chain's `KernelHops`) and the static
/// pattern-engine mode (`PatternEngine`) on the MATCH operator — the
/// shared plan presentation's args, no HTTP-specific encoding.
#[test]
fn profile_plan_arguments_carry_the_traversal_rung() {
    let db = db();
    {
        let mut conn = db.connect().unwrap();
        conn.query(relate_test_support::plan_rung::THREE_HOP_CORRIDOR)
            .unwrap();
    }
    let statement = format!(
        "PROFILE {}",
        relate_test_support::plan_rung::THREE_HOP_MATCH
    );
    let body = serde_json::json!({ "statement": statement }).to_string();
    let reply = dispatch(&db, &post("neo4j", &body), OPEN, &txs());
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    let plan = &body["profiledQueryPlan"];

    /// The first node (pre-order) whose `arguments` carry `key`.
    fn arg<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
        if let Some(value) = node.get("arguments").and_then(|args| args.get(key)) {
            return Some(value);
        }
        node.get("children")
            .and_then(serde_json::Value::as_array)
            .and_then(|children| children.iter().find_map(|child| arg(child, key)))
    }

    assert_eq!(
        arg(plan, "KernelRung"),
        Some(&json!("chain")),
        "the v2 plan names the chain rung: {plan:?}"
    );
    assert_eq!(arg(plan, "KernelHops"), Some(&json!(3)));
    assert_eq!(
        arg(plan, "PatternEngine"),
        Some(&json!("auto")),
        "the static pattern-engine mode rides the MATCH operator: {plan:?}"
    );
}

#[test]
fn explain_carries_query_plan_and_profile_carries_profiled_query_plan() {
    let db = db();
    // EXPLAIN: the envelope carries `queryPlan` in the Neo4j Query API v2
    // node shape (operatorType with this engine's suffix / arguments /
    // identifiers / children / the metric fields), never
    // `profiledQueryPlan`.
    let explain = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "EXPLAIN MATCH (n) RETURN n.name"}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(explain.status, 200);
    let explain_body = body_json(&explain);
    let plan = &explain_body["queryPlan"];
    assert_eq!(plan["operatorType"], json!("ProduceResults@relate"));
    assert_eq!(plan["arguments"]["Id"], json!(0));
    // The metrics ride as node fields, like Neo4j's own v2 response.
    assert!(plan["dbHits"].is_number());
    assert!(plan["records"].is_number());
    assert!(plan["time"].is_number());
    assert_eq!(plan["hasPageCacheStats"], json!(false));
    // A bare node pattern is a scan (ADR-0344 phase 3 subjects).
    assert_eq!(
        plan["children"][0]["operatorType"],
        json!("AllNodesScan@relate")
    );
    assert!(plan["identifiers"].is_array());
    assert!(explain_body.get("profiledQueryPlan").is_none());

    // PROFILE: the envelope carries `profiledQueryPlan`, the same
    // node shape plus the per-operator metric fields (node-level
    // `records`/`time`/`dbHits` and the engine's own `arguments` entries),
    // never `queryPlan`.
    let profile = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "PROFILE CREATE (n:N {name: 'A'})"}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(profile.status, 200);
    let profile_body = body_json(&profile);
    let profiled = &profile_body["profiledQueryPlan"];
    assert_eq!(profiled["operatorType"], json!("Create@relate"));
    assert!(profiled["records"].is_number());
    assert!(profiled["time"].is_number());
    // The metrics ride BOTH the node fields and the engine's richer
    // `arguments` entries — Neo4j keeps its `Rows`/`DbHits` in
    // `arguments` too (its `Memory`/`GlobalMemory` live there), so the
    // richer internal form survives the projection at this seam.
    assert!(profiled["arguments"]["Rows"].is_number());
    assert!(profiled["arguments"]["TimeNs"].is_number());
    assert!(profiled["arguments"]["Id"].is_i64());
    // The plan's structural fields are PLAIN containers with typed LEAVES
    // (Neo4j's shape): the client iterates `arguments` and decodes each
    // value, so a tagged Map here would expose `$type`/`_value` entries and
    // throw `Unknown type: undefined` (LABS-376).
    assert!(
        profiled["arguments"].is_object() && profiled["arguments"].get("$type").is_none(),
        "arguments is a plain object (never a tagged Map): {profiled:?}"
    );
    assert!(
        profiled["identifiers"].is_array()
            && profiled["identifiers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i.is_string()),
        "identifiers is a plain array of strings: {profiled:?}"
    );
    assert!(profile_body.get("queryPlan").is_none());

    // A plain run carries neither key.
    let plain = dispatch(
        &db,
        &post("neo4j", r#"{"statement": "RETURN 1 AS n"}"#),
        OPEN,
        &txs(),
    );
    let plain_body = body_json(&plain);
    assert!(plain_body.get("queryPlan").is_none());
    assert!(plain_body.get("profiledQueryPlan").is_none());
}

#[test]
fn query_plan_field_projects_the_plain_json_node() {
    let db = db();
    let mut conn = db.connect().unwrap();
    let explained = conn.query("EXPLAIN RETURN 1 AS n").unwrap();
    let (key, node) = crate::query_plan_field(explained.plan_root().unwrap()).unwrap();
    assert_eq!(key, "queryPlan");
    assert_eq!(node["operatorType"], json!("ProduceResults@relate"));
    assert!(node["arguments"].is_object());
    assert!(node["arguments"].get("$type").is_none());

    let profiled = conn.query("PROFILE RETURN 1 AS n").unwrap();
    let (key, node) = crate::query_plan_field(profiled.plan_root().unwrap()).unwrap();
    assert_eq!(key, "profiledQueryPlan");
    assert!(node["records"].is_number());
    assert!(node["arguments"].get("$type").is_none());
}
