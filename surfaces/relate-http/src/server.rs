//! The listener loop and per-connection handler, shaped for later embedding
//! in the `relate serve --http PORT` CLI node (ADR-0232 Consequences):
//! the caller opens the [`Database`], binds the [`TcpListener`], resolves the
//! [`AuthConfig`] (via `relate_auth::resolve`: `--auth` flag >
//! `RELATE_AUTH` > generated token), and calls [`serve`].

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use relate::Database;
use relate_auth::AuthConfig;

use crate::content_encoding::{self, BodyEncoding, ReplyEncodingPolicy, VARY_ACCEPT_ENCODING};
use crate::http::{
    drain_reader, read_admitted_request, write_head, AdmissionError, AdmissionOutcome, Cors,
    DrainPlan, Framing, OriginAllowlist, ReadLimits, REJECT_DRAIN_TIMEOUT,
};
use crate::query_api;
use crate::reply::Reply;
use crate::stream::{self, CountedJson};
use relate_serve::{
    serve_capped, ConnectionCap, DEFAULT_BODY_DEADLINE, DEFAULT_CONNECTION_STACK_BYTES,
    DEFAULT_HEADER_DEADLINE, DEFAULT_MAX_CONNECTIONS, DEFAULT_READ_TIMEOUT,
};

pub use relate_serve::DEFAULT_MAX_BODY_BYTES;

/// Server configuration for the HTTP Query API surface.
pub struct HttpConfig {
    /// The resolved auth mode (ADR-0232 §1): `AuthConfig::None` lets every
    /// request through; `AuthConfig::Basic` requires the RFC 7617
    /// `Authorization: Basic` header on the query route.
    pub auth: AuthConfig,
    /// Largest accepted request body in bytes (default
    /// [`DEFAULT_MAX_BODY_BYTES`]).
    pub max_body_bytes: usize,
    /// Cap on simultaneously open connections (default
    /// [`DEFAULT_MAX_CONNECTIONS`], `0` = unlimited). An over-cap connection
    /// is accepted then immediately closed (ADR-0110's policy, ADR-0333).
    /// Ignored when [`Self::connection_cap`] is set.
    pub max_connections: usize,
    /// Process-wide cap shared with other surfaces. `None` builds a cap
    /// from [`Self::max_connections`].
    pub connection_cap: Option<Arc<relate_serve::ConnectionCap>>,
    /// Per-read timeout: a stalled reader is dropped instead of pinning its
    /// connection thread (default [`DEFAULT_READ_TIMEOUT`]).
    pub read_timeout: Duration,
    /// Absolute deadline for a complete request header block, measured from
    /// the start of the connection (default [`DEFAULT_HEADER_DEADLINE`],
    /// 10 s; ADR-0451). The per-read timeout is re-armed to the time
    /// remaining before every header read, so a peer dribbling bytes cannot
    /// hold a worker past the deadline. `Duration::ZERO` disables it.
    pub header_deadline: Duration,
    /// Absolute deadline for a complete request body, measured from the
    /// start of the body read (default [`DEFAULT_BODY_DEADLINE`], 60 s;
    /// ADR-0451). Re-armed per read like the header deadline, so a peer
    /// trickling a declared `Content-Length` body cannot pin a worker.
    /// `Duration::ZERO` disables it.
    pub body_deadline: Duration,
    /// The Bolt endpoint advertised as `bolt_direct` in the `GET /`
    /// discovery document. Populated by `relate serve`'s config
    /// construction (a named binding there today: `None` until `--bolt`
    /// exists, and that flag's change fills it); covered end-to-end by the
    /// `a_configured_bolt_endpoint_is_advertised_in_discovery` test, so the
    /// document never advertises an endpoint the server does not have.
    pub bolt_direct: Option<String>,
    /// The browser-Origin allowlist for the guard and the CORS echo
    /// (default [`OriginAllowlist::Default`]: loopback plus the hosted
    /// Neo4j Browser).
    pub allowed_origins: OriginAllowlist,
    /// The open explicit transactions of the `…/tx` routes (ADR-0237).
    /// Shared by every connection of this server.
    pub transactions: crate::tx_registry::TxRegistry,
    /// Paths that are not Query API routes (ADR-0428). `None` is the
    /// product server: unknown paths are 404.
    pub extension: Option<Arc<dyn HttpExtension>>,
}

/// A host route served on the same connection as the Query API (ADR-0428).
pub trait HttpExtension: Send + Sync {
    /// Paths that skip the credential check. A health probe is public.
    /// The server ignores this answer on a Query API route.
    fn is_public(&self, method: &str, path: &str) -> bool;
    /// `Some` when this extension owns the request.
    fn handle(&self, request: &crate::http::Request) -> Option<crate::Reply>;
    /// Last look at a reply before it is written. The default leaves it
    /// unchanged. A host uses this to remap a status it understands
    /// (MemoryDB rewrites a growable graph-capacity 507 to 429).
    fn map_reply(&self, reply: Reply) -> Reply {
        reply
    }
}

impl HttpConfig {
    /// Check the admission deadlines against the library bound
    /// (`relate_serve::MAX_ADMISSION_DEADLINE`, ADR-0451). The CLI
    /// resolver enforces the same bound; this is the guard for a directly
    /// constructed config entering the library.
    pub fn validate(&self) -> Result<(), String> {
        relate_serve::validate_admission_deadlines(self.header_deadline, self.body_deadline)
    }

    /// A config with the given auth mode and the default limits.
    pub fn new(auth: AuthConfig) -> Self {
        Self {
            auth,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            connection_cap: None,
            read_timeout: DEFAULT_READ_TIMEOUT,
            header_deadline: DEFAULT_HEADER_DEADLINE,
            body_deadline: DEFAULT_BODY_DEADLINE,
            bolt_direct: None,
            allowed_origins: OriginAllowlist::Default,
            transactions: crate::tx_registry::TxRegistry::new(),
            extension: None,
        }
    }
}

/// Accept loop: the shared [`serve_capped`] loop (thread-per-connection
/// under the configured cap, accept-then-close over cap, accept errors
/// retried — the ADR-0110 policy the Bolt surface sets, on sync primitives)
/// with [`serve_handler_validated`] as the per-connection handler (the
/// config is validated once here, not per connection). Returns the typed
/// error for an invalid config; on success it diverges by design — there is
/// no shutdown condition, the process owns the listener's lifetime.
pub fn serve(db: Arc<Database>, listener: TcpListener, config: HttpConfig) -> io::Result<()> {
    // A directly constructed config can bypass the CLI resolver; this entry
    // returns the typed error instead of parking it in
    // `Instant::now() + duration` per connection. The accept loop itself
    // still diverges on success — the process owns the listener's lifetime.
    let limits = validate_config(&config)?;
    let cap = config
        .connection_cap
        .clone()
        .unwrap_or_else(|| Arc::new(ConnectionCap::new(config.max_connections)));
    let config = Arc::new(config);
    serve_capped(
        listener,
        "http",
        DEFAULT_CONNECTION_STACK_BYTES,
        cap,
        move |stream| serve_handler_validated(&db, stream, &config, &limits),
    )
}

/// The one typed-error mapping from a config validation failure — and the
/// one place the admission limits are built: an entry validates and
/// constructs them together, and the connection path only carries them.
fn validate_config(config: &HttpConfig) -> io::Result<ReadLimits> {
    ReadLimits::new(
        config.header_deadline,
        config.body_deadline,
        config.max_body_bytes,
        config.read_timeout,
    )
    .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))
}

/// Write one reply: the destructure → measure → negotiate → extra-headers →
/// response-write sequence, once for every path out of [`serve_handler`].
/// The reply's content type is the negotiated one (ADR-0232 §3's envelope
/// shape never changes; Neo4j Browser's driver requires the media type it
/// asked for).
///
/// Framing is decided from the uncompressed serialized size before any head
/// byte goes out (ADR-0386 §2/§5): [`stream::count_json`] counts into a
/// buffer bounded at the stream threshold, and a body below it keeps the
/// one-shot `Content-Length` path — the compressed length when
/// [`content_encoding::encode_body`] selects gzip (ADR-0385) — while a body
/// at or above it goes out as `Transfer-Encoding: chunked` chunk frames.
/// Above the threshold gzip is selected by weight and size alone (the
/// compressed size cannot be known before the head,
/// [`BodyEncoding::for_streamed_body`]) and wraps the frame writer, so the
/// compressor's writes feed the same fixed-size frame buffer. A chunk
/// boundary is therefore a buffer flush — a transport detail clients must
/// not depend on — never a row boundary.
///
/// A negotiated reply always advertises `Vary: Accept-Encoding`, identity
/// included, so a shared cache keys both variants. The discovery document is
/// [`ReplyEncodingPolicy::Excluded`]: one-shot identity, no `Vary`, no
/// chunking.
fn write_reply(
    writer: &mut impl Write,
    reply: Reply,
    encoding: ReplyEncodingPolicy,
) -> io::Result<()> {
    let Reply {
        status,
        body,
        content_type,
        mut extra_headers,
    } = reply;
    // The policy match carries both seam effects: `Excluded` (the small
    // public discovery document) is identity bytes, no `Vary`, never
    // chunked (ADR-0385 §2, ADR-0386 §7); `Negotiated` reads the request's
    // `Accept-Encoding`, always advertises `Vary`, and frames at the
    // streaming threshold.
    let (accept_encoding, framed) = match encoding {
        ReplyEncodingPolicy::Excluded => (
            None,
            CountedJson::OneShot(serde_json::to_vec(&body).map_err(io::Error::other)?),
        ),
        ReplyEncodingPolicy::Negotiated(accept_encoding) => {
            extra_headers.push(("vary".to_string(), VARY_ACCEPT_ENCODING.to_string()));
            (accept_encoding, stream::count_json(&body)?)
        }
    };
    match framed {
        CountedJson::OneShot(payload) => {
            let encoded = content_encoding::encode_body(accept_encoding.as_deref(), payload)?;
            push_content_encoding(&mut extra_headers, encoded.encoding);
            crate::http::write_response(
                writer,
                status,
                content_type,
                &encoded.bytes,
                &extra_headers,
            )
        }
        CountedJson::Stream => {
            let body_encoding = BodyEncoding::for_streamed_body(accept_encoding.as_deref());
            push_content_encoding(&mut extra_headers, body_encoding);
            write_head(
                writer,
                status,
                content_type,
                Framing::Chunked,
                &extra_headers,
            )?;
            stream::write_chunked(writer, &body, body_encoding)
        }
    }
}

/// The `content-encoding` header for one decided representation — the one
/// place the header is emitted: identity omits it entirely (never
/// `content-encoding: identity`), gzip names the coding.
fn push_content_encoding(extra_headers: &mut Vec<(String, String)>, encoding: BodyEncoding) {
    if let Some(value) = encoding.header_value() {
        extra_headers.push(("content-encoding".to_string(), value.to_string()));
    }
}

/// Write the CORS preflight answer: the body-less 204 with the allow methods
/// and headers, plus the `access-control-allow-origin` echo for an
/// allowlisted `Origin`. The gate classifies a preflight before auth and
/// before any body byte, so this answer is unauthenticated and body-free by
/// construction — and outside the compression seam (no `Vary`, no
/// `content-encoding`).
fn write_preflight(writer: &mut impl io::Write, reply_origin: Option<String>) -> io::Result<()> {
    let mut extra: Vec<(String, String)> = vec![
        (
            "access-control-allow-methods".to_string(),
            "GET, POST, OPTIONS".to_string(),
        ),
        (
            // The browser-facing surface must admit the headers Browser-like
            // tooling sends: the tracing headers (`sentry-trace`, `baggage`)
            // beyond the API's own `Authorization`/`Content-Type` — a
            // preflight that rejects them fails the discovery probe before
            // any query runs (LABS-376).
            "access-control-allow-headers".to_string(),
            "Authorization, Content-Type, sentry-trace, baggage".to_string(),
        ),
    ];
    if let Some(origin) = reply_origin {
        extra.push(("access-control-allow-origin".to_string(), origin));
    }
    crate::http::write_response(writer, 204, "application/json", b"", &extra)
}

/// Handle one plaintext TCP connection. Sets the read timeout and arms the
/// absolute header/body deadlines per read, then runs the same request path
/// as [`serve_connection`]. A rejected body is
/// drained only after the reply is written, and only for
/// [`REJECT_DRAIN_TIMEOUT`], so a peer withholding the body cannot pin
/// the thread for the full read timeout. The socket is cloned for the
/// deadline armer: both handles name the same socket, so re-arming one is
/// visible to reads on the other.
pub fn serve_handler(db: &Database, stream: TcpStream, config: &HttpConfig) -> io::Result<()> {
    let limits = validate_config(config)?;
    serve_handler_validated(db, stream, config, &limits)
}

/// [`serve_handler`] without the entry check: the product accept loop
/// validates the config once before it starts, so its per-connection path
/// enters here. The socket-arming setup is shared, not duplicated.
fn serve_handler_validated(
    db: &Database,
    stream: TcpStream,
    config: &HttpConfig,
    limits: &ReadLimits,
) -> io::Result<()> {
    stream.set_read_timeout(Some(config.read_timeout))?;
    let arm = stream.try_clone()?;
    serve_stream(
        db,
        stream,
        config,
        limits,
        |stream| stream.set_read_timeout(Some(REJECT_DRAIN_TIMEOUT)),
        |remaining| arm.set_read_timeout(Some(remaining)),
    )
}

/// Handle one connection on `stream` (ADR-0428). The caller owns the
/// transport: a plaintext [`TcpStream`] from [`serve_handler`], or the
/// decrypted half of a TLS handshake. One request, then `Connection: close`.
///
/// This function does not set a read timeout. A host that passes a stream
/// with none must set one on the stream, or a peer that withholds a
/// rejected body pins the connection thread. [`serve_handler`] is the
/// product path and applies [`REJECT_DRAIN_TIMEOUT`] to that drain. The
/// absolute header deadline ([`HttpConfig::header_deadline`]) is still
/// enforced between reads here, but its per-read arming needs the socket's
/// timeout: a host that wants the deadline to hold *during* a blocking read
/// must set a read timeout no longer than the deadline (the same rule as
/// the drain).
pub fn serve_connection<S: Read + Write>(
    db: &Database,
    stream: S,
    config: &HttpConfig,
) -> io::Result<()> {
    let limits = validate_config(config)?;
    serve_stream(db, stream, config, &limits, |_| Ok(()), |_| Ok(()))
}

fn serve_stream<S: Read + Write>(
    db: &Database,
    stream: S,
    config: &HttpConfig,
    limits: &ReadLimits,
    arm_drain: impl FnOnce(&S) -> io::Result<()>,
    mut arm_read: impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<()> {
    let mut reader = io::BufReader::new(stream);
    let outcome = dispatch(db, &mut reader, config, limits, &mut arm_read)?;
    match outcome {
        Dispatch::Silent => Ok(()),
        Dispatch::Preflight { origin } => write_preflight(reader.get_mut(), origin),
        Dispatch::Reply {
            reply,
            encoding,
            drain,
        } => {
            let reply = match &config.extension {
                Some(extension) => extension.map_reply(reply),
                None => reply,
            };
            write_reply(reader.get_mut(), reply, encoding)?;
            if !matches!(drain, DrainPlan::None) {
                arm_drain(reader.get_ref())?;
                drain_reader(&mut reader, drain, config.max_body_bytes);
            }
            Ok(())
        }
    }
}

enum Dispatch {
    Silent,
    Preflight {
        origin: Option<String>,
    },
    Reply {
        reply: Reply,
        encoding: ReplyEncodingPolicy,
        drain: DrainPlan,
    },
}

fn dispatch<S: Read>(
    db: &Database,
    reader: &mut io::BufReader<S>,
    config: &HttpConfig,
    limits: &ReadLimits,
    arm_read: &mut impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<Dispatch> {
    let extension_public = |head: &crate::http::RequestHead| {
        crate::query_api::is_discovery_probe(head)
            || (!crate::query_api::is_query_route(&head.path)
                && config
                    .extension
                    .as_ref()
                    .is_some_and(|extension| extension.is_public(&head.method, &head.path)))
    };
    // The admission pipeline owns the gate, the malformed/deadline fold and
    // both absolute deadlines (ADR-0451); the surface only renders the typed
    // outcome. Trickled bytes cannot reset a fixed per-read timeout because
    // the reader re-arms to the remaining deadline before every read.
    let admitted = read_admitted_request(
        reader,
        &config.auth,
        &config.allowed_origins,
        Cors::Browser,
        extension_public,
        limits,
        &mut *arm_read,
    )
    .map_err(AdmissionError::into_io)?;
    // Policy: the CORS echo, defined once — an allowlisted `Origin` is
    // echoed on every response, rejections included, so the browser can read
    // its errors; a non-allowlisted `Origin` gets nothing (the browser then
    // blocks the response, which is correct for a 403). The compression
    // policy is per reply, decided where its head is: the map below.
    let echo_origin = |origin: Option<&str>| {
        origin
            .filter(|o| config.allowed_origins.allows(o))
            .map(str::to_string)
    };
    let reply_origin = admitted
        .head()
        .and_then(|head| echo_origin(head.header("origin")));
    // Map: every admission outcome becomes one Reply plus the post-write
    // drain and the response-encoding policy of the head it answers. A
    // pre-body rejection carries its parsed head, so the Origin echo and the
    // encoding negotiation read the real headers and the already-sent body is
    // drained after the answer (no RST clobbering the response).
    let (mut reply, drain, encoding): (Reply, DrainPlan, ReplyEncodingPolicy) = match admitted {
        AdmissionOutcome::Closed => return Ok(Dispatch::Silent),
        AdmissionOutcome::Preflight { .. } => {
            return Ok(Dispatch::Preflight {
                origin: reply_origin,
            });
        }
        // A malformed head never produced a request head to read
        // `Accept-Encoding` from: identity, but still on the seam.
        AdmissionOutcome::BadRequest { message, drain } => (
            crate::reply::bad_request(&message),
            drain,
            ReplyEncodingPolicy::for_unparsed_head(),
        ),
        AdmissionOutcome::Rejected { rejection, drain } => {
            let (head, public, wire) = rejection.into_parts();
            let reply = wire.into_reply();
            let encoding = ReplyEncodingPolicy::for_head(&head, public);
            (reply, drain, encoding)
        }
        AdmissionOutcome::Request { request, public } => {
            let encoding = ReplyEncodingPolicy::for_head(&request.head, public);
            (
                query_api::handle(
                    db,
                    &request,
                    &config.auth,
                    config.bolt_direct.as_deref(),
                    &config.transactions,
                    if public {
                        query_api::CredentialGate::Public
                    } else {
                        query_api::CredentialGate::Accepted
                    },
                    config.extension.as_deref(),
                ),
                DrainPlan::None,
                encoding,
            )
        }
    };
    // The caller writes this reply before draining. Unread request bytes
    // stay in the buffer until that drain, which runs before the socket
    // closes.
    if let Some(origin) = reply_origin {
        reply
            .extra_headers
            .push(("access-control-allow-origin".to_string(), origin));
    }
    Ok(Dispatch::Reply {
        reply,
        encoding,
        drain,
    })
}

// The reply-seam tests live in their own file (they would push this module
// past 1000 lines), reached through this module.
#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "embed_tests.rs"]
mod embed_tests;
