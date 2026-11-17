//! The Accept/media-type negotiation and typed value-encoding tests,
//! split from `query_api_tests.rs` — one concern per file.

use super::tests::*;
use super::*;
use crate::v2_envelope::tests::body_json;

#[test]
fn accept_negotiates_the_content_type_and_never_406s_the_browser() {
    let db = db();
    let mut req = post("neo4j", r#"{"statement": "RETURN 1 AS n"}"#);
    // Neo4j Browser's actual Accept list (the versioned JSONL variant
    // first, then the JSON forms): answered with the Query API v2 media
    // type and the TYPED encoding — Browser's client requires that content
    // type (it rejects `application/json`) and decodes the tagged values
    // (LABS-376). The request keeps its plain content type (ADR-0425:
    // the Accept negotiates the RESPONSE; the request type is orthogonal).
    req.headers.push(("accept".into(), BROWSER_ACCEPT.into()));
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_type, "application/vnd.neo4j.query");
    assert_eq!(
        body_json(&reply)["data"]["values"],
        json!([[{"$type": "Integer", "_value": "1"}]])
    );
    // Plain JSON clients and no Accept at all: application/json.
    req.headers.retain(|(name, _)| name != "accept");
    req.headers
        .push(("accept".into(), "application/json".into()));
    assert_eq!(
        dispatch(&db, &req, OPEN, &txs()).content_type,
        "application/json"
    );
    req.headers.retain(|(name, _)| name != "accept");
    assert_eq!(
        dispatch(&db, &req, OPEN, &txs()).content_type,
        "application/json"
    );
    // A client asking only for something we cannot produce: 406 (and
    // the error envelope stays application/json).
    req.headers
        .push(("accept".into(), "application/xml".into()));
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 406, "application/xml");
    assert_eq!(reply.content_type, "application/json");
}

#[test]
fn typed_and_plain_value_encoding_follow_the_neo4j_contract() {
    let db = db();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:City {name: 'Oslo', founded: 1048})-[:FOUNDED_IN]->(:Era {y: 1048})")
        .unwrap();
    let read = |accept: &str| {
        let mut req = post("neo4j", r#"{"statement": "MATCH (c:City) RETURN c"}"#);
        req.headers.push(("accept".into(), accept.into()));
        dispatch(&db, &req, OPEN, &txs())
    };
    // Plain (application/json): the entity is a plain object.
    let reply = read("application/json");
    let body = body_json(&reply);
    let node = &body["data"]["values"][0][0];
    assert_eq!(node["labels"], json!(["City"]));
    assert!(node["elementId"].is_string(), "{node}");
    assert_eq!(node["properties"]["name"], json!("Oslo"));
    assert!(node.get("$type").is_none() && node.get("kind").is_none());
    // Typed (vnd.neo4j.query): the $type/_value wrap with the Neo4j
    // field names, integers as strings.
    let reply = read("application/vnd.neo4j.query");
    assert_eq!(reply.content_type, "application/vnd.neo4j.query");
    let body = body_json(&reply);
    let node = &body["data"]["values"][0][0];
    assert_eq!(node["$type"], json!("Node"));
    assert!(node["_value"]["_element_id"].is_string(), "{node}");
    assert_eq!(node["_value"]["_labels"], json!(["City"]));
    assert_eq!(
        node["_value"]["_properties"]["founded"],
        json!({"$type": "Integer", "_value": "1048"})
    );
}

#[test]
fn request_dialect_matrix_follows_the_real_servers() {
    let db = db();
    // The versioned request: the envelope parameter decodes — a temporal
    // payload lands as its ISO-8601 string (the client contract: the
    // typed value is built in Cypher, e.g. date($d)).
    let req = crate::query_api::tests::post_typed(
        "neo4j",
        r#"{"statement": "RETURN date($d) = date('2026-10-25') AS same",
            "parameters": {"d": {"$type": "Date", "_value": "2026-10-25"}}}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 200, "{}", body_json(&reply));
    assert_eq!(body_json(&reply)["data"]["values"], json!([[true]]));
    // Plain application/json: the same envelope is a user map — its
    // `_value` entry is reachable as a map lookup, not a typed value.
    let req = post(
        "neo4j",
        r#"{"statement": "RETURN $d['_value'] AS x",
            "parameters": {"d": {"$type": "Date", "_value": "2026-10-25"}}}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 200, "{}", body_json(&reply));
    assert_eq!(body_json(&reply)["data"]["values"], json!([["2026-10-25"]]));
    for content_type in [
        "application/vnd.neo4j.query",
        "application/vnd.neo4j.query.v1.0",
        "application/vnd.neo4j.query.v1.2",
        "Application/Vnd.Neo4j.Query; charset=utf-8",
    ] {
        let req = post_with_type(
            "neo4j",
            Some(content_type),
            r#"{"statement": "RETURN $n AS n, $items AS items", "parameters": {
                "n": {"$type": "Integer", "_value": "42"},
                "items": {"$type": "List", "_value": [
                    {"$type": "Map", "_value": {
                        "name": {"$type": "String", "_value": "Oslo"}
                    }}
                ]}
            }}"#,
        );
        let reply = dispatch(&db, &req, OPEN, &txs());
        assert_eq!(reply.status, 200, "{content_type}: {}", body_json(&reply));
        assert_eq!(
            body_json(&reply)["data"]["values"],
            json!([[42, [{"name": "Oslo"}]]]),
            "{content_type}"
        );
    }
    for content_type in [
        None,
        Some("application/xml"),
        Some("application/vnd.neo4j.querying"),
    ] {
        let req = post_with_type("neo4j", content_type, r#"{"statement": "RETURN 1"}"#);
        let reply = dispatch(&db, &req, OPEN, &txs());
        assert_eq!(reply.status, 400, "{content_type:?}: {}", body_json(&reply));
        assert_eq!(body_json(&reply)["errors"][0]["code"], "42000");
    }
}

#[test]
fn unversioned_requests_decode_parameters_on_every_transaction_route() {
    let db = db();
    let txs = txs();
    let request = |path: &str, value: i64| {
        let body = json!({
            "statement": "RETURN $n AS n",
            "parameters": {"n": {"$type": "Integer", "_value": value.to_string()}},
            "accessMode": "READ"
        });
        tx_post_browser(path, &body.to_string())
    };
    let begin = dispatch(&db, &request("/db/neo4j/query/v2/tx", 42), OPEN, &txs);
    assert_eq!(begin.status, 201, "{}", body_json(&begin));
    assert_eq!(
        body_json(&begin)["data"]["values"],
        json!([[{"$type": "Integer", "_value": "42"}]])
    );
    let id = tx_id(&begin);
    for (suffix, value) in [("", 43), ("/commit", 44)] {
        let path = format!("/db/neo4j/query/v2/tx/{id}{suffix}");
        let reply = dispatch(&db, &request(&path, value), OPEN, &txs);
        assert_eq!(reply.status, 200, "{path}: {}", body_json(&reply));
        assert_eq!(reply.content_type, "application/vnd.neo4j.query");
        assert_eq!(
            body_json(&reply)["data"]["values"],
            json!([[{"$type": "Integer", "_value": value.to_string()}]])
        );
    }
}

/// ADR-0425's code split, pinned at the route: parameter DECODE failures
/// carry the class-less general code (42000) — ParameterMissing's 42N02
/// stays for a statement referencing a parameter absent from
/// `parameters`.
#[test]
fn undecodable_parameters_fail_with_the_general_code() {
    let db = db();
    let req = crate::query_api::tests::post_typed(
        "neo4j",
        r#"{"statement": "RETURN $x",
            "parameters": {"x": {"$type": "Frobnicate", "_value": 1}}}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 400);
    let body = body_json(&reply);
    assert_eq!(body["errors"][0]["code"], "42000");
    assert!(body["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("unknown typed parameter"));
}

#[test]
fn typed_request_parameters_decode() {
    // Browser's metadata sidebar shape: a typed Integer parameter
    // feeding a slice bound (this was the 22G03 "slice end" failure —
    // the typed map used to flow through as a map).
    let db = db();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:City {name: 'Oslo'}), (:City {name: 'Paris'})")
        .unwrap();
    drop(conn);
    // ADR-0425: envelope parameters require the versioned request type —
    // under plain application/json this body's `itemLimit` is a map, the
    // real servers' behavior.
    let req = crate::query_api::tests::post_typed(
        "neo4j",
        r#"{"statement": "MATCH (c:City) RETURN c.name AS n ORDER BY n",
                "parameters": {"itemLimit": {"$type": "Integer", "_value": "1"}}}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 200, "{}", body_json(&reply));
    // The typed parameter decoded to 1; use it as a slice bound the way
    // the sidebar query does.
    let req = crate::query_api::tests::post_typed(
        "neo4j",
        r#"{"statement": "MATCH (c:City) RETURN collect(c.name)[..$itemLimit] AS result",
                "parameters": {"itemLimit": {"$type": "Integer", "_value": "1"}}}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    let body = body_json(&reply);
    assert_eq!(reply.status, 200, "{body}");
    assert_eq!(body["data"]["values"], json!([[["Oslo"]]]));
    // A parameter object with extra keys stays a map (ambiguity rule).
    let req = crate::query_api::tests::post_typed(
        "neo4j",
        r#"{"statement": "RETURN $m.other AS x",
                "parameters": {"m": {"$type": "Integer", "_value": "10", "other": 7}}}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    let body = body_json(&reply);
    assert_eq!(reply.status, 200, "{body}");
    assert_eq!(body["data"]["values"], json!([[7]]));
    // An unknown $type is a clear 400.
    let req = crate::query_api::tests::post_typed(
        "neo4j",
        r#"{"statement": "RETURN 1",
                "parameters": {"m": {"$type": "Wat", "_value": 1}}}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 400);
    assert!(body_json(&reply)["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("unknown typed parameter"));
}

#[test]
fn the_exact_browser_connectivity_probe_decodes() {
    // Michael's failing request, verbatim shape: the Browser Accept list
    // is answered with the Query API v2 media type and the typed body —
    // what Browser's client requires (it rejects `application/json`), and
    // the encoding it decodes (LABS-376).
    let db = db();
    let req = post_browser(
        "system",
        r#"{"statement":"CALL db.ping()","includeCounters":true,"accessMode":"READ"}"#,
    );
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_type, "application/vnd.neo4j.query");
    let body = body_json(&reply);
    assert_eq!(body["data"]["fields"], json!(["success"]));
    assert_eq!(
        body["data"]["values"],
        json!([[{"$type": "Boolean", "_value": true}]])
    );
    assert!(body["counters"]["nodes_created"].is_number());
    assert_eq!(body["bookmarks"], json!([]));
}

#[test]
fn the_typed_plan_keeps_every_structural_field_plain() {
    // The typed media type (what Browser negotiates) decodes with its
    // `$type` codec: every STRUCTURAL plan field — `arguments`,
    // `identifiers`, `children`, `warnings` — must stay a plain container,
    // or the client meets a tagged one and throws `Unknown type:
    // undefined` (LABS-376). The warned query pins `warnings` too.
    let db = db();
    // The label-less scan warns only while a property index exists to
    // suggest (the engine's own warning test's setup).
    dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement":"CREATE INDEX person_name FOR (n:Person) ON (n.name)"}"#,
        ),
        OPEN,
        &txs(),
    );
    let req = post_browser("neo4j", r#"{"statement":"EXPLAIN MATCH (n) RETURN n"}"#);
    // The Browser Accept list, verbatim in the builder: the typed media
    // type must be NEGOTIATED (the content type alone does not select
    // the typed encoding — the test would pass vacuously without it).
    let reply = dispatch(&db, &req, OPEN, &txs());
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_type, "application/vnd.neo4j.query");
    let body = body_json(&reply);
    let plan = &body["queryPlan"];
    for field in ["arguments", "identifiers", "children", "warnings"] {
        let value = &plan[field];
        if value.is_null() {
            continue;
        }
        assert!(
            value.get("$type").is_none(),
            "{field} is a plain container under the typed encoding: {value:?}"
        );
    }
    assert!(plan["arguments"].is_object());
    assert!(plan["identifiers"].is_array());
    assert!(plan["dbHits"].is_number());
    assert!(plan["time"].is_number());
    // The AllNodesScan warning rides the scan node (the root's child),
    // plain like every other structural field.
    let warnings = &plan["children"][0]["warnings"];
    assert!(warnings.is_array(), "the scan warns: {plan:?}");
    assert!(warnings[0].get("$type").is_none(), "{warnings:?}");
}
