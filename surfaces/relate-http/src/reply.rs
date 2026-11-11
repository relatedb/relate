//! The shared reply machinery every Query API route family consumes: the
//! `Reply` type and its constructors, the error envelopes and their
//! GQLSTATUS mapping, the negotiated response media type, and the one
//! statement executor (`run_on`) with its success envelope. Lives below the
//! routers: `query_api` (the auto-commit route) and `tx` (the
//! managed-transaction routes) both import it; it imports neither.

use serde_json::{json, Map, Value as Json};

use relate::{
    client_message, error_info, log_internal_original, query_stats_to_wire, Error, ErrorStage,
    QueryResult,
};
use relate_error::{GQLSTATUS_GENERAL, GQLSTATUS_SYNTAX};
use relate_model::wire::{self, WireStatsSnapshot};

use crate::http::{is_versioned_query_family, APPLICATION_JSON, VND_NEO4J_QUERY};
use crate::typed_params::{request_params, RequestDialect};
use crate::v2_envelope::ReplyBody;
use crate::v2_json::v2_value_json;
use crate::v2_plan;

/// The outcome of dispatching one request: an HTTP status and the JSON body.
pub struct Reply {
    /// The HTTP status.
    pub status: u16,
    /// The reply body: its top-level members in wire order (ADR-0386 §3) —
    /// the success envelope's `data` first and then its tail, or one
    /// whole-object reply's members as its constructor inserted them.
    pub body: ReplyBody,
    /// The response content type: `application/json` everywhere except a
    /// successful query answer negotiated to the Neo4j Query API v2 media
    /// type from the request's Accept header.
    pub content_type: &'static str,
    /// Extra response headers (the `WWW-Authenticate` challenge on a 401).
    pub extra_headers: Vec<(String, String)>,
}

impl Reply {
    /// A reply whose body is one whole JSON object: an error envelope, the
    /// discovery document, a bookmarks-only transaction answer.
    pub(crate) fn json(status: u16, body: ReplyBody) -> Self {
        Self::json_as(status, body, ResponseMediaType::Json)
    }

    /// A whole JSON object as a reply. `serde_json::Map` iterates in sorted
    /// key order: the workspace builds serde_json without `preserve_order`.
    /// Used by an embedding host's extension routes (ADR-0428).
    pub fn from_json(status: u16, value: Json) -> Self {
        let body = match value {
            Json::Object(map) => ReplyBody::new(map),
            other => ReplyBody::new([("value", other)]),
        };
        Self::json(status, body)
    }

    /// A reply in the negotiated media type (a successful query answer);
    /// [`Self::json`] is the plain-JSON spelling. The constructor, not a
    /// field-by-field build, is how every reply is made.
    pub(crate) fn json_as(status: u16, body: ReplyBody, media_type: ResponseMediaType) -> Self {
        Self {
            status,
            body,
            content_type: media_type.content_type(),
            extra_headers: Vec::new(),
        }
    }
}

/// The GQLSTATUS code for an unauthenticated request (Neo4j's unauthorized
/// code).
pub(crate) const UNAUTHORIZED_CODE: &str = "42NFF";

/// The 400 envelope for a body that does not parse: one mapping both
/// routers (the auto-commit route and the tx routes) consult.
pub(crate) fn invalid_request_body(e: serde_json::Error) -> Reply {
    error_envelope(400, GQLSTATUS_SYNTAX, &format!("invalid request body: {e}"))
}

/// The error-envelope reply for a transport-level rejection (the browser
/// guard's 403/415, the pre-body 401, a malformed request): 401s carry the
/// unauthorized code, everything else the GQL general code.
/// [`crate::serve_handler`] maps the shared gate outcome onto it.
pub(crate) fn error_reply(status: u16, message: &str) -> Reply {
    let code = if status == 401 {
        UNAUTHORIZED_CODE
    } else {
        GQLSTATUS_GENERAL
    };
    error_envelope(status, code, message)
}

/// The error envelope `{"errors":[{"code","message"}]}` (ADR-0232 §3),
/// mirroring the Bolt trust-boundary sanitization of ADR-0260: internal-class
/// errors are genericized, everything else passes its message through.
/// `metrics` is the optional receipt member on the error object.
pub(crate) fn error_envelope(status: u16, code: &str, message: &str) -> Reply {
    error_envelope_with(status, code, message, None)
}

fn error_envelope_with(
    status: u16,
    code: &str,
    message: &str,
    metrics: Option<serde_json::Value>,
) -> Reply {
    let mut error = json!({"code": code, "message": message});
    if let Some(metrics) = metrics {
        error["metrics"] = metrics;
    }
    Reply::json(status, ReplyBody::new([("errors", json!([error]))]))
}

/// The request-side 400 envelope (the general code): the one constructor
/// every request-decode rejection rides — the dialect resolver, the
/// parameter fold and the tx READ guard alike.
pub(crate) fn bad_request(message: &str) -> Reply {
    error_envelope(400, GQLSTATUS_GENERAL, message)
}

/// The client message for an engine error at this boundary, with the
/// internal class's original logged server-side first (Bolt and the C ABI
/// boundary log theirs too; ADR-0260, OPLANE_REQ-00340359) — the one
/// helper `run_error` and `namespace_connect_failure` consult. The Cow
/// keeps the internal class's fixed message borrowed.
fn logged_client_message(e: &Error) -> std::borrow::Cow<'_, str> {
    log_internal_original(e, "http");
    client_message(e)
}

/// The namespace-connect failure mapping, one helper: a `connect_namespace`
/// error is the engine's code and the message its classification allows
/// across (ADR-0260 — the internal class is genericized, so a storage
/// path never rides the envelope) under a 404 — consulted by the
/// auto-commit arm and the tx begin route alike.
pub(crate) fn namespace_connect_failure(e: &Error) -> Reply {
    // The route's 404 answers the caller-facing classes (an unknown
    // namespace is the client's own doing); an internal-class failure
    // during connect is the server's own, and files exactly as
    // `run_error` files it (500, genericized) — one engine-error policy.
    match error_info(e).stage {
        ErrorStage::Internal => run_error(e),
        ErrorStage::Compile | ErrorStage::Runtime => {
            let info = error_info(e);
            error_envelope(404, info.gql_status, &client_message(e))
        }
    }
}

/// Run one statement on `conn` and build its Reply in the negotiated
/// media type. The one statement executor for the auto-commit route and
/// the tx routes; the result carries the outcome as the type — `Ok` the
/// success envelope, `Err` the error envelope — so no consumer re-detects
/// it from the wire status. `success_status` is the route's own success
/// code (200 auto-commit, 201 for the tx begin), so every reply is born
/// with its final status and no caller edits the field afterwards.
pub(crate) fn run_on(
    conn: &mut relate::Connection,
    statement: &str,
    parameters: Option<Map<String, Json>>,
    include_counters: bool,
    media_type: ResponseMediaType,
    dialect: RequestDialect,
    success_status: u16,
) -> Result<Reply, Reply> {
    // The request's dialect (ADR-0425, resolved at the route) picks the
    // fold through the one dispatcher beside the enum; the 400 mapping is
    // this route's.
    // A class-less request-side decode rejection (not ParameterMissing,
    // which the engine reports for an unbound $name): the route's
    // GQLSTATUS_GENERAL per the stated one-constant policy.
    let params =
        request_params(dialect, parameters.as_ref()).map_err(|message| bad_request(&message))?;
    match conn.query_with_params(statement, params) {
        // The one success-reply constructor: its `data` is `fields` then
        // `values`, the order both reply writers emit (ADR-0386 §3), and
        // the one negotiated media type drives the value encoding and the
        // content type the reply advertises.
        Ok(result) => Ok(success_envelope(
            &result,
            include_counters,
            media_type,
            success_status,
        )),
        Err(e) => Err(run_error(&e)),
    }
}

/// An engine error as an error-envelope Reply. The classification is
/// consulted once: the code and the message are the engine's
/// (ADR-0502 §2 — `gql_status`, and [`logged_client_message`], which
/// genericizes the internal class per ADR-0260 / OPLANE_REQ-00340359 and
/// logs its original server-side); the HTTP status is this transport's
/// own presentation — variant overrides for the capacity and
/// backpressure families, the stage for everything else, with the
/// internal class (engine-internal, IO, storage, locked) as the server's
/// own failure (500).
pub(crate) fn run_error(e: &Error) -> Reply {
    let info = error_info(e);
    let status = match e {
        Error::Capacity { .. } => 507,
        Error::ResourceLimit {
            name: Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT,
            ..
        }
        | Error::ResourceLimit {
            retryable: true, ..
        } => 429,
        _ => match info.stage {
            ErrorStage::Internal => 500,
            ErrorStage::Compile | ErrorStage::Runtime => 400,
        },
    };
    let message = logged_client_message(e);
    let metrics = e.receipt().map(|receipt| {
        let mut metrics = json!({
            "peakLogicalBytes": receipt.peak_logical_bytes,
        });
        // `retryable` is not otherwise visible on the wire: the status
        // mapping folds `concurrent-logical-workspace-limit` and every
        // retryable `ResourceLimit` into the same 429, so a host that must
        // count retryable and non-retryable logical-workspace refusals
        // separately (ADR-0440) cannot derive the bit from status alone.
        // The classification's own bit is the single source. Additive
        // field; an older reader ignores it.
        metrics["retryable"] = json!(info.retryable);
        metrics
    });
    error_envelope_with(status, info.gql_status, &message, metrics)
}

/// One parsed `Accept` entry: which servable family it names, if any.
/// Parameters (`;q=…`) are stripped and discarded — weights are read for
/// nothing here (see [`negotiated_media_type`]).
enum AcceptEntry {
    /// The Query API v2 media type or a versioned `…vN.N+json`/`+jsonl`
    /// variant (the same typed envelope).
    Neo4jQuery,
    /// `application/json` or a wildcard.
    Json,
    /// Anything else — parsed but never served.
    Other,
}

impl AcceptEntry {
    fn parse(entry: &str) -> Self {
        let media_type = crate::http::lowercased_media_type(entry);
        // The one family prefix, derived from the constant: the media type
        // itself, or a versioned variant of it (`.vN.N+json` / `+jsonl`).
        // A versioned variant is answered with the Query API v2 media
        // type — the same typed envelope; Browser's client REQUIRES that
        // content type (it rejects `application/json` with "Wrong
        // content-type") and then decodes with its typed (`{"$type":…}`)
        // codec, so the body carries the typed encoding under this type
        // (LABS-376).
        if media_type == VND_NEO4J_QUERY || is_versioned_query_family(&media_type) {
            return Self::Neo4jQuery;
        }
        match media_type.as_str() {
            APPLICATION_JSON | "*/*" | "application/*" => Self::Json,
            _ => Self::Other,
        }
    }
}

/// The response media type negotiated from an `Accept` header. The rule is
/// order-insensitive: any `application/vnd.neo4j.query` entry (or a
/// versioned `…vN.N+json`/`+jsonl` variant) selects the typed media type;
/// otherwise any `application/json` or wildcard entry selects plain JSON;
/// otherwise the request gets a 406. Entry order and `;q=…` weights are
/// read for nothing — unlike the sibling `Accept-Encoding` parse, which
/// honors `q=0` for gzip — a deliberate simplification: the two servable
/// media types are equivalent renderings of the same envelope, so a weight
/// or order could only force the other rendering of identical content. No
/// `Accept` header (or an empty one) means the client takes anything:
/// `application/json`, so curl-style clients keep working. Neo4j Browser's
/// actual list (`vnd.neo4j.query.v1.0+jsonl, vnd.neo4j.query,
/// application/json`) negotiates to [`ResponseMediaType::Neo4jQuery`].
pub(crate) fn negotiated_media_type(accept: Option<&str>) -> Option<ResponseMediaType> {
    let accept = accept.unwrap_or("");
    if accept.trim().is_empty() {
        return Some(ResponseMediaType::Json);
    }
    // Two order-insensitive folds: any vnd-family entry selects the typed
    // media type; otherwise any json/wildcard entry selects plain JSON;
    // otherwise 406.
    let mut saw_json = false;
    for entry in accept.split(',') {
        match AcceptEntry::parse(entry) {
            AcceptEntry::Neo4jQuery => return Some(ResponseMediaType::Neo4jQuery),
            AcceptEntry::Json => saw_json = true,
            AcceptEntry::Other => {}
        }
    }
    saw_json.then_some(ResponseMediaType::Json)
}

/// The 406: the request named nothing servable — the message names the set
/// [`ResponseMediaType`] owns.
pub(crate) fn not_acceptable() -> Reply {
    error_envelope(
        406,
        GQLSTATUS_GENERAL,
        &format!("not acceptable: this server responds {APPLICATION_JSON} or {VND_NEO4J_QUERY}"),
    )
}

/// The response media type negotiated from the request's `Accept` header:
/// the plain `application/json` rendering, or the Neo4j Query API v2 media
/// type's typed (`{"$type","_value"}`) encoding. The one place the wire
/// content type and the value encoding are decided together, so a reply can
/// never advertise one encoding and serialize the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseMediaType {
    /// `application/json`: plain JSON values.
    Json,
    /// `application/vnd.neo4j.query`: the typed value encoding (LABS-376).
    Neo4jQuery,
}

impl ResponseMediaType {
    /// The `content-type` header value this media type advertises.
    fn content_type(self) -> &'static str {
        match self {
            Self::Json => APPLICATION_JSON,
            Self::Neo4jQuery => VND_NEO4J_QUERY,
        }
    }

    /// Whether values take the typed (`{"$type","_value"}`) encoding.
    fn typed(self) -> bool {
        matches!(self, Self::Neo4jQuery)
    }
}

/// The success envelope: the one constructor of a success reply
/// (ADR-0386 §3). `data` comes first — `data.fields` and the materialized
/// `data.values` rows — then the tail in wire order: the structured plan of
/// an EXPLAIN/PROFILE run (`queryPlan`/`profiledQueryPlan`, ADR-0344/0345,
/// ADR-0357), `counters`, `notifications` (ADR-0363 decision 12) and
/// `bookmarks`, with the tx routes' `transaction` block appended last by
/// [`ReplyBody::insert_member`].
///
/// The row array is moved in, never rebuilt, so no writer re-reads the JSON
/// tree. The negotiated media type decides the value encoding here, beside the
/// content type the reply advertises.
fn success_envelope(
    result: &QueryResult,
    include_counters: bool,
    media_type: ResponseMediaType,
    success_status: u16,
) -> Reply {
    let typed = media_type.typed();
    let fields = Json::Array(
        result
            .header
            .columns
            .iter()
            .map(|c| Json::String(c.clone()))
            .collect(),
    );
    // Rows as arrays aligned with `fields`, values in the Neo4j Query API v2
    // encoding (see [`v2_value_json`]). The envelope owns the materialized
    // rows (ADR-0386 §6: the engine has no cursor); the writer walks them one
    // at a time and never rebuilds the array.
    let rows = result
        .rows
        .iter()
        .map(|row| {
            Json::Array(
                row.iter()
                    .map(|v| v2_value_json(&relate::value_to_cypher(v), typed))
                    .collect(),
            )
        })
        .collect();
    // The `data` member opens every success envelope with `fields` then the
    // materialized `values` rows. That order is an invariant, not a
    // coincidence: serde_json's `Map` is a `BTreeMap` here (the
    // `preserve_order` feature is off workspace-wide — the workspace
    // Cargo.toml enables no features on serde_json), so object members
    // serialize in key order and "fields" sorts before "values" on every
    // build; the stream tests pin the exact `{"data":{"fields"` prefix at
    // both thresholds, so a third key ever joining `data` is caught. The
    // top-level order below is the body's member list, not a map's.
    let mut data = Map::new();
    data.insert("fields".to_string(), fields);
    data.insert("values".to_string(), Json::Array(rows));
    let mut members = vec![("data".to_string(), Json::Object(data))];
    // The structured plan of an EXPLAIN/PROFILE statement (ADR-0344/0345,
    // ADR-0357) rides the envelope in the Neo4j Query API v2 field shape —
    // `queryPlan` for EXPLAIN, `profiledQueryPlan` for PROFILE — through
    // the engine's one decoration (`PlanNode::to_neo4j_value`, root-shaped
    // and adding the Neo4j-facing root args) and the same `v2_value_json`
    // encoding the row values use. Absent on plain runs. The whole v2
    // translation lives in [`crate::v2_plan`], beside the value encoding.
    if let Some(tree) = result.plan_root() {
        let (key, node) = v2_plan::plan_field(tree, typed);
        members.push((key.to_string(), node));
    }
    let stats: WireStatsSnapshot = query_stats_to_wire(&result.stats);
    if include_counters || stats_updated_graph(&stats) {
        members.push(("counters".to_string(), wire::stats_to_wire_json(&stats)));
    }
    // ADR-0363 decision 12: the EXPLAIN/PROFILE notifications in the
    // Neo4j shape, through the engine serializer — the remedy folds into
    // `description`, and no `remedy`, `gql_status`, `operator_id` or
    // `position` key is emitted (there is no position value, so the slot
    // is omitted, never null). The whole key is absent on an empty list.
    if !result.notifications().is_empty() {
        members.push((
            wire::notification_keys::NOTIFICATIONS.to_string(),
            Json::Array(
                result
                    .notifications()
                    .iter()
                    .map(wire::notification_to_neo4j_json)
                    .collect(),
            ),
        ));
    }
    members.push(("bookmarks".to_string(), Json::Array(Vec::new())));
    // LABS-1120: the statement's timing block rides the reply after the
    // Neo4j Query API v2 members — one keyed `timings` object (the model's
    // one builder, the same nested form the CLI machine modes carry) with
    // the `planCache` outcome beside it. Namespaced Relate extension keys,
    // absent in the reference server's shape: clients that ignore unknown
    // members are unaffected (the official drivers' hydrators drop unknown
    // keys), and the alternative — no phase visibility on this surface —
    // is what LABS-1120 set out to fix.
    let timings = relate::timing_stamp_ms(result);
    members.push(("timings".to_string(), timings.to_timings_json()));
    members.push((
        "planCache".to_string(),
        Json::String(timings.plan_cache.into()),
    ));
    Reply::json_as(success_status, ReplyBody::new(members), media_type)
}

/// Whether any mutation counter is nonzero (the statement updated the graph).
fn stats_updated_graph(stats: &WireStatsSnapshot) -> bool {
    wire::STAT_COUNTERS
        .iter()
        .any(|counter| (counter.count)(stats) != 0)
}

#[cfg(test)]
mod tests {
    use relate::{ExecutionReceipt, TokenOutcome, INTERNAL_MESSAGE};

    use super::*;
    use crate::v2_envelope::tests::body_json;

    fn peak_token_refusal(retryable: bool) -> Error {
        Error::resource_limit_with_receipt(
            Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT,
            16 * 4096,
            retryable,
            ExecutionReceipt {
                peak_logical_bytes: 20 * 4096,
                outcome: TokenOutcome::CapacityExceeded,
            },
        )
    }

    /// ADR-0440: `run_error`'s status/code mapping alone cannot distinguish
    /// a retryable logical-workspace refusal from a non-retryable one — both are
    /// 429 with the same message — so a client implementing embedded Relate
    /// that counts them separately reads `metrics.retryable`.
    #[test]
    fn a_retryable_peak_token_refusal_is_429_with_retryable_true_in_metrics() {
        let reply = run_error(&peak_token_refusal(true));
        assert_eq!(reply.status, 429);
        let metrics = &body_json(&reply)["errors"][0]["metrics"];
        assert_eq!(metrics["retryable"], true);
        assert_eq!(metrics["peakLogicalBytes"].as_u64(), Some(20 * 4096u64));
        assert!(metrics.get("tokenSchedule").is_none());
    }

    #[test]
    fn a_non_retryable_peak_token_refusal_is_still_429_but_retryable_false_in_metrics() {
        let reply = run_error(&peak_token_refusal(false));
        assert_eq!(reply.status, 429);
        assert_eq!(
            body_json(&reply)["errors"][0]["metrics"]["retryable"],
            false
        );
    }

    /// A `ResourceLimit` with no receipt (e.g. `WRITER_DRAINING`) has no
    /// `metrics` object at all, so it never gains a stray `retryable` key.
    #[test]
    fn a_receipt_less_resource_limit_has_no_metrics_object() {
        let reply = run_error(&Error::resource_limit(Error::WRITER_DRAINING, 0, true));
        assert_eq!(reply.status, 429);
        assert!(body_json(&reply)["errors"][0].get("metrics").is_none());
    }

    #[test]
    fn read_only_writer_refusal_is_42n18() {
        let read_only = Error::semantic(
            "WriteOnReadOnlyAccessDatabase: No write operations are allowed on this database.",
        );
        let reply = run_error(&read_only);
        assert_eq!(reply.status, 400);
        assert_eq!(body_json(&reply)["errors"][0]["code"], "42N18");
    }

    /// The internal class — storage/IO/locked failures, not just
    /// `Error::Internal` — is the server's own failure (500) and its
    /// message is the genericized one (ADR-0260, OPLANE_REQ-00340359):
    /// a storage error's file path never rides the envelope.
    #[test]
    fn internal_class_failures_are_500_with_the_genericized_message() {
        for e in [
            Error::storage("checkpoint /var/lib/relate/secret.rl failed"),
            Error::Locked("/var/lib/relate/secret.rl".into()),
            Error::internal("invariant broken"),
        ] {
            let reply = run_error(&e);
            assert_eq!(reply.status, 500, "{e}");
            let body = body_json(&reply);
            assert_eq!(body["errors"][0]["message"], INTERNAL_MESSAGE, "{e}");
            assert!(
                !body.to_string().contains("secret.rl"),
                "internal detail must not ride the envelope: {body}"
            );
        }
    }

    /// A namespace-connect failure is the route's 404 for the
    /// caller-facing classes, and files exactly as `run_error` files the
    /// internal class (500, genericized) — the connect path is the same
    /// trust boundary (OPLANE_REQ-00340359).
    #[test]
    fn namespace_connect_failure_genericizes_the_internal_class() {
        let e = Error::storage("open /var/lib/relate/secret.rl failed");
        let reply = namespace_connect_failure(&e);
        assert_eq!(reply.status, 500);
        let body = body_json(&reply);
        assert_eq!(body["errors"][0]["message"], INTERNAL_MESSAGE);
        assert!(!body.to_string().contains("secret.rl"));
        // The caller-facing classes keep the route's 404 and their code.
        let unknown = Error::parameters_missing(["nope"]);
        let reply = namespace_connect_failure(&unknown);
        assert_eq!(reply.status, 404);
    }
}
