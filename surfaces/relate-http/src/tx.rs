//! The route family of the Query API v2 `tx` endpoints (ADR-0237): the
//! request body (`TxRequest`, whose access mode is the registry's
//! [`AccessMode`]), the statement run (`run_tx_statement`), and the four
//! handlers (`begin`, `run`, `commit`, `rollback`) the Query API router
//! calls. The registry core they drive —
//! the open-transaction map, the lease, the cap reservation, the lazy
//! expiry sweep, and the managed-transaction limits — lives in
//! [`crate::tx_registry`]; this module names it as data and renders every
//! answer.
//!
//! A route leases its entry out of the registry and runs the statement
//! with the registry lock released, so a slow statement in one transaction
//! cannot stall another transaction's routes. The lease has two exits —
//! [`Lease::release`] puts the entry back (a run), [`Lease::take`] hands
//! it to its one-way close (commit, rollback).
//!
//! The reply contract is ADR-0435's: `expires` is the honest wall-clock
//! rendering of the entry's own deadline, omitted when expiry is disabled;
//! an expired transaction answers `404`/`42N90` to its first observer; and
//! an over-cap begin answers `429`/`42N91` before the engine is asked for
//! a snapshot pin.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value as Json};

use relate::Database;

use crate::reply::{
    error_envelope, invalid_request_body, run_error, run_on, Reply, ResponseMediaType,
};
use crate::tx_limits::render_deadline;
use crate::tx_registry::{AccessMode, Lease, LookupMiss, TxEntry, TxRegistry};
use crate::typed_params::RequestDialect;
use crate::v2_envelope::ReplyBody;
use relate_error::GQLSTATUS_GENERAL;

/// A tx-route body: the statement fields are the same as
/// [`StatementRequest`], but `statement` is optional (a pure BEGIN or COMMIT
/// carries only `accessMode`), and `accessMode` is honored: a `READ`
/// transaction rejects write and schema statements.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TxRequest {
    #[serde(default)]
    statement: Option<String>,
    #[serde(default)]
    parameters: Option<Map<String, Json>>,
    #[serde(default)]
    include_counters: bool,
    #[serde(default)]
    access_mode: AccessMode,
}

/// The statement-less transaction answer: `{"bookmarks": []}` (ADR-0237) —
/// under the request's negotiated media type, like every other tx reply.
fn bookmarks_only_reply(status: u16, media_type: ResponseMediaType) -> Reply {
    Reply::json_as(
        status,
        ReplyBody::new([("bookmarks", Json::Array(Vec::new()))]),
        media_type,
    )
}

/// A tx body parses leniently: an empty body is a pure BEGIN/COMMIT
/// (Browser sends exactly `{"accessMode":"READ"}` for a read BEGIN).
fn parse_tx_body(body: &[u8]) -> Result<TxRequest, Reply> {
    if body.is_empty() {
        return Ok(TxRequest::default());
    }
    serde_json::from_slice(body).map_err(invalid_request_body)
}

/// The `transaction` block of a tx response: the id and the deadline the
/// transaction will be swept at — the wall-clock rendering of the entry's
/// `last_activity + idle_timeout` (ADR-0435 §1). The typed limits' `None`
/// idle timeout means expiry is disabled: there is no deadline to report,
/// so the block is just the id.
fn tx_block(id: &str, last_activity: Instant, idle_timeout: Option<Duration>) -> Json {
    match idle_timeout {
        Some(idle_timeout) => json!({
            "id": id,
            "expires": expires_at(last_activity, idle_timeout),
        }),
        None => json!({"id": id}),
    }
}

/// The RFC 3339 deadline of an entry's idle expiry. Enforcement runs on the
/// monotonic clock (`last_activity.elapsed()`); an `Instant` carries no
/// wall-clock anchor, so the deadline renders as "now plus the deadline's
/// remaining idle allowance" — the same instant. A wall-clock step
/// therefore moves only the rendering, never the enforcement.
///
/// The render is infallible by construction: [`crate::TxLimits::new`]
/// refuses an idle timeout whose `now + idle_timeout + RENDER_MARGIN`
/// horizon the render cannot represent (the validation runs this same
/// `Utc::now()` arithmetic at startup, with a day of uptime safety), and
/// `remaining` never exceeds the configured timeout. The `expect` states
/// that invariant; there is no silent omission path.
fn expires_at(last_activity: Instant, idle_timeout: Duration) -> String {
    let remaining = idle_timeout.saturating_sub(last_activity.elapsed());
    render_deadline(remaining)
        .expect("TxLimits::new refuses an idle timeout past the renderable deadline range")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Run the tx body's optional statement under its access-mode guard. A
/// `READ` transaction runs read statements only (classified through
/// EXPLAIN, like the MCP tools); anything else is a failure envelope and
/// the transaction stays open for the client to roll back. The outcome is
/// `run_on`'s own `Ok`/`Err` — the success/failure fact is never
/// re-derived from a wire status by a consumer.
fn run_tx_statement(
    entry: &mut TxEntry,
    body: TxRequest,
    media_type: ResponseMediaType,
    dialect: RequestDialect,
    success_status: u16,
) -> Result<Reply, Reply> {
    // The body is owned: its parameter map moves into the statement run
    // once, never cloned.
    let TxRequest {
        statement,
        parameters,
        include_counters,
        ..
    } = body;
    let Some(statement) = statement else {
        // No statement: the bookmarks-only reply, born with the route's
        // own success status.
        return Ok(bookmarks_only_reply(success_status, media_type));
    };
    if matches!(entry.mode, AccessMode::Read) {
        let explained = match entry.conn.query(&format!("EXPLAIN {statement}")) {
            Ok(explained) => explained,
            Err(e) => return Err(run_error(&e)),
        };
        if explained.header.query_type != relate::QueryType::Read {
            return Err(crate::reply::bad_request(
                "writing is not allowed in a READ transaction",
            ));
        }
    }
    run_on(
        &mut entry.conn,
        &statement,
        parameters,
        include_counters,
        media_type,
        dialect,
        success_status,
    )
}

/// `POST /db/{db}/query/v2/tx`: open an explicit transaction (the engine's
/// `Connection::begin`, ADR-0028), run the body's optional statement, and
/// answer `{"transaction": {id, expires}, …}` with 201. A failing begin
/// statement rolls the fresh transaction back instead of leaking it.
pub(crate) fn begin(
    db: &Database,
    database: &str,
    request_body: &[u8],
    media_type: ResponseMediaType,
    txs: &TxRegistry,
    owner: Option<&str>,
    dialect: RequestDialect,
) -> Result<Reply, Reply> {
    let body = parse_tx_body(request_body)?;
    // The cap slot is reserved after the sweep and before the connection
    // exists (ADR-0435 §2): an over-cap begin never takes a snapshot pin,
    // an entry the sweep reclaimed frees its slot for this begin, and the
    // reservation holds the slot for this begin's whole admitted span —
    // every `?` below (or a panic) drops it back.
    let admission = txs.admit().map_err(registry_full)?;
    let mut conn = db
        .connect_namespace(database)
        .map_err(|e| crate::reply::namespace_connect_failure(&e))?;
    conn.begin().map_err(|e| run_error(&e))?;
    // The transaction is born locally and registered only once its begin
    // statement ran: nothing can look it up half-open, so the failure
    // path owns the connection outright — one rollback, no registry
    // chase-back.
    let mut entry = TxEntry::new(conn, body.access_mode, owner.map(str::to_string));
    // The route's 201 is the statement run's own success status: the
    // reply is born with it, never edited afterwards.
    let mut reply = match run_tx_statement(&mut entry, body, media_type, dialect, 201) {
        Ok(reply) => reply,
        Err(reply) => {
            // A failing begin statement rolls the fresh transaction back
            // instead of leaking it.
            roll_back(entry);
            return Err(reply);
        }
    };
    // The begin body's statement completed — the client is alive
    // (ADR-0435 §1: refresh after the statement, never at entry).
    let last_activity = entry.touch();
    let id = admission.register(entry);
    reply.body.insert_member(
        "transaction",
        tx_block(&id, last_activity, txs.limits().idle_timeout()),
    );
    Ok(reply)
}

/// `POST /db/{db}/query/v2/tx/{id}`: run a statement in the open
/// transaction. An empty body is the keep-alive probe (the answer carries
/// no data, just the transaction block).
pub(crate) fn run(
    id: &str,
    request_body: &[u8],
    media_type: ResponseMediaType,
    txs: &TxRegistry,
    owner: Option<&str>,
    dialect: RequestDialect,
) -> Result<Reply, Reply> {
    let body = parse_tx_body(request_body)?;
    // The registry miss folds into the route's 404 through the one lease
    // adapter: an unknown id is a bare 404 — no `transaction` block
    // advertises a transaction the server does not have — while a
    // statement failure inside a live transaction still carries it (the
    // tx survives for the client to roll back).
    let mut lease = lease_or_reply(txs, id, owner)?;
    // The entry is leased out of the registry for the statement's
    // duration, so the registry mutex is released here and the engine runs
    // unlocked: a slow statement in this transaction does not stall
    // another transaction's routes. A statement that unwinds drops the
    // lease, which rolls the transaction back and returns the cap slot.
    let mut outcome = run_tx_statement(lease.entry(), body, media_type, dialect, 200);
    // The statement completed — failed or not, the client is alive — and
    // returning the lease re-stamps the transaction's activity (ADR-0435
    // §1) and puts it back in the registry.
    let last_activity = lease.release();
    // A statement failure inside a live transaction still carries the
    // transaction block (the tx survives for the client to roll back); the
    // block is inserted once, into whichever arm's reply.
    let block = tx_block(id, last_activity, txs.limits().idle_timeout());
    let reply = match &mut outcome {
        Ok(reply) | Err(reply) => reply,
    };
    reply.body.insert_member("transaction", block);
    outcome
}

/// `POST …/tx/{id}/commit`: run the optional final statement, then commit
/// and close. The answer carries no transaction block (the tx is gone).
pub(crate) fn commit(
    id: &str,
    request_body: &[u8],
    media_type: ResponseMediaType,
    txs: &TxRegistry,
    owner: Option<&str>,
    dialect: RequestDialect,
) -> Result<Reply, Reply> {
    let body = parse_tx_body(request_body)?;
    // The final statement runs under the same lease as a run's, so the
    // transaction's snapshot pin keeps counting against the cap for the
    // statement's whole span (ADR-0435 §2) — a long COMMIT body cannot
    // hide capacity.
    let mut lease = lease_or_reply(txs, id, owner)?;
    // One commit site, one error mapping: a failing statement answer drops
    // the lease, which rolls the uncommitted transaction back with its
    // entry (the `?` returns it); anything else commits once and the reply
    // goes out. No last-activity refresh: the entry leaves the registry, so
    // there is no deadline left to honor (ADR-0435 §1 refreshes operations
    // that keep it alive).
    let reply = run_tx_statement(lease.entry(), body, media_type, dialect, 200)?;
    let mut entry = lease.take();
    if let Err(e) = entry.conn.commit() {
        return Err(run_error(&e));
    }
    Ok(reply)
}

/// `DELETE …/tx/{id}`: roll back and close — the 202 honors the request's
/// negotiated media type like every other tx reply. The lease is the
/// rollback's whole pin accounting, exactly as it is the commit's: the
/// entry is out of the map and still counted until [`Lease::take`] hands
/// it over.
pub(crate) fn rollback(
    id: &str,
    txs: &TxRegistry,
    media_type: ResponseMediaType,
    owner: Option<&str>,
) -> Result<Reply, Reply> {
    let lease = lease_or_reply(txs, id, owner)?;
    let mut entry = lease.take();
    match entry.conn.rollback() {
        Ok(()) => Ok(Reply::json_as(202, ReplyBody::empty(), media_type)),
        Err(e) => Err(run_error(&e)),
    }
}

/// Lease `id` for one statement route and fold a registry miss into the
/// route's reply: [`TxRegistry::lease`] with the miss mapped through
/// [`lookup_miss`]. The one entry point the run, commit, and rollback routes
/// call, so the miss mapping is not re-spelled per route. The registry
/// itself never names a reply type — this adapter is the route family's.
fn lease_or_reply<'a>(
    txs: &'a TxRegistry,
    id: &str,
    owner: Option<&str>,
) -> Result<Lease<'a>, Reply> {
    txs.lease(id, owner).map_err(|miss| lookup_miss(id, miss))
}

/// The tx teardown policy: take the entry and roll its connection back.
/// Dropping the connection is the documented backstop (an uncommitted
/// `Connection::begin` transaction rolls back on drop), so a rollback
/// failure cannot leak a transaction — the explicit call exists for the
/// error's own sake, not the cleanup's.
fn roll_back(mut entry: TxEntry) {
    let _ = entry.conn.rollback();
}

/// The 404 for an unknown or closed transaction id.
fn tx_not_found(id: &str) -> Reply {
    error_envelope(
        404,
        GQLSTATUS_GENERAL,
        &format!("transaction {id} not found"),
    )
}

/// The 404 for a transaction this operation took and found idle past the
/// deadline (ADR-0435 §3): the status matches the unknown-id recovery — the
/// client abandons the block and retries the same way — while the typed
/// code marks the expiry for logs and telemetry.
fn tx_expired(id: &str, idle_timeout: Duration) -> Reply {
    error_envelope(
        404,
        "42N90",
        &format!("transaction {id} expired after {idle_timeout:?} idle"),
    )
}

/// The 429 for a begin the registry cap refused (ADR-0435 §3): the
/// message names the configured cap and the flag that raises it.
fn registry_full(max_open: usize) -> Reply {
    error_envelope(
        429,
        "42N91",
        &format!(
            "transaction registry full: {max_open} open transactions \
             (close one or raise --max-open-transactions)"
        ),
    )
}

/// The reply a registry miss maps to: the typed expiry for the operation
/// that took the id and found it expired, the generic not-found for
/// everything else (an unknown id, another caller's transaction, or an
/// entry an earlier operation's sweep already reclaimed). The expired arm
/// carries the timeout the entry exceeded — an expired miss exists only
/// when an idle timeout is configured — so this rendering needs no limits
/// argument and no `expect`.
fn lookup_miss(id: &str, miss: LookupMiss) -> Reply {
    match miss {
        LookupMiss::Expired(idle_timeout) => tx_expired(id, idle_timeout),
        LookupMiss::Missing => tx_not_found(id),
    }
}
