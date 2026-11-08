//! The Query API v2 request handlers' tests: end-to-end `Request` →
//! `Reply` assertions through the public `handle` entry point.

use super::*;
use crate::v2_envelope::tests::body_json;
use relate_auth::AuthConfig;

pub(super) fn db() -> Database {
    Database::open(":memory:").unwrap()
}

/// Auth off (the request sails through) / auth on (the tests' `post`
/// carries no credential, so it is challenged).
pub(super) fn txs() -> TxRegistry {
    TxRegistry::new()
}

/// The disconnected two-pattern store's fixture: one `:Person` and one
/// `:City`. A label absent from the catalog would add its own unknown-label
/// warning beside the statement's.
pub(super) fn seed_person_and_city(db: &Database) {
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:Person)").unwrap();
    conn.query("CREATE (:City)").unwrap();
}

/// Dispatch one in-process request through the route: `handle` with the
/// discovery classification this direct caller computes itself (the wire
/// path reads the gate's carried flag instead).
pub(super) fn dispatch(
    db: &Database,
    request: &Request,
    auth: &relate_auth::AuthConfig,
    txs: &TxRegistry,
) -> Reply {
    handle(
        db,
        request,
        auth,
        None,
        txs,
        CredentialGate::Unchecked {
            public: is_discovery_probe(request),
        },
        None,
    )
}

/// Neo4j Browser's actual Accept list, verbatim (LABS-376): the versioned
/// JSONL variant first, then the JSON forms. The one spelling — every
/// Browser-scenario test consults it.
pub(crate) const BROWSER_ACCEPT: &str =
    "application/vnd.neo4j.query.v1.0+jsonl, application/vnd.neo4j.query, application/json";

/// The Browser request header pair (ADR-0425's request dialect): the
/// versioned content type plus the LABS-376 Accept list — one spelling
/// for every Browser-scenario test.
pub(super) fn browser_request_headers() -> Vec<(String, String)> {
    vec![
        (
            "content-type".to_string(),
            "application/vnd.neo4j.query.v1.2".to_string(),
        ),
        ("accept".to_string(), BROWSER_ACCEPT.to_string()),
    ]
}

/// A statement POST at an explicit path with an explicit content type
/// (or none) — the one in-process request construction the fixed-dialect
/// helpers and the fail-closed cases share (ADR-0425: a statement POST
/// carries a content type; the negative cases spell the missing and the
/// unversioned one).
fn tx_post_with_type(path: &str, content_type: Option<&str>, body: &str) -> Request {
    let headers: Vec<(&str, &str)> = content_type
        .map(|value| vec![("content-type", value)])
        .unwrap_or_default();
    crate::http::tests::head_at("POST", path, &headers).into_request(body.as_bytes().to_vec())
}

/// A statement POST in the plain request dialect at an explicit path —
/// the content type the plain-JSON bodies ride; the tx-route tests use
/// it, the auto-commit route goes through [`post`].
pub(super) fn tx_post(path: &str, body: &str) -> Request {
    tx_post_with_type(path, Some(crate::http::APPLICATION_JSON), body)
}

/// The rollback route's request for transaction `id`: a body-less DELETE at
/// the transaction's path — the one spelling every expiry and lease test
/// uses, so no call site rebuilds the method flip.
pub(super) fn rollback_request(id: &str) -> Request {
    let mut request = tx_post(&format!("/db/neo4j/query/v2/tx/{id}"), "");
    request.method = "DELETE".into();
    request
}

/// The auto-commit query route's statement POST: the one place its path
/// is spelled — the explicit content type (typed, plain, or none; the
/// fail-closed negative cases ride the absent one) is the parameter,
/// and [`post`] / [`post_typed`] are the fixed-dialect delegates.
pub(super) fn post_with_type(database: &str, content_type: Option<&str>, body: &str) -> Request {
    tx_post_with_type(&format!("/db/{database}/query/v2"), content_type, body)
}

/// A typed-dialect statement POST at the auto-commit query route of one
/// database — [`post_with_type`] with the versioned content type, the
/// typed twin of [`post`].
pub(super) fn post_typed(database: &str, body: &str) -> Request {
    post_with_type(database, Some("application/vnd.neo4j.query.v1.2"), body)
}

/// The transaction id a BEGIN (or tx-run) wire envelope carries — the
/// `serde_json::Value` sibling of [`tx_id`], beside [`parse_expires`], so
/// the wire tests do not re-spell the JSON pointer path.
pub(crate) fn tx_id_from(envelope: &serde_json::Value) -> String {
    envelope["transaction"]["id"]
        .as_str()
        .expect("a transaction reply carries the id")
        .to_string()
}

/// The transaction id a BEGIN (or tx-run) reply carries — the one extraction
/// spelling; the in-process twin of [`tx_id_from`].
pub(super) fn tx_id(reply: &Reply) -> String {
    tx_id_from(&body_json(reply))
}

/// The `transaction.expires` deadline an envelope carries, decoded from its
/// RFC 3339 rendering — the one spelling the in-process tx assertions and
/// the wire test parse with.
pub(crate) fn parse_expires(envelope: &serde_json::Value) -> chrono::DateTime<chrono::Utc> {
    let raw = envelope["transaction"]["expires"]
        .as_str()
        .expect("a live transaction carries expires");
    chrono::DateTime::parse_from_rfc3339(raw)
        .expect("expires is RFC 3339")
        .with_timezone(&chrono::Utc)
}

/// The deliberately slow statement the lease and wire tests use as their
/// clock and their in-flight work: a million-row unwind runs well over a
/// hundred milliseconds even in the optimized test profile — long enough to
/// overlap another transaction's whole flow and to re-stamp activity above
/// the reply's millisecond rendering, short enough to join, and never a
/// sleep on a real idle deadline.
pub(crate) const SLOW_STATEMENT: &str =
    r#"{"statement":"UNWIND range(1, 1000000) AS x RETURN count(x)"}"#;

/// POST a BEGIN at `path` with `body` and return the 201 reply. Bodies and
/// paths vary across the flow test, so the caller keeps the reply for its
/// own assertions; [`begin_tx`] is the plain `{}` neo4j form.
pub(super) fn begin_tx_reply(db: &Database, txs: &TxRegistry, path: &str, body: &str) -> Reply {
    let begin = dispatch(db, &tx_post(path, body), OPEN, txs);
    assert_eq!(begin.status, 201, "{}", body_json(&begin));
    begin
}

/// Begin a `{}` transaction on the neo4j namespace and return its id.
pub(super) fn begin_tx(db: &Database, txs: &TxRegistry) -> String {
    tx_id(&begin_tx_reply(db, txs, "/db/neo4j/query/v2/tx", "{}"))
}

pub(super) const OPEN: &AuthConfig = &AuthConfig::None;
const CLOSED: &AuthConfig = &AuthConfig::Basic {
    user: String::new(),
    password: String::new(),
};

/// A plain-dialect statement POST at the auto-commit query route of one
/// database — [`post_with_type`] with the plain content type, the route
/// spelled only there.
pub(super) fn post(database: &str, body: &str) -> Request {
    post_with_type(database, Some(crate::http::APPLICATION_JSON), body)
}

/// A registry with the two ADR-0435 knobs set explicitly — the one
/// construction spelling every managed-transaction test shares. The
/// arguments are the wire spelling (`0` = off); `TxLimits::new` converts
/// them into the typed model.
pub(super) fn registry(idle_timeout: std::time::Duration, max_open: usize) -> TxRegistry {
    TxRegistry::with_limits(
        crate::tx_limits::TxLimits::new(idle_timeout, max_open)
            .expect("the fixture limits are renderable"),
    )
}

/// The reply of a tx route call that must succeed / must fail (`Reply`
/// carries no `Debug`, so `Result::unwrap` is not available).
pub(super) fn reply_ok(outcome: Result<Reply, Reply>) -> Reply {
    match outcome {
        Ok(reply) => reply,
        Err(_) => panic!("expected a successful tx reply"),
    }
}

pub(super) fn reply_err(outcome: Result<Reply, Reply>) -> Reply {
    match outcome {
        Ok(_) => panic!("expected an error reply"),
        Err(reply) => reply,
    }
}

/// Assert the typed ADR-0435 §3 first-observer expiry for `id`: 404/42N90,
/// the message names the id and the 60 s deadline, and no `transaction`
/// block advertises the dead transaction. Shared by the run, commit, and
/// rollback routes, whose expiry answers differ only in the call under test.
pub(super) fn assert_expired_first_observer(id: &str, reply: &Reply) {
    assert_eq!(reply.status, 404);
    let body = body_json(reply);
    assert_eq!(body["errors"][0]["code"], json!("42N90"));
    let message = body["errors"][0]["message"].as_str().unwrap();
    assert!(message.contains(id), "{message}");
    assert!(message.contains("expired after 60s idle"), "{message}");
    assert!(body.get("transaction").is_none(), "{body}");
}

#[test]
fn success_envelope_has_fields_values_and_empty_bookmarks() {
    let db = db();
    let reply = dispatch(
        &db,
        &post("neo4j", r#"{"statement": "RETURN 1 AS n, 'x' AS s"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    assert_eq!(body["data"]["fields"], json!(["n", "s"]));
    assert_eq!(body["data"]["values"], json!([[1, "x"]]));
    assert_eq!(body["bookmarks"], json!([]));
    // No counters without includeCounters on a pure read.
    assert!(body.get("counters").is_none());
}

#[test]
fn parameters_bind_into_the_statement() {
    let db = db();
    let reply = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "RETURN $n + 1 AS n", "parameters": {"n": 41}}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 200);
    assert_eq!(body_json(&reply)["data"]["values"], json!([[42]]));
}

#[test]
fn write_statement_carries_counters_without_the_flag() {
    let db = db();
    let reply = dispatch(
        &db,
        &post("neo4j", r#"{"statement": "CREATE (:City {name: 'Oslo'})"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    assert_eq!(body["counters"]["nodes_created"], 1);
    assert_eq!(body["counters"]["constraints_removed"], 0);
    assert_eq!(body["data"]["values"], json!([]));
}

#[test]
fn include_counters_forces_counters_on_a_read() {
    let db = db();
    let reply = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "RETURN 1 AS n", "includeCounters": true}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(body_json(&reply)["counters"]["nodes_created"], 0);
}

/// The below-threshold envelope's wire order, and its identity with the
/// streamed body (ADR-0386 §3, audit 4a): `data` first, then the plan /
/// counters / notifications / bookmarks tail — and the bytes the one-shot
/// `Content-Length` path sends are exactly the bytes the chunked writer's
/// frames carry for the same reply.
#[test]
fn a_below_threshold_envelope_writes_data_first_and_streams_the_same_bytes() {
    let db = db();
    seed_person_and_city(&db);
    let reply = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "EXPLAIN MATCH (a:Person), (b:City) RETURN a, b",
                    "includeCounters": true}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 200, "{}", body_json(&reply));
    let text = serde_json::to_string(&reply.body).expect("a reply body serializes");
    // The wire order is a contract, not a JSON map's iteration order: `data`
    // first, then the tail in the order the constructor inserts it.
    let position = |member: &str| {
        text.find(member)
            .unwrap_or_else(|| panic!("the envelope carries {member}: {text}"))
    };
    for tail in [
        r#""queryPlan":"#,
        r#""counters":"#,
        r#""notifications":"#,
        r#""bookmarks":"#,
    ] {
        assert!(
            position(r#""data":"#) < position(tail),
            "data precedes the {tail} member: {text}"
        );
    }
    assert!(
        position(r#""queryPlan":"#) < position(r#""counters":"#)
            && position(r#""counters":"#) < position(r#""notifications":"#)
            && position(r#""notifications":"#) < position(r#""bookmarks":"#),
        "the tail keeps its insertion order: {text}"
    );
    // LABS-1120: the timing block follows the Neo4j members — bookmarks,
    // then the keyed `timings` object, then the `planCache` outcome
    // closing the envelope.
    assert!(
        text.ends_with(r#""planCache":"miss"}"#),
        "the plan-cache outcome closes the envelope: {text}"
    );
    assert!(
        position(r#""bookmarks":"#) < position(r#""timings":"#)
            && position(r#""timings":"#) < position(r#""planCache":"#),
        "the timing block trails the Neo4j members in wire order: {text}"
    );
    // Below the threshold the reply is one-shot, from exactly these bytes…
    let one_shot = match crate::stream::count_json(&reply.body).expect("the envelope serializes") {
        crate::stream::CountedJson::OneShot(bytes) => bytes,
        crate::stream::CountedJson::Stream => {
            panic!("the fixture envelope stays below the stream threshold")
        }
    };
    assert_eq!(one_shot, text.as_bytes());
    assert!(one_shot.len() < crate::stream::STREAM_THRESHOLD_BYTES);
    // … and the chunked writer, fed the same reply, reassembles to them.
    let mut wire = Vec::new();
    crate::stream::write_chunked(
        &mut wire,
        &reply.body,
        crate::content_encoding::BodyEncoding::Identity,
    )
    .expect("the identity chunked writer");
    let (streamed, frames) = relate_test_support::loopback::dechunk(&wire);
    assert!(frames > 0, "the body arrives in at least one frame");
    assert_eq!(
        streamed, one_shot,
        "the streamed body is the one-shot bytes"
    );
}

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
    let (key, node) = crate::query_plan_field(explained.plan_root().unwrap());
    assert_eq!(key, "queryPlan");
    assert_eq!(node["operatorType"], json!("ProduceResults@relate"));
    assert!(node["arguments"].is_object());
    assert!(node["arguments"].get("$type").is_none());

    let profiled = conn.query("PROFILE RETURN 1 AS n").unwrap();
    let (key, node) = crate::query_plan_field(profiled.plan_root().unwrap());
    assert_eq!(key, "profiledQueryPlan");
    assert!(node["records"].is_number());
    assert!(node["arguments"].get("$type").is_none());
}

#[test]
fn cypher_error_is_the_error_envelope() {
    let db = db();
    let reply = dispatch(
        &db,
        &post("neo4j", r#"{"statement": "RETURN"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 400);
    let body = body_json(&reply);
    assert_eq!(body["errors"][0]["code"], "42N06");
    assert!(body["errors"][0]["message"].is_string());
}

/// LABS-610: the traversal-depth admission limit surfaces as the same
/// syntax-error envelope (HTTP 400 / `42N06`), before the walker runs.
#[test]
fn traversal_depth_exceeded_is_a_syntax_error_envelope() {
    let db = db();
    let reply = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "MATCH (a)-[*1..1000000]->(b) RETURN a"}"#,
        ),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 400);
    let body = body_json(&reply);
    assert_eq!(body["errors"][0]["code"], "42N06");
    let message = body["errors"][0]["message"].as_str().unwrap();
    assert!(message.contains("TraversalDepthExceeded"), "{message}");
    assert!(message.contains("--max-traversal-depth"), "{message}");
}

/// LABS-680 (ADR-0477): a safe-mode refusal is the same syntax-error
/// envelope (HTTP 400 / `42N06`), with no new surface mapping.
#[test]
fn safe_mode_violation_is_a_syntax_error_envelope() {
    let db = Database::open_with(
        ":memory:",
        relate::OpenOptions {
            safe_mode: true,
            ..Default::default()
        },
    )
    .unwrap();
    let reply = dispatch(
        &db,
        &post("neo4j", r#"{"statement": "MATCH (n) RETURN n"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 400);
    let body = body_json(&reply);
    assert_eq!(body["errors"][0]["code"], "42N06");
    let message = body["errors"][0]["message"].as_str().unwrap();
    assert!(message.contains("SafeModeViolation"), "{message}");
    assert!(
        message.contains("This is a policy, not a database limit"),
        "{message}"
    );
}

/// A whole-object reply is the same insertion-ordered body list as a success
/// envelope, so its members serialize in wire order: the error envelope's
/// `errors` member, then the error object's `code` before `message`.
#[test]
fn an_error_envelope_serializes_its_members_in_wire_order() {
    let db = db();
    let mut get = post("neo4j", "");
    get.method = "GET".into();
    let reply = dispatch(&db, &get, OPEN, &txs());
    assert_eq!(reply.status, 405, "{}", body_json(&reply));
    assert_eq!(
        serde_json::to_string(&reply.body).expect("the error envelope serializes"),
        r#"{"errors":[{"code":"42000","message":"method not allowed"}]}"#
    );
}

#[test]
fn unknown_database_is_42n00() {
    let db = db();
    let reply = dispatch(
        &db,
        &post("movies", r#"{"statement": "RETURN 1"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 404);
    assert_eq!(body_json(&reply)["errors"][0]["code"], "42N00");
}

#[test]
fn system_namespace_routes_and_db_ping_answers() {
    // ADR-0116's fixed `system` namespace routes like any other, and
    // the Browser connectivity probe (db.ping) answers.
    let db = db();
    let reply = dispatch(
        &db,
        &post("system", r#"{"statement": "CALL db.ping()"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    assert_eq!(body["data"]["fields"], json!(["success"]));
    assert_eq!(body["data"]["values"], json!([[true]]));
}

#[test]
fn discovery_document_is_public_and_minimal() {
    let db = db();
    let mut get = post("neo4j", "");
    get.method = "GET".into();
    get.path = "/".into();
    get.headers = vec![("host".into(), "127.0.0.1:7474".into())];
    // Public: even with auth on and no credential, GET / answers 200.
    let reply = dispatch(&db, &get, CLOSED, &txs());
    assert_eq!(reply.status, 200);
    let body = body_json(&reply);
    assert_eq!(
        body["query"],
        json!("http://127.0.0.1:7474/db/{databaseName}/query/v2")
    );
    assert_eq!(body["neo4j_edition"], json!("community"));
    assert!(body["neo4j_version"].is_string());
    // No SSO: Browser skips provider discovery.
    assert_eq!(body["auth_config"]["oidc_providers"], json!([]));
    // The tx routes are advertised (they exist, ADR-0237).
    assert_eq!(
        body["transaction"],
        json!("http://127.0.0.1:7474/db/{databaseName}/query/v2/tx")
    );
    // No bolt endpoint configured: no bolt_direct field.
    assert!(body.get("bolt_direct").is_none());

    // A Host containing the route substring must not garble the advertised
    // URLs: both are formatted from one base, not found by substitution.
    let mut odd = post("neo4j", "");
    odd.method = "GET".into();
    odd.path = "/".into();
    odd.headers = vec![("host".into(), "foo/query/v2.bar".into())];
    let reply = dispatch(&db, &odd, CLOSED, &txs());
    let body = body_json(&reply);
    assert_eq!(
        body["query"],
        json!("http://foo/query/v2.bar/db/{databaseName}/query/v2")
    );
    assert_eq!(
        body["transaction"],
        json!("http://foo/query/v2.bar/db/{databaseName}/query/v2/tx")
    );
    // The document is a whole-object reply body: its members serialize in the
    // constructor's insertion order, not in a JSON map's key order.
    let text = serde_json::to_string(&reply.body).expect("the discovery document serializes");
    let position = |member: &str| {
        text.find(member)
            .unwrap_or_else(|| panic!("the document carries {member}: {text}"))
    };
    for (earlier, later) in [
        (r#""query":"#, r#""transaction":"#),
        (r#""transaction":"#, r#""neo4j_version":"#),
        (r#""neo4j_version":"#, r#""neo4j_edition":"#),
        (r#""neo4j_edition":"#, r#""auth_config":"#),
    ] {
        assert!(
            position(earlier) < position(later),
            "{earlier} precedes {later}: {text}"
        );
    }
    // Without a Host header the query URL is root-relative.
    get.headers = Vec::new();
    let reply = dispatch(&db, &get, CLOSED, &txs());
    let body = body_json(&reply);
    assert_eq!(body["query"], json!("/db/{databaseName}/query/v2"));
    // A configured bolt endpoint is appended after the fixed members.
    let reply = handle(
        &db,
        &get,
        CLOSED,
        Some("bolt://127.0.0.1:7687"),
        &txs(),
        CredentialGate::Unchecked {
            public: is_discovery_probe(&get),
        },
        None,
    );
    let body = body_json(&reply);
    assert_eq!(body["bolt_direct"], json!("bolt://127.0.0.1:7687"));
    let text = serde_json::to_string(&reply.body).expect("the discovery document serializes");
    assert!(
        text.ends_with(r#""bolt_direct":"bolt://127.0.0.1:7687"}"#),
        "{text}"
    );
}

#[test]
fn unauthenticated_is_401_with_challenge() {
    let db = db();
    let reply = dispatch(
        &db,
        &post("neo4j", r#"{"statement": "RETURN 1"}"#),
        CLOSED,
        &txs(),
    );
    assert_eq!(reply.status, 401);
    assert_eq!(body_json(&reply)["errors"][0]["code"], "42NFF");
    assert!(reply
        .extra_headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("www-authenticate")));
    // Auth runs before routing: even an unknown path is a 401, never a
    // 404 (no route-shape leak pre-auth).
    let mut off_route = post("neo4j", r#"{"statement": "RETURN 1"}"#);
    off_route.path = "/nope".into();
    assert_eq!(dispatch(&db, &off_route, CLOSED, &txs()).status, 401);
    assert_eq!(dispatch(&db, &off_route, OPEN, &txs()).status, 404);
}

#[test]
fn wrong_method_is_405_and_unknown_path_is_404() {
    let db = db();
    let mut get = post("neo4j", r#"{"statement": "RETURN 1"}"#);
    get.method = "GET".into();
    assert_eq!(dispatch(&db, &get, OPEN, &txs()).status, 405);
    let mut tx = post("neo4j", "{}");
    tx.path = "/db/neo4j/query/v2/tx".into();
    // The tx routes exist (ADR-0237): GET there is a 405, and an unknown
    // tx id a 404.
    tx.method = "GET".into();
    assert_eq!(dispatch(&db, &tx, OPEN, &txs()).status, 405);
    let mut unknown = post("neo4j", "{}");
    unknown.path = "/db/neo4j/query/v2/tx/tx-9999".into();
    assert_eq!(dispatch(&db, &unknown, OPEN, &txs()).status, 404);
}
