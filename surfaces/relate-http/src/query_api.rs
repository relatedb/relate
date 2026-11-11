//! The Query API v2 statement route: `POST /db/{databaseName}/query/v2`
//! (ADR-0232 §3). Request and response shapes are the charter's subset —
//! `statement` / `parameters` / `includeCounters` in,
//! `data.fields` / `data.values` / `bookmarks` / `counters` or
//! `errors[{code,message}]` out.

use serde_json::{json, Map, Value as Json};

use relate::Database;

use crate::http::{Request, RequestHead};
use crate::reply::{auth_error_code, error_envelope, invalid_request_body, run_on, Reply};
use crate::tx_registry::TxRegistry;
use crate::typed_params::RequestDialect;
use crate::v2_envelope::ReplyBody;
use relate_error::{GQLSTATUS_DATABASE_NOT_FOUND, GQLSTATUS_GENERAL};

/// The request body: `statement` (required), `parameters` (optional object of
/// `$name` bindings), `includeCounters` (optional, default false). Everything
/// else the reference API carries (`accessMode`, `impersonatedUser`,
/// `maxExecutionTime`, `txConfig`) is out of the charter subset and ignored
/// by serde.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementRequest {
    pub statement: String,
    #[serde(default)]
    pub parameters: Option<Map<String, Json>>,
    #[serde(default)]
    pub include_counters: bool,
}

/// Whether `handle` still has to call `check_authorization`.
pub(crate) enum CredentialGate {
    /// `read_admitted_request` already accepted the credential.
    Accepted,
    /// The gate skipped the check. A query route is still authenticated.
    Public,
    /// A direct caller. `public` is that caller's skip bit.
    #[cfg(test)]
    Unchecked { public: bool },
}

/// An authentication failure in the Query API envelope.
fn auth_failure(auth: &relate_auth::AuthConfig, err: relate_auth::AuthError) -> Reply {
    let failure = auth.http_failure(err);
    let mut reply = error_envelope(failure.status, auth_error_code(err), failure.message);
    if let Some(challenge) = failure.challenge {
        reply.extra_headers.push(challenge);
    }
    reply
}

/// Dispatch one parsed request against the database, authenticating first —
/// the same gate order as the MCP surface's `handle_http` (ADR-0232 §1: one
/// auth story, enforced identically on every request): a failed
/// `Authorization: Basic` check against `auth` is a 401 before any routing,
/// so an unauthenticated request learns nothing about routes. The one
/// deliberate exception is `GET /` — the discovery document is public, as
/// in Neo4j (tooling probes it before credentials exist). Then:
/// `POST /db/{databaseName}/query/v2` runs the statement; any other path is a
/// 404, any other method on the route is a 405, and the
/// managed-transaction routes (`…/tx`, `…/tx/{id}`, `…/tx/{id}/commit`)
/// follow ADR-0237. `bolt_direct` is advertised in the discovery document
/// only when this server actually has a Bolt endpoint.
pub fn handle(
    db: &Database,
    request: &Request,
    auth: &relate_auth::AuthConfig,
    bolt_direct: Option<&str>,
    txs: &TxRegistry,
    gate: CredentialGate,
    extension: Option<&dyn crate::HttpExtension>,
) -> Reply {
    // The one discovery predicate, surface-owned (the public `GET /`
    // document is a Query API fact, not a wire-layer one): the gate calls it
    // through its probe parameter and carries the classification on Pass;
    // this route reads that carried flag as data. A direct caller (tests,
    // an embedding host) computes it with the predicate.
    // Discovery is the only public Query API route. An extension path may
    // be public too and is answered below. A query route stays
    // authenticated even if a caller marked it public (ADR-0428).
    // `Accepted` means the connection gate already called
    // `check_authorization`; calling it again would verify a Custom
    // credential twice.
    if is_discovery_probe(request) {
        return discovery_document(request, bolt_direct);
    }
    let must_check = match gate {
        CredentialGate::Accepted => false,
        CredentialGate::Public => is_query_route(&request.path),
        #[cfg(test)]
        CredentialGate::Unchecked { public } => !public || is_query_route(&request.path),
    };
    if must_check {
        if let Err(err) = auth.check_authorization(request.header("authorization")) {
            return auth_failure(auth, err);
        }
    }
    let segments: Vec<&str> = request.path.split('/').filter(|s| !s.is_empty()).collect();
    let ["db", database, "query", "v2", rest @ ..] = segments.as_slice() else {
        if let Some(reply) = extension.and_then(|extension| extension.handle(request)) {
            return reply;
        }
        return not_found();
    };
    // Content negotiation: the response content type is negotiated from
    // the request's Accept header (order-insensitive: any vnd-family entry
    // wins wherever it appears) — Neo4j Browser's
    // driver hard-fails on a bare application/json answer when it asked
    // for the Query API v2 media type. The body shape never changes, and
    // the negotiated media type decides the value encoding at the one
    // success-envelope constructor. Only entries outside the vnd, json and
    // wildcard families (e.g. `application/xml`) get a 406 — the versioned
    // `…vN.N+json`/`+jsonl` spellings are served the typed envelope.
    let Some(media_type) =
        crate::reply::negotiated_media_type(request.header_list("accept").as_deref())
    else {
        return crate::reply::not_acceptable();
    };
    // The arms' guards are fallible replies, so the route returns them
    // as `Err` and this one fold turns them into the reply: every arm's
    // `?` keeps the guard order the arm states, without a match per
    // guard (the owner lookup alone would otherwise be spelled once per
    // branch).
    match route(db, request, auth, database, rest, media_type, txs) {
        Ok(reply) | Err(reply) => reply,
    }
}

/// The routed arms of the Query API surface, each guard as `?`: the
/// caller owns discovery, authentication and content negotiation, so an
/// `Err` here is always a reply the arm decided (400/404/405/406).
#[allow(clippy::too_many_arguments)]
fn route(
    db: &Database,
    request: &Request,
    auth: &relate_auth::AuthConfig,
    database: &str,
    rest: &[&str],
    media_type: crate::reply::ResponseMediaType,
    txs: &TxRegistry,
) -> Result<Reply, Reply> {
    match rest {
        // The auto-commit statement route: POST-only, its parameter
        // dialect resolved by the arm's one guard.
        [] => {
            let dialect = post_dialect(request)?;
            let body: StatementRequest =
                serde_json::from_slice(&request.body).map_err(invalid_request_body)?;
            // `{databaseName}` selects one of the ADR-0116 fixed
            // namespaces; an unknown name fails with the engine's 42N00
            // DatabaseNotFound.
            let mut conn = db
                .connect_namespace(database)
                .map_err(|e| crate::reply::namespace_connect_failure(&e))?;
            run_on(
                &mut conn,
                &body.statement,
                body.parameters,
                body.include_counters,
                media_type,
                dialect,
                200,
            )
        }
        // The managed-transaction routes (ADR-0237). Every statement-
        // carrying arm is POST-only through the same one guard.
        ["tx"] => {
            let dialect = post_dialect(request)?;
            let owner = tx_owner(auth, request)?;
            crate::tx::begin(
                db,
                database,
                &request.body,
                media_type,
                txs,
                owner.as_deref(),
                dialect,
            )
        }
        ["tx", id] => {
            // The method dispatch comes first so the arm's guard order
            // matches its siblings: a statement-carrying request resolves
            // its dialect before the owner is consulted (a 400 outranks a
            // 401), the body-less DELETE consults no dialect at all.
            match request.method.as_str() {
                "POST" => {
                    let dialect = resolve_dialect(request)?;
                    let owner = tx_owner(auth, request)?;
                    crate::tx::run(
                        id,
                        &request.body,
                        media_type,
                        txs,
                        owner.as_deref(),
                        dialect,
                    )
                }
                "DELETE" => {
                    let owner = tx_owner(auth, request)?;
                    crate::tx::rollback(id, txs, media_type, owner.as_deref())
                }
                _ => Err(method_not_allowed()),
            }
        }
        ["tx", id, "commit"] => {
            let dialect = post_dialect(request)?;
            let owner = tx_owner(auth, request)?;
            crate::tx::commit(
                id,
                &request.body,
                media_type,
                txs,
                owner.as_deref(),
                dialect,
            )
        }
        _ => Err(not_found()),
    }
}

/// Resolve the owner from the verified credential and the host-declared
/// owner header before a managed transaction is opened or accessed.
fn tx_owner(auth: &relate_auth::AuthConfig, request: &Request) -> Result<Option<String>, Reply> {
    let credential = request
        .header("authorization")
        .and_then(relate_auth::presented_credential);
    let header_value = auth.owner_header().and_then(|name| request.header(name));
    auth.transaction_owner(credential.as_deref(), header_value)
        .map_err(|err| auth_failure(auth, err))
}

/// The POST-only statement guard: the one method check a statement-
/// carrying route arm performs, with the ADR-0425 dialect resolution
/// folded into it — a non-POST is the arm's 405, a POST resolves its
/// parameter dialect through [`resolve_dialect`]. One guard, so no arm
/// carries a method check its enclosing match has already answered.
fn post_dialect(request: &Request) -> Result<RequestDialect, Reply> {
    if request.method != "POST" {
        return Err(method_not_allowed());
    }
    resolve_dialect(request)
}

/// Resolve a statement request's parameter dialect from its Content-Type
/// (ADR-0425): the versioned Query API family is typed envelopes, plain
/// `application/json` is plain values — and anything else, the
/// unversioned `application/vnd.neo4j.query` every real server rejects
/// included, fails closed with 400 (the route's GQLSTATUS_GENERAL) naming the
/// two accepted spellings.
fn resolve_dialect(request: &Request) -> Result<RequestDialect, Reply> {
    crate::typed_params::request_dialect(request.header("content-type"))
        .map_err(|message| crate::reply::bad_request(&message))
}

/// The 405 envelope: one helper every route arm shares, so a new route
/// never re-spells the guard.
fn method_not_allowed() -> Reply {
    error_envelope(405, GQLSTATUS_GENERAL, "method not allowed")
}

/// The 404 envelope for an unknown path: one helper beside
/// [`method_not_allowed`]; the engine's DatabaseNotFound code, not a
/// re-spelled literal.
fn not_found() -> Reply {
    error_envelope(404, GQLSTATUS_DATABASE_NOT_FOUND, "not found")
}

/// The discovery document at `GET /` (public, as in Neo4j): the query
/// endpoint URL (absolute via the `Host` header when present, root-relative
/// otherwise), the `neo4j_version`/`neo4j_edition` pair clients sniff for,
/// and `bolt_direct` only when this server actually has a Bolt endpoint
/// (`serve` has no `--bolt` today, so the field is absent — the wiring is
/// there for a future one to fill). Kept deliberately minimal.
fn discovery_document(request: &Request, bolt_direct: Option<&str>) -> Reply {
    // One URL prefix, both members formatted from it: no substring surgery
    // on the client-controlled `Host` value.
    let base = request
        .header("host")
        .map(|host| format!("http://{host}"))
        .unwrap_or_default();
    let mut doc = ReplyBody::new([
        (
            "query",
            Json::String(format!("{base}/db/{{databaseName}}/query/v2")),
        ),
        // The tx routes exist (ADR-0237), so the document advertises them.
        (
            "transaction",
            Json::String(format!("{base}/db/{{databaseName}}/query/v2/tx")),
        ),
        (
            "neo4j_version",
            Json::String(env!("CARGO_PKG_VERSION").to_string()),
        ),
        ("neo4j_edition", Json::String("community".to_string())),
        // No SSO here: the fixed empty OIDC list keeps Neo4j Browser from
        // probing (and failing) SSO provider discovery.
        ("auth_config", json!({"oidc_providers": []})),
    ]);
    if let Some(bolt) = bolt_direct {
        doc.insert_member("bolt_direct", Json::String(bolt.to_string()));
    }
    Reply::json(200, doc)
}

/// Whether `head` is the public `GET /` discovery probe: the one predicate,
/// surface-owned. Its callers: the pre-body auth gate (`read_admitted_request`, via the
/// surface's probe parameter) and the route above. The reply-encoding policy
/// does not call it — it consumes the gate's carried `public` flag as data.
/// The public discovery document is a Query API route fact, so it lives
/// beside the route, not in the shared wire module: ADR-0232's discovery
/// document is reachable before credentials exist (tooling probes it), and
/// ADR-0385 §2 keeps it outside the compressed seam, so it carries neither
/// gzip nor `Vary`.
pub(crate) fn is_discovery_probe(head: &RequestHead) -> bool {
    head.method == "GET" && head.path == "/"
}

/// Whether `path` is a Query API route (`/db/{database}/query/v2` and its
/// transaction suffixes). An extension's `is_public` answer does not apply
/// here (ADR-0428).
pub(crate) fn is_query_route(path: &str) -> bool {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    matches!(segments.as_slice(), ["db", _, "query", "v2", ..])
}

// The tests live in `query_api_tests` — one concern per file, reached
// through this module. The shared whole-body view lives beside the body
// type it reads (`v2_envelope::tests::body_json`).
#[cfg(test)]
#[path = "query_api_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "parameter_missing_tests.rs"]
mod parameter_missing_tests;

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod notifications_tests;

#[cfg(test)]
#[path = "negotiation_tests.rs"]
mod negotiation_tests;

// The managed-transaction flow tests, split by concern from one 858-line
// file: expiry and the honest deadline, the cap and its reservations, and
// the lease-concurrency tests beside their start_slow harness. The shared
// fixtures (registry, reply helpers, expires, the first-observer assertion)
// stay in `query_api::tests`.
#[cfg(test)]
#[path = "tx_expiry_tests.rs"]
mod tx_expiry_tests;

#[cfg(test)]
#[path = "tx_cap_tests.rs"]
mod tx_cap_tests;

#[cfg(test)]
#[path = "tx_lease_tests.rs"]
mod tx_lease_tests;
