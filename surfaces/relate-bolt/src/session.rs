//! One Bolt session (ADR-0233): the gates every message passes — the
//! pre-HELLO auth gate (ADR-0126), the rejected-HELLO state (ADR-0313),
//! the 5.x AUTHENTICATION gate between HELLO and LOGON (ADR-0484), RESET
//! priority over queued work (ADR-0317), the failed state (ADR-0110) and
//! protocol breaches (ADR-0314) — and every handler, including
//! RUN/PULL/DISCARD over the result store in `results.rs`. The typed
//! request vocabulary lives in `requests.rs` (ADR-0315); the request-field
//! decoders live in `fields.rs`.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use relate::{Connection, Executed, Value};

use crate::auth::AuthConfig;
use crate::dialect::Dialect;
use crate::failures::{
    send_engine_failure, send_failure_with_code, send_ignored, workspace_refusal,
    FAILED_HELLO_DELAY_MS, REQUEST_INVALID_CODE, UNAUTHORIZED_CODE,
};
use crate::fields::{
    bolt_agent_is_string_map, db_name, extra_str, illegal_value_for_field,
    metadata_notifications_failure, patch_names, ConsumeRequest, NotificationConfig, RunRequest,
};
use crate::keepalive::{run_statement, with_keep_alive};
use crate::packstream::{
    chunk_message, decode_message, encode, encode_error, struct_tag, DecodeError, Pack,
};
use crate::patches::BoltPatches;
use crate::protocol::{ConnConfig, QueryOutcome};
use crate::requests::{breach_text, request, Message, OverBudget, Refused, Undecodable};
use crate::results::{fails_run, Body, OpenResults, OverCap};
use crate::summary::{self, send_success};
use crate::value_codec::{MdbPackError, ValueCodec};
use crate::version::BoltVersion;
use relate_error::{TRANSACTION_OUT_OF_MEMORY_CODE, UNKNOWN_ERROR_CODE};
use session_pages::{write_page, Page, RecordError};

/// The FAILURE message of a HELLO without a string `user_agent` (ADR-0313),
/// Neo4j's text.
pub(crate) const USER_AGENT_REQUIRED: &str =
    "Illegal value for field \"user_agent\": Expected value to be non-null";

/// The FAILURE message of a 5.x HELLO whose `bolt_agent` is not a map of
/// string keys and string values (ADR-0484 decision 3), Neo4j's text.
pub(crate) const BOLT_AGENT_REQUIRED: &str =
    "Illegal value for field \"bolt_agent\": Must be a map with string keys and string values.";

/// The FAILURE message of a 4.4 HELLO that does not authenticate
/// (ADR-0126); the 5.x LOGON's own spelling differs (ADR-0484 decision
/// 4).
const HELLO_UNAUTHORIZED: &str = "authentication failed";

/// The FAILURE message of a LOGON that does not authenticate (ADR-0484
/// decision 4), the probed Neo4j 2026.09 text — unlike the 4.4 HELLO
/// failure's own text.
pub(crate) const LOGON_UNAUTHORIZED: &str =
    "The client is unauthorized due to authentication failure.";

/// Whether the session continues after a message.
pub(crate) enum Flow {
    Continue,
    Close,
}

/// Where the session stands with HELLO and LOGON (ADR-0126, ADR-0313,
/// ADR-0484 decision 4).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// No HELLO has validated yet.
    AwaitingHello,
    /// A HELLO without `user_agent` (4.4) or with a malformed 5.x field
    /// was rejected; no session is open.
    HelloRejected,
    /// A 5.x HELLO validated but no LOGON has authenticated: the Bolt
    /// AUTHENTICATION state, where only LOGON is answered.
    AwaitingLogon,
    /// The session is open.
    Open,
}

/// One Bolt session's mutable state (ADR-0233): the engine `Connection`
/// (owned by the connection thread for the session's lifetime), the HELLO
/// and LOGON gates, the failed state (ADR-0110), the open results
/// (ADR-0311), the negotiated dialect — the handshake's version and the
/// validated HELLO's patches (ADR-0329, ADR-0484) — and the notifications
/// filter its 5.2 keys configure (ADR-0484 decision 8).
pub(crate) struct Session {
    conn: Connection,
    gate: Gate,
    failed: bool,
    results: OpenResults,
    /// This connection's id, sent as the HELLO SUCCESS `connection_id`
    /// metadata (ADR-0293); allocated once at session start.
    connection_id: String,
    /// The connection's negotiated dialect (ADR-0329, ADR-0484): the
    /// version the handshake negotiated — it gates the request vocabulary;
    /// the 5.x messages (LOGON, LOGOFF, TELEMETRY) decode only at 5.4, and
    /// at 4.4 stay fatal unknowns (ADR-0315) — and the `patch_bolt`
    /// patches the validated 4.4 HELLO negotiated (none at 5.x, where the
    /// HELLO ignores `patch_bolt`). The one bundle the value codec and the
    /// RUN parameter decode read.
    dialect: Dialect,
    /// The connection-level notifications filter the 5.4 HELLO's keys
    /// configured (ADR-0484 decision 8); the default filters nothing, and
    /// a 4.4 connection never leaves it.
    notifications: NotificationConfig,
    /// The notifications config the transaction's BEGIN selected
    /// (ADR-0484 decision 8): the BEGIN keys' overlay over
    /// [`Self::notifications`] at 5.4, or the connection config itself
    /// at 4.4, where the keys are ignored. Read only inside a
    /// transaction — `run` selects its base by `conn.in_transaction()`
    /// — stored by [`Self::begin_tx`], restored to the connection
    /// config by [`Self::end_tx`] together with the results.
    tx_notifications: NotificationConfig,
    /// Host counter for a finished query. Shared by every connection of
    /// the listener; `None` on a server that does not count.
    on_query: Option<Arc<dyn Fn(QueryOutcome) + Send + Sync>>,
}

impl Session {
    pub(crate) fn new(conn: Connection, config: &ConnConfig, version: BoltVersion) -> Self {
        Self {
            conn,
            // With AuthConfig::None every 4.4 connection starts open;
            // otherwise the validated HELLO opens it. A 5.4 connection
            // always starts awaiting HELLO: the 5.x HELLO does not
            // authenticate (ADR-0484 decision 3), so the session opens at
            // LOGON under every auth config (decision 4).
            gate: if matches!(*config.auth, AuthConfig::None) && !version.speaks_5x() {
                Gate::Open
            } else {
                Gate::AwaitingHello
            },
            failed: false,
            results: OpenResults::default(),
            connection_id: config.next_connection_id(),
            // No patch is negotiated until the validated 4.4 HELLO lists
            // one (ADR-0329); the version is the handshake's (ADR-0484).
            dialect: Dialect::new(BoltPatches::default(), version),
            notifications: NotificationConfig::default(),
            tx_notifications: NotificationConfig::default(),
            on_query: config.on_query.clone(),
        }
    }

    fn note_query(&self, outcome: QueryOutcome) {
        if let Some(on_query) = &self.on_query {
            on_query(outcome);
        }
    }

    /// Serve one batch of framed messages (ADR-0317): every message queued
    /// ahead of the batch's last reachable RESET that has not started
    /// answers IGNORED; the RESETs and everything after the last one run
    /// normally ([`last_reset`] locates that RESET).
    ///
    /// Messages are decoded one at a time, never as a batch: the RESET
    /// pre-scan decodes the decodable prefix one message at a time, and the
    /// dispatch decodes one message at a time, dropping it before the next.
    /// A batch's peak heap is one message's values, not the sum over its
    /// messages (SEC-serve-batch).
    pub(crate) fn serve_batch<W: Write>(
        &mut self,
        stream: &mut W,
        batch: Vec<Vec<u8>>,
        config: &ConnConfig,
    ) -> Result<Flow, Box<dyn std::error::Error + Send + Sync>> {
        let last_reset = last_reset(&batch);
        for (index, payload) in batch.iter().enumerate() {
            let queued_ahead_of_reset = last_reset.is_some_and(|reset| index < reset);
            let decoded = match decode_message(payload) {
                Ok(pack) => request(pack, self.dialect.version).map_err(Refused::Undecodable),
                Err(DecodeError::BudgetExceeded { at_least_bytes }) => {
                    Err(Refused::OverBudget(OverBudget {
                        at_least_bytes,
                        bytes: payload.len(),
                        message: struct_tag(payload)
                            .and_then(|tag| Message::from_signature(tag, self.dialect.version)),
                    }))
                }
                Err(fatal) => return Err(fatal.into()),
            };
            if let Flow::Close = self.dispatch(stream, decoded, queued_ahead_of_reset, config)? {
                return Ok(Flow::Close);
            }
        }
        Ok(Flow::Continue)
    }

    /// Dispatch one decoded message through the state machine: each
    /// [`Gate`] state's arm owns that state's whole message policy — the
    /// pre-HELLO auth gate (ADR-0126), the rejected-HELLO state
    /// (ADR-0313), the 5.x AUTHENTICATION gate (ADR-0484 decision 4),
    /// and the open session's RESET priority (ADR-0317), failed state
    /// (ADR-0110) and breaches (ADR-0314) ahead of its handlers.
    /// GOODBYE closes from every state; wire-write errors propagate.
    fn dispatch<W: Write>(
        &mut self,
        stream: &mut W,
        decoded: Result<(Message, Vec<Pack>), Refused>,
        queued_ahead_of_reset: bool,
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        match self.gate {
            // Auth (ADR-0126): with a credential configured, nothing but
            // HELLO runs until a HELLO has validated (GOODBYE is always
            // allowed). A client that skips HELLO gets unauthorized — not
            // a silent session. The arm answers before the fatal decode,
            // so an unknown message before HELLO is answered the same
            // way, and so is an over-budget one (ADR-0577). At 5.4 the gate always starts armed (the HELLO does
            // not authenticate there, ADR-0484 decision 3), so it holds
            // the pre-HELLO stretch under every auth config.
            Gate::AwaitingHello => match decoded {
                Ok((Message::Hello, fields)) => self.hello(stream, &fields, config),
                Ok((Message::Goodbye, _)) => Ok(Flow::Close),
                _ => {
                    send_failure_with_code(
                        stream,
                        UNAUTHORIZED_CODE,
                        "HELLO with valid credentials required first",
                    )?;
                    Ok(Flow::Close)
                }
            },
            // ADR-0313: no session is open. A second HELLO is a breach in
            // Neo4j's AUTHENTICATION state; everything else but GOODBYE
            // (which always closes) is IGNORED.
            Gate::HelloRejected => match decoded {
                Err(Refused::Undecodable(undecodable)) => Ok(undecodable_exit(undecodable)),
                Err(Refused::OverBudget(_)) => {
                    send_ignored(stream)?;
                    Ok(Flow::Continue)
                }
                Ok((Message::Hello, _)) => {
                    self.send_breach(stream, Message::Hello, "AUTHENTICATION")
                }
                Ok((Message::Goodbye, _)) => Ok(Flow::Close),
                Ok(_) => {
                    send_ignored(stream)?;
                    Ok(Flow::Continue)
                }
            },
            // ADR-0484 decision 4: the 5.x AUTHENTICATION state accepts
            // only LOGON (GOODBYE always closes) — a LOGON queued ahead
            // of a RESET is IGNORED with the rest of the work. Everything
            // else — RUN included — is the ADR-0314 breach naming the
            // AUTHENTICATION state, then the close.
            Gate::AwaitingLogon => match decoded {
                Err(Refused::Undecodable(undecodable)) => Ok(undecodable_exit(undecodable)),
                // ADR-0577: an over-budget message is not a LOGON, so it
                // is this gate's breach — the FAILURE naming the limit,
                // then the close; queued ahead of a RESET it is IGNORED.
                Err(Refused::OverBudget(over)) => {
                    if queued_ahead_of_reset && over.interruptible() {
                        send_ignored(stream)?;
                        Ok(Flow::Continue)
                    } else {
                        self.over_budget_exit(stream, &over)
                    }
                }
                Ok((Message::Goodbye, _)) => Ok(Flow::Close),
                Ok((message @ Message::Logon, fields)) => {
                    // ADR-0317, consulted through the never-interrupted
                    // set like the open session's guard below: LOGON is
                    // interruptible work, so queued ahead of the batch's
                    // last RESET it answers IGNORED.
                    if queued_ahead_of_reset && !message.never_interrupted() {
                        send_ignored(stream)?;
                        Ok(Flow::Continue)
                    } else {
                        self.logon(stream, &fields, config)
                    }
                }
                Ok((message, _)) => self.send_breach(stream, message, "AUTHENTICATION"),
            },
            // The open session: RESET priority over queued work
            // (ADR-0317), the failed state (ADR-0110), the breaches
            // (ADR-0314), then the handlers.
            Gate::Open => {
                let (message, fields) = match decoded {
                    Ok(request) => request,
                    Err(Refused::Undecodable(undecodable)) => {
                        return Ok(undecodable_exit(undecodable))
                    }
                    Err(Refused::OverBudget(over)) => {
                        return self.over_budget_open(stream, &over, queued_ahead_of_reset)
                    }
                };
                // ADR-0317, through the never-interrupted set: HELLO,
                // GOODBYE and the batch's RESETs are never answered
                // IGNORED; every other message queued ahead of the
                // batch's last RESET is.
                if queued_ahead_of_reset && !message.never_interrupted() {
                    send_ignored(stream)?;
                    return Ok(Flow::Continue);
                }
                // Failed state (ADR-0110, ADR-0314): the model owns which
                // messages are IGNOREd until RESET clears it — HELLO,
                // RESET and ROUTE are always answered.
                if self.failed && message.ignored_while_failed() {
                    send_ignored(stream)?;
                    return Ok(Flow::Continue);
                }
                if self.is_breach(message) {
                    return self.send_breach(stream, message, self.state_name());
                }
                match message {
                    Message::Hello => self.hello(stream, &fields, config),
                    Message::Reset => self.reset(stream),
                    Message::Run => self.run(stream, &fields, config),
                    Message::Pull => self.pull(stream, &fields),
                    Message::Discard => self.discard(stream, &fields),
                    Message::Begin => self.begin(stream, &fields),
                    Message::Commit => self.commit(stream, config),
                    Message::Rollback => self.rollback(stream),
                    Message::Route => self.route(stream, &fields, config),
                    // ADR-0484 decision 4: LOGON is received in the
                    // AUTHENTICATION state only — the AwaitingLogon arm
                    // answers it there — so an open session answers it
                    // with the ADR-0314 breach naming its own state.
                    Message::Logon => self.send_breach(stream, message, self.state_name()),
                    Message::Logoff => self.logoff(stream),
                    Message::Telemetry => self.telemetry(stream, &fields),
                    Message::Goodbye => Ok(Flow::Close),
                }
            }
        }
    }

    /// A request the open session's state cannot handle (ADR-0314): COMMIT
    /// or ROLLBACK with no transaction, BEGIN inside one, PULL or DISCARD
    /// with no open result. Consulted by the open session's arm only —
    /// every other gate answers its messages whole — and LOGON is never
    /// one: the open session's handler match answers it with its own
    /// breach (ADR-0484 decision 4).
    fn is_breach(&self, message: Message) -> bool {
        match message {
            Message::Commit | Message::Rollback => !self.conn.in_transaction(),
            Message::Begin => self.conn.in_transaction(),
            Message::Pull | Message::Discard => self.results.is_empty(),
            // The handlers answer these from any open session state.
            Message::Hello
            | Message::Goodbye
            | Message::Reset
            | Message::Run
            | Message::Route
            | Message::Logon
            | Message::Logoff
            | Message::Telemetry => false,
        }
    }

    /// The Neo4j 4.4 state the session is in, as its breach text names it.
    fn state_name(&self) -> &'static str {
        match (self.conn.in_transaction(), self.results.is_empty()) {
            (false, true) => "READY",
            (false, false) => "STREAMING",
            (true, true) => "IN_TRANSACTION",
            (true, false) => "TX_STREAMING",
        }
    }

    /// The credential gate the 4.4 HELLO and the 5.x LOGON share
    /// (ADR-0126, ADR-0484 decision 4): check the extra map at
    /// `fields[0]`'s scheme/principal/credentials triple against the
    /// configured auth, and on a mismatch wait out the ADR-0139 guessing
    /// delay — a failed attempt is rate-limited, a successful one is
    /// answered at once, and blocking this connection's own thread is the
    /// correct semantics (ADR-0233) — before the Unauthorized FAILURE,
    /// `unauthorized_text`, each caller's probed spelling, and the close.
    /// A match opens the session and reports the verified principal
    /// engine-ward (ADR-0273: a validated attempt under basic auth has
    /// just proven the principal equals the preset user, so
    /// `dbms.showCurrentUser` then answers the real user; under
    /// `AuthConfig::None` the principal is unverified and is never
    /// forwarded — the procedure keeps the documented placeholder).
    ///
    /// `Ok(None)` — authenticated, the session open: the caller answers
    /// its own SUCCESS. `Ok(Some(flow))` — the FAILURE was sent; return
    /// the flow.
    fn authenticate<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        config: &ConnConfig,
        unauthorized_text: &str,
    ) -> std::io::Result<Option<Flow>> {
        if !crate::auth::check_hello(
            &config.auth,
            extra_str(fields, 0, "scheme"),
            extra_str(fields, 0, "principal"),
            extra_str(fields, 0, "credentials"),
        ) {
            std::thread::sleep(Duration::from_millis(FAILED_HELLO_DELAY_MS));
            send_failure_with_code(stream, UNAUTHORIZED_CODE, unauthorized_text)?;
            return Ok(Some(Flow::Close));
        }
        self.gate = Gate::Open;
        if let Some(user) = config.auth.verified_principal() {
            self.conn.set_principal(Some(user));
        }
        Ok(None)
    }

    /// The rejected-HELLO transition (ADR-0313, ADR-0484 decision 3):
    /// set the rejected-HELLO gate, answer the Request.Invalid FAILURE —
    /// `text`, the probed spelling of the field this caller rejected —
    /// and continue; the state then answers what follows (a re-HELLO is
    /// the AUTHENTICATION breach, everything else but GOODBYE IGNORED).
    /// The one spelling of the transition: the `user_agent` rejection
    /// every version takes and the 5.x `bolt_agent` and notifications
    /// rejections all route through here.
    fn reject_hello<W: Write>(&mut self, stream: &mut W, text: &str) -> std::io::Result<Flow> {
        self.gate = Gate::HelloRejected;
        send_failure_with_code(stream, REQUEST_INVALID_CODE, text)?;
        Ok(Flow::Continue)
    }

    /// HELLO (0x01). A HELLO without a string `user_agent` is rejected
    /// first (ADR-0313). Bolt 4.4 carries the auth token in the HELLO map
    /// (scheme/principal/credentials); with auth enabled (ADR-0126) a
    /// missing or mismatched token is a FAILURE and the connection closes.
    /// At 5.4 the HELLO does not authenticate at all (ADR-0484 decision 3):
    /// [`Session::hello_5x`] validates the 5.x fields and leaves the
    /// session awaiting LOGON.
    fn hello<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        if extra_str(fields, 0, "user_agent").is_none() {
            return self.reject_hello(stream, USER_AGENT_REQUIRED);
        }
        if self.dialect.version.speaks_5x() {
            return self.hello_5x(stream, fields, config);
        }
        // The 4.4 credential gate: the shared check, delay, Unauthorized
        // FAILURE, close, gate and principal sequence, this arm's probed
        // failure text.
        if let Some(failed) = self.authenticate(stream, fields, config, HELLO_UNAUTHORIZED)? {
            return Ok(failed);
        }
        // ADR-0329: the HELLO extra's `patch_bolt` list is the negotiation
        // input; `fields::patch_names` owns the extra-map lookup and the
        // string-entry rule, and `summary::patch_bolt` owns the SUCCESS
        // entry's Pack shape.
        self.dialect.patches = BoltPatches::from_hello(patch_names(fields));
        let mut metadata = summary::hello(
            &config.server_identity,
            &self.connection_id,
            config.limits.keep_alive_interval,
            // The 4.4 HELLO carries no version-gated hint entries.
            Vec::new(),
        );
        if let Some(names) = self.dialect.patches.hello_metadata() {
            metadata.push(summary::patch_bolt(&names));
        }
        send_success(stream, metadata)?;
        Ok(Flow::Continue)
    }

    /// RESET (0x0F): rollback an open txn (ADR-0028), clear the failed
    /// state, end the transaction-scoped session state (ADR-0311,
    /// ADR-0484 decision 8) and answer SUCCESS.
    fn reset<W: Write>(&mut self, stream: &mut W) -> std::io::Result<Flow> {
        self.end_tx();
        if self.conn.in_transaction() {
            // A failed rollback (storage I/O) is a FAILURE, not a clean
            // reset: the failed state stays set, so a retrying RESET gets
            // another attempt instead of the session pretending it worked.
            if let Err(e) = self.conn.rollback() {
                return self.fail_engine(stream, &e);
            }
        }
        self.failed = false;
        send_success(stream, vec![])?;
        Ok(Flow::Continue)
    }

    /// The version-gated notifications parse BEGIN and RUN share
    /// (ADR-0484 decision 8): at 5.4 the extra map at `fields[index]`'s
    /// notifications keys overlay `base`; at 4.4 the keys are ignored
    /// and the base stands. A value that does not parse is the `Err` the
    /// caller fails its message with before anything runs.
    fn overlay_notifications(
        &self,
        fields: &[Pack],
        index: usize,
        base: &NotificationConfig,
    ) -> Result<NotificationConfig, String> {
        if !self.dialect.version.speaks_5x() {
            return Ok(base.clone());
        }
        match NotificationConfig::parse(fields, index) {
            Ok(statement) => Ok(NotificationConfig::overlay(&statement, base)),
            Err(parse_failure) => Err(parse_failure),
        }
    }

    /// BEGIN (0x11). ADR-0117 §7: BEGIN selects the transaction's namespace
    /// from its extra's `db`, applied before `begin()` because
    /// `set_namespace` fails while a transaction is open (a BEGIN inside
    /// one is a breach and never reaches here, ADR-0314). The extra's
    /// `bookmarks` are accepted and ignored (ADR-0310). At 5.4 the extra's
    /// notifications keys (ADR-0484 decision 8) overlay the HELLO config
    /// for the transaction's statements — an invalid value fails the
    /// BEGIN before anything starts; at 4.4 the keys are ignored and the
    /// connection config stands. The transaction starts with no open
    /// results and qids from 0 (ADR-0311).
    fn begin<W: Write>(&mut self, stream: &mut W, fields: &[Pack]) -> std::io::Result<Flow> {
        let tx_notifications = match self.overlay_notifications(fields, 0, &self.notifications) {
            Ok(config) => config,
            Err(parse_failure) => return self.fail_metadata_notifications(stream, &parse_failure),
        };
        if let Err(e) = apply_extra_db(&mut self.conn, fields, 0) {
            return self.fail_engine(stream, &e);
        }
        if let Err(e) = self.conn.begin() {
            return self.fail_engine(stream, &e);
        }
        self.begin_tx(tx_notifications);
        send_success(stream, vec![])?;
        Ok(Flow::Continue)
    }

    /// Open the transaction-scoped session state (ADR-0311, ADR-0484
    /// decision 8): the notifications config the transaction's
    /// statements read — the BEGIN keys' selection, the connection
    /// config itself at 4.4, where the keys are ignored — and the empty
    /// result store a transaction starts with. [`Self::end_tx`] is the
    /// mirrored end every transaction-ending path shares.
    fn begin_tx(&mut self, notifications: NotificationConfig) {
        self.tx_notifications = notifications;
        self.results.clear();
    }

    /// End the transaction-scoped session state together: every open
    /// result (ADR-0311) and the notifications selection (ADR-0484
    /// decision 8), restored to the connection config — the one end
    /// COMMIT, ROLLBACK and RESET call, so a transaction-ending path
    /// cannot drift.
    fn end_tx(&mut self) {
        self.results.clear();
        self.tx_notifications = self.notifications.clone();
    }

    /// COMMIT (0x12): one WAL commit under the NOOP keep-alive (ADR-0312);
    /// SUCCESS carries the bookmark the engine's commit returned — the
    /// store's commit sequence (ADR-0310, ADR-0502 §7) — or is `{}` when
    /// the transaction committed nothing. Every open result is dropped
    /// (ADR-0311), and the transaction's notifications selection ends
    /// with it (ADR-0484 decision 8).
    fn commit<W: Write>(&mut self, stream: &mut W, config: &ConnConfig) -> std::io::Result<Flow> {
        self.end_tx();
        let conn = &mut self.conn;
        let committed = with_keep_alive(stream, config.limits.keep_alive_interval, |heartbeat| {
            conn.commit_with_heartbeat(heartbeat)
        })?;
        let bookmark = match committed {
            Ok(bookmark) => bookmark,
            Err(e) => return self.fail_engine(stream, &e),
        };
        send_success(stream, summary::commit(bookmark))?;
        Ok(Flow::Continue)
    }

    /// ROLLBACK (0x13): discard the transaction's writes and every open
    /// result (ADR-0311), and the transaction's notifications selection
    /// with them (ADR-0484 decision 8).
    fn rollback<W: Write>(&mut self, stream: &mut W) -> std::io::Result<Flow> {
        self.end_tx();
        if let Err(e) = self.conn.rollback() {
            return self.fail_engine(stream, &e);
        }
        send_success(stream, vec![])?;
        Ok(Flow::Continue)
    }

    /// ROUTE (0x66, ADR-0269; self address per ADR-0140; home database and
    /// role order per ADR-0318): a single-server self routing table.
    fn route<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        let requested_address = extra_str(fields, 0, "address");
        // Same decode and the same home-database default as RUN/BEGIN
        // (ADR-0117 §7, ADR-0318).
        let db = db_name(fields, 2);
        // ADR-0140: advertise the address the client dialed when it sent
        // one, else this connection's local socket address — decided at the
        // transport boundary: `serve_conn` sets `local_address` from the
        // socket or closes the connection.
        let self_address = requested_address.unwrap_or(&config.local_address);
        send_success(stream, summary::route(self_address, db))?;
        Ok(Flow::Continue)
    }

    /// RUN (0x10) cypher params extra. The statement executes eagerly and
    /// its result is buffered (ADR-0110) under its own qid in an explicit
    /// transaction (ADR-0311); an auto-commit RUN replaces the previous
    /// result.
    fn run<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        config: &ConnConfig,
    ) -> std::io::Result<Flow> {
        let in_transaction = self.conn.in_transaction();
        if !in_transaction {
            self.results.clear();
        }
        // ADR-0342: the statement field is required and the params must
        // convert; both malformed shapes are the same request failure
        // (ADR-0177) and nothing executes. The dialect rides the decode:
        // at 5.4 the DateTime parameter pair is the fixed UTC forms
        // (ADR-0484 decision 7).
        let RunRequest { cypher, params } = match RunRequest::parse(fields, self.dialect) {
            Ok(request) => request,
            Err(message) => return self.fail_with_code(stream, REQUEST_INVALID_CODE, &message),
        };
        // ADR-0484 decision 8: at 5.4 the RUN extra's notifications keys
        // overlay the base — the transaction's BEGIN selection inside
        // one, else the HELLO config — for this statement's ending
        // summary; an invalid value fails the RUN before anything
        // executes. At 4.4 the keys are ignored and the base stands.
        let base = if in_transaction {
            &self.tx_notifications
        } else {
            &self.notifications
        };
        let notifications = match self.overlay_notifications(fields, 2, base) {
            Ok(config) => config,
            Err(parse_failure) => return self.fail_metadata_notifications(stream, &parse_failure),
        };
        // ADR-0117 §7: an auto-commit RUN selects its namespace per request
        // from the extra's `db` (no carry-over). Inside an explicit
        // transaction RUN carries no `db` and keeps the namespace chosen at
        // BEGIN. The extra's `bookmarks` are accepted and ignored
        // (ADR-0310).
        if let Err(e) = apply_extra_db(&mut self.conn, fields, 2) {
            return self.fail_engine(stream, &e);
        }
        let staged = match run_statement(
            &mut self.conn,
            stream,
            &cypher,
            params,
            config.limits.query_timeout,
            config.limits.keep_alive_interval,
        )? {
            Ok(staged) => staged,
            // ADR-0319: a failure before execution fails the RUN.
            Err(e) => return self.fail_engine(stream, &e),
        };
        let (body, headline) = match staged {
            Executed::Rows(result) => {
                let headline = RunHeadline {
                    columns: result.header.columns.clone(),
                    column_types: result.header.column_types.clone(),
                    t_first: result.header.elapsed,
                };
                (
                    Body::Rows {
                        result: Box::new(result),
                        cursor: 0,
                    },
                    headline,
                )
            }
            Executed::Failed(failure) if fails_run(&failure.error) => {
                return self.fail_engine(stream, &failure.error);
            }
            Executed::Failed(failure) => (
                Body::Failed(failure.error),
                RunHeadline {
                    columns: failure.header.columns,
                    column_types: failure.header.column_types,
                    t_first: failure.header.elapsed,
                },
            ),
        };
        // ADR-0110/0311: the reservation measures the result against the
        // cap minus the bytes every open result already holds; `0` disables
        // the cap. An over-cap result is dropped without being stored. The
        // statement text is kept on the result for the ending summary's
        // plan pair (ADR-0357), built at the ending where the plan tree is
        // at hand too.
        let qid =
            match self
                .results
                .open(body, config.limits.max_result_bytes, cypher, notifications)
            {
                Ok(qid) => qid,
                Err(OverCap) => {
                    return self.fail_with_code(
                        stream,
                        TRANSACTION_OUT_OF_MEMORY_CODE,
                        &format!(
                            "query result exceeds max-result-bytes limit of {} bytes",
                            config.limits.max_result_bytes
                        ),
                    )
                }
            };
        // ADR-0309: the RUN SUCCESS carries the row metadata — `fields`,
        // `columnTypes` (ADR-0224), `t_first` and the transaction `qid`.
        // The EXPLAIN/PROFILE plan rides the ending summary (ADR-0357).
        let run_summary = summary::run(
            &headline.columns,
            &headline.column_types,
            headline.t_first,
            in_transaction.then_some(qid),
        );
        send_success(stream, run_summary)?;
        Ok(Flow::Continue)
    }

    /// PULL {n, qid} (0x3F): at most `n` buffered RECORDs of the addressed
    /// result, `n = -1` drains (ADR-0110, ADR-0311).
    fn pull<W: Write>(&mut self, stream: &mut W, fields: &[Pack]) -> std::io::Result<Flow> {
        self.consume(stream, fields, Consume::Pull)
    }

    /// DISCARD {n, qid} (0x2F): drop at most `n` rows of the addressed
    /// result, `n = -1` drops the rest (ADR-0311).
    fn discard<W: Write>(&mut self, stream: &mut W, fields: &[Pack]) -> std::io::Result<Flow> {
        self.consume(stream, fields, Consume::Discard)
    }

    /// The shared PULL/DISCARD step. A result that leaves rows answers
    /// `{has_more: true}` and stays open; the step that takes its last row
    /// sends the ending summary (ADR-0309) and closes it; a deferred
    /// execution failure answers FAILURE and closes it (ADR-0319). The
    /// session guarantees at least one result is open (ADR-0314).
    fn consume<W: Write>(
        &mut self,
        stream: &mut W,
        fields: &[Pack],
        mode: Consume,
    ) -> std::io::Result<Flow> {
        // ADR-0342: a present-but-malformed `n` or `qid` is a request
        // failure, exactly like an unknown `qid` below; `ConsumeRequest`
        // owns that decode, so every malformed shape leaves through this
        // one exit.
        let request = match ConsumeRequest::parse(fields) {
            Ok(request) => request,
            Err(message) => return self.fail_with_code(stream, REQUEST_INVALID_CODE, &message),
        };
        let Some(mut open) = self.results.take(request.qid) else {
            return self.fail_with_code(
                stream,
                REQUEST_INVALID_CODE,
                &format!("no open result with qid {}", request.qid),
            );
        };
        let started = Instant::now();
        let page = match &mut open.body {
            Body::Rows { result, cursor } => {
                let remaining = result.rows.len() - *cursor;
                let take = request.take(remaining);
                // ADR-0484: the page's rows and the ending summary's plan
                // value encode under the connection's dialect beside the
                // namespace the result's own header stamped — the same
                // fact its ending `db` entry reports.
                let codec = ValueCodec::new(self.dialect, result.header.namespace);
                // PULL writes the rows it takes; DISCARD writes nothing. A
                // row past the packstream depth limit is client-reachable
                // (ADR-0110), so the write's outcome decides the page.
                let written = match mode {
                    Consume::Pull => {
                        write_page(stream, &result.rows[*cursor..*cursor + take], codec)
                    }
                    Consume::Discard => Ok(()),
                };
                // ADR-0309: t_last is the whole milliseconds spent
                // streaming or discarding, summed over the result's PULL
                // and DISCARD messages. This message's share is measured
                // after its work, so the ending summary and every later
                // page carry the real running total.
                open.streamed += started.elapsed();
                match written {
                    // A path-invariant break, an unrepresentable value or
                    // an element with no element id is an engine bug or an
                    // engine-side value problem, never client input: keep
                    // it loud as the io error.
                    Err(RecordError::Fatal(e)) => return Err(e),
                    Err(RecordError::DepthExceeded) => Page::DepthExceeded,
                    Ok(()) => {
                        *cursor += take;
                        if *cursor == result.rows.len() {
                            // ADR-0357: the ending summary carries the plan
                            // pair — Neo4j's placement for the drained
                            // result, so strict clients (cypher-shell) read
                            // it from the PULL or DISCARD that ends the
                            // result. The one plan-summary policy decides
                            // the entry, never a second spelling here. The
                            // query notifications ride beside it (ADR-0363
                            // decision 11), through their one policy too.
                            Page::Ending(summary::ending(
                                result,
                                open.streamed,
                                codec,
                                &open.notification_config,
                            )?)
                        } else {
                            Page::HasMore
                        }
                    }
                }
            }
            Body::Failed(error) => return self.fail_engine(stream, error),
        };
        match page {
            Page::Ending(metadata) => {
                send_success(stream, metadata)?;
                self.note_query(QueryOutcome::Success);
            }
            Page::HasMore => {
                self.results.restore(open);
                send_success(stream, summary::has_more())?;
            }
            Page::DepthExceeded => {
                // ADR-0110: the depth limit is client-reachable (a deeply
                // nested parameter value returned in a row), so it degrades
                // the session — FAILURE, failed state, IGNORED until RESET,
                // after any RECORDs already written, never a killed
                // connection mid-stream. The result keeps its cursor.
                self.results.restore(open);
                return self.fail_with_code(
                    stream,
                    REQUEST_INVALID_CODE,
                    &format!("result value {}", MdbPackError::DepthExceeded),
                );
            }
        }
        Ok(Flow::Continue)
    }
    /// The ADR-0110 failure idiom for an engine error, written once: the
    /// failed state, the FAILURE on the wire, and the session continues
    /// (RESET clears it).
    fn fail_engine<W: Write>(
        &mut self,
        stream: &mut W,
        e: &relate::Error,
    ) -> std::io::Result<Flow> {
        self.failed = true;
        send_engine_failure(stream, e)?;
        if let Some(outcome) = workspace_refusal(e) {
            self.note_query(outcome);
        }
        Ok(Flow::Continue)
    }

    /// The failed-state transition for a server-authored FAILURE (ADR-0110,
    /// ADR-0342): set the failed state, send `code`/`message`, continue.
    /// Every malformed-request exit routes through here.
    fn fail_with_code<W: Write>(
        &mut self,
        stream: &mut W,
        code: &str,
        message: &str,
    ) -> std::io::Result<Flow> {
        self.failed = true;
        send_failure_with_code(stream, code, message)?;
        Ok(Flow::Continue)
    }

    /// The 5.4 notifications-keys parse failure (ADR-0484 decision 8):
    /// the failed state with `Neo.ClientError.Request.Invalid` and the
    /// probed `metadata` wrapper — the one spelling of the exit the BEGIN
    /// and RUN notifications-parse failures both route through.
    fn fail_metadata_notifications<W: Write>(
        &mut self,
        stream: &mut W,
        parse_failure: &str,
    ) -> std::io::Result<Flow> {
        self.fail_with_code(
            stream,
            REQUEST_INVALID_CODE,
            &metadata_notifications_failure(parse_failure),
        )
    }

    /// The ADR-0314 breach exit: the FAILURE naming the message class and
    /// the session's state, then the close — the one spelling, shared by
    /// the breach gate, the rejected-HELLO state, the 5.x AUTHENTICATION
    /// gate and the LOGON breach. Unlike the fail idioms above it sets no
    /// failed state: the connection closes.
    fn send_breach<W: Write>(
        &self,
        stream: &mut W,
        message: Message,
        state: &str,
    ) -> std::io::Result<Flow> {
        send_failure_with_code(stream, REQUEST_INVALID_CODE, &breach_text(message, state))?;
        Ok(Flow::Close)
    }

    /// The answer to a well-formed message over the decoded-value budget
    /// (ADR-0577) in the open session: the message was framed whole, so
    /// the connection is in sync and the request fails like any other
    /// rejected one — a FAILURE naming the limit, the failed state, the
    /// session continues (RESET clears it). A message queued ahead of a
    /// RESET, or arriving while the session is already failed, answers
    /// IGNORED through the same [`Message`] classifications the handled
    /// messages use.
    fn over_budget_open<W: Write>(
        &mut self,
        stream: &mut W,
        over: &OverBudget,
        queued_ahead_of_reset: bool,
    ) -> std::io::Result<Flow> {
        if (queued_ahead_of_reset && over.interruptible())
            || (self.failed && over.ignored_while_failed())
        {
            send_ignored(stream)?;
            return Ok(Flow::Continue);
        }
        self.fail_with_code(stream, REQUEST_INVALID_CODE, &over.failure_text())
    }

    /// The over-budget answer where no session is open to fail (the 5.x
    /// AUTHENTICATION gate, ADR-0484 decision 4): the FAILURE, then the
    /// close, as for any other message that gate does not accept.
    fn over_budget_exit<W: Write>(
        &self,
        stream: &mut W,
        over: &OverBudget,
    ) -> std::io::Result<Flow> {
        send_failure_with_code(stream, REQUEST_INVALID_CODE, &over.failure_text())?;
        Ok(Flow::Close)
    }
}

/// The fatal decode exit (ADR-0315): a message that is not a request
/// this connection's version speaks — no FAILURE, the connection closes.
/// Every gate arm answers an undecodable message this way except the
/// pre-HELLO one, which answers it with the unauthorized FAILURE instead
/// so a skip-HELLO client is told why (ADR-0126).
fn undecodable_exit(undecodable: Undecodable) -> Flow {
    eprintln!("bolt: {undecodable}; closing connection");
    Flow::Close
}

/// The batch's last reachable RESET (0x0F), by index (ADR-0317): the last
/// RESET inside the prefix before the first fatal decode error (an
/// over-budget message is not fatal, ADR-0577). A message after the first
/// fatal one is never reached — the fatal decode (ADR-0315) closes the session
/// first — so a RESET there queues nothing ahead of it. The prefix bound is
/// found by decoding the prefix one message at a time and dropping each
/// before the next: the single-message peak again, never the batch
/// (SEC-serve-batch).
fn last_reset(batch: &[Vec<u8>]) -> Option<usize> {
    // The one spelling of "this payload is a RESET": two wire bytes, no
    // decode and no allocation, and the tag hoisted out of the per-payload
    // closure.
    let reset_tag = Message::Reset.signature();
    let is_reset = |payload: &Vec<u8>| struct_tag(payload) == Some(reset_tag);
    let prefix_end = batch
        .iter()
        .position(|payload| decode_message(payload).is_err_and(|e| e.is_fatal()))
        .unwrap_or(batch.len());
    batch[..prefix_end].iter().rposition(is_reset)
}

/// Apply the `db` of a RUN or BEGIN extra map as the session namespace
/// (ADR-0117 §7). Wire-pure: the caller renders the error with the same
/// engine-failure idiom as its sibling arms. In a transaction this is a
/// no-op — RUN keeps the BEGIN-chosen namespace.
fn apply_extra_db(
    conn: &mut Connection,
    fields: &[Pack],
    index: usize,
) -> Result<(), relate::Error> {
    if conn.in_transaction() {
        return Ok(());
    }
    conn.set_namespace(db_name(fields, index))
}

/// The RUN SUCCESS headline both staging branches produce (ADR-0309): the
/// planned column names, their ADR-0224 type names and the time until the
/// result was available.
struct RunHeadline {
    columns: Vec<String>,
    column_types: Vec<String>,
    t_first: Duration,
}

/// PULL streams the rows it takes; DISCARD drops them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Consume {
    Pull,
    Discard,
}

/// The 5.x session leg (ADR-0484): the HELLO that no longer authenticates
/// (decision 3), LOGON and LOGOFF (decision 4) and TELEMETRY (decision 5)
/// — the handlers behind the 5.4 arms of the dispatch above, split from
/// this file at the thousand-line rule. A child of this module, so the
/// handlers keep touching the session's private state.
#[path = "session_5x.rs"]
mod session_5x;

/// The PULL/DISCARD page-writing machinery (ADR-0309, ADR-0110): the
/// page outcome, the typed RECORD-write failure and the page writers
/// behind `Session::consume`, split from this file at the thousand-line
/// rule like the 5.x leg. A child of this module.
#[path = "session_pages.rs"]
mod session_pages;
