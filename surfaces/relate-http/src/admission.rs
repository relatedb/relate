//! The admission-read pipeline both HTTP surfaces run (ADR-0451): the
//! bounded request-head and body readers (the per-read timeout is re-armed
//! to the absolute deadline's remaining time), the shared pre-body gate and
//! its surface-facing outcome, and the bounded post-rejection drain policy.
//! Sibling to `origin` / `stream` / `content_encoding`; the wire types
//! themselves stay in [`crate::http`].

use std::io::{self, BufReader, Read};
use std::time::{Duration, Instant};

use crate::http::{HttpVersion, RequestHead, MAX_HEADERS, MAX_HEADER_BYTES};
use crate::origin::{browser_guard, OriginAllowlist};
use crate::reply::AuthFailure;
use relate_error::GQLSTATUS_GENERAL;

/// Why an admission read stopped before producing a request. The class is
/// carried in the type so a surface matches variants instead of sniffing
/// `io::ErrorKind` or a message string: malformed input is answered `400`,
/// an expired absolute deadline drops the connection with no reply, and a
/// transport failure drops it with the accept loop's log line.
#[derive(Debug)]
pub enum AdmissionError {
    /// The request head or body was malformed: answer `400` (the caller
    /// drains per its `DrainPlan`).
    Malformed(String),
    /// An absolute deadline expired: drop the connection, no reply.
    Deadline(&'static str),
    /// The transport failed (reset, EOF mid-message, …): drop the connection.
    Io(io::Error),
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(message) => f.write_str(message),
            Self::Deadline(message) => f.write_str(message),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for AdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Malformed(_) | Self::Deadline(_) => None,
        }
    }
}

impl From<io::Error> for AdmissionError {
    /// Transport errors are the only `io::Error` this seam carries; a
    /// protocol error is constructed as [`AdmissionError::Malformed`] at the
    /// point it is known.
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The declared `Content-Length` (0 when absent); a malformed value is a
/// typed malformed-input error, never a silent zero. This is an admission
/// policy, not a wire-type fact, so it lives beside its two consumers.
fn content_length(head: &RequestHead) -> Result<usize, AdmissionError> {
    match head.header("content-length") {
        None => Ok(0),
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| malformed("malformed content-length")),
    }
}

/// A protocol error, typed at construction.
fn malformed(message: &'static str) -> AdmissionError {
    AdmissionError::Malformed(message.to_string())
}

/// The surface's `400` text for a malformed request: the one place the
/// prefix is composed, for the head-phase and body-phase folds alike.
fn malformed_reply(message: &str) -> String {
    format!("malformed request: {message}")
}

impl AdmissionError {
    /// The fatal (drop-the-connection) form as the transport error a surface
    /// entry returns. `Malformed` is rendered as a `400` by
    /// [`read_admitted_request`], so this arm is only a total mapping.
    pub fn into_io(self) -> io::Error {
        match self {
            Self::Malformed(message) => io::Error::new(io::ErrorKind::InvalidData, message),
            Self::Deadline(message) => io::Error::new(io::ErrorKind::TimedOut, message),
            Self::Io(error) => error,
        }
    }
}

/// The limits one admission read runs under: the two absolute deadlines, the
/// body cap, and the ordinary per-read timeout restored between the header
/// and body phases. Grouping them keeps [`read_admitted_request`]'s argument
/// list honest and lets both surfaces fill it from their own config.
pub struct ReadLimits {
    header_deadline: Duration,
    body_deadline: Duration,
    max_body_bytes: usize,
    read_timeout: Duration,
}

impl ReadLimits {
    /// Build the limits, validating both deadlines against the shared
    /// library bound (the check every config entry also runs). The fields
    /// are private: a hand-built value cannot bypass validation and reach
    /// `deadline_instant`'s `Instant::now() + duration` unchecked.
    pub fn new(
        header_deadline: Duration,
        body_deadline: Duration,
        max_body_bytes: usize,
        read_timeout: Duration,
    ) -> Result<Self, String> {
        relate_serve::validate_admission_deadlines(header_deadline, body_deadline)?;
        Ok(Self {
            header_deadline,
            body_deadline,
            max_body_bytes,
            read_timeout,
        })
    }
}

/// A rejection's reply payload: the status, message and extra headers travel
/// with the parsed head and its public classification, so a surface renders
/// the answer without re-deriving any of it.
pub struct Rejection {
    /// The parsed head (the Origin echo and encoding policy read it).
    pub head: RequestHead,
    /// The surface's public-probe route matched.
    pub public: bool,
    /// The HTTP status (400 for the version floor, 401, 403, 415).
    pub status: u16,
    /// The GQLSTATUS code the error envelope carries.
    pub code: &'static str,
    /// The human-readable reason.
    pub message: String,
    /// Extra response headers (`WWW-Authenticate` on a 401).
    pub extra_headers: Vec<(String, String)>,
}

impl Rejection {
    /// A non-authentication rejection (version floor, browser guard,
    /// malformed body): the general GQLSTATUS code and no extra headers.
    fn general(head: RequestHead, public: bool, status: u16, message: String) -> Self {
        Self {
            head,
            public,
            status,
            code: GQLSTATUS_GENERAL,
            message,
            extra_headers: Vec::new(),
        }
    }

    /// An authentication rejection: the failure's status, code, message and
    /// `WWW-Authenticate` challenge, unpacked once by [`AuthFailure`].
    fn auth(head: RequestHead, public: bool, failure: AuthFailure) -> Self {
        Self {
            head,
            public,
            status: failure.failure.status,
            code: failure.code,
            message: failure.failure.message.to_string(),
            extra_headers: failure.failure.challenge.into_iter().collect(),
        }
    }
}

/// What [`read_admitted_request`] produced: a request ready for the surface's
/// router, a gate rejection to render, a malformed-head rejection, a CORS
/// preflight, or a clean EOF. The reply fields and the drain travel
/// together, so a surface renders the answer without re-deriving any policy.
pub enum AdmissionOutcome {
    /// A clean EOF before any bytes: write nothing.
    Closed,
    /// The head did not parse: answer `400` and drain whatever the peer sent.
    BadRequest {
        /// The error message (already prefixed as the surface's 400 text).
        message: String,
        /// What to drain after the reply.
        drain: DrainPlan,
    },
    /// A CORS preflight: answer `204` with the allow headers.
    Preflight {
        /// The parsed head (the Origin echo reads it).
        head: RequestHead,
    },
    /// A gate rejected the request before its body: render this reply, then
    /// drain `drain`.
    Rejected {
        /// The rejection's reply payload.
        rejection: Rejection,
        /// What to drain after the reply.
        drain: DrainPlan,
    },
    /// A complete request ready for the surface's router.
    Request {
        /// The head and body.
        request: crate::http::Request,
        /// The surface's public-probe route matched.
        public: bool,
    },
}

impl AdmissionOutcome {
    /// A pre-body rejection paired with the drain plan for its head: the one
    /// place the two are composed.
    fn rejected(rejection: Rejection, max_body_bytes: usize) -> Self {
        let drain = DrainPlan::for_head(&rejection.head, max_body_bytes);
        Self::Rejected { rejection, drain }
    }

    /// The parsed head the outcome answers, when one parsed (the Origin echo
    /// reads it).
    pub(crate) fn head(&self) -> Option<&RequestHead> {
        match self {
            Self::Preflight { head } => Some(head),
            Self::Rejected { rejection, .. } => Some(&rejection.head),
            Self::Request { request, .. } => Some(&request.head),
            Self::Closed | Self::BadRequest { .. } => None,
        }
    }
}

/// The one documented admission path for both HTTP surfaces: read the head
/// under the header deadline, restore the ordinary per-read timeout, run the
/// shared gate, and on a pass read the body under the body deadline. Every
/// outcome — malformed, deadline, transport failure, rejection, preflight,
/// EOF, or a complete request — is one typed [`AdmissionOutcome`] or
/// [`AdmissionError`], so the surface only renders.
///
/// `is_public` classifies the surface's unauthenticated routes (a surface
/// with none passes a predicate that always returns `false`). `arm` re-arms
/// the transport's per-read timeout to the remaining deadline time before
/// each read; a no-op closure is right for a host whose transport cannot be
/// re-armed (the elapsed checks still run between reads).
pub fn read_admitted_request(
    reader: &mut BufReader<impl Read>,
    auth: &relate_auth::AuthConfig,
    origins: &OriginAllowlist,
    cors: Cors,
    is_public: impl Fn(&RequestHead) -> bool,
    limits: &ReadLimits,
    mut arm: impl FnMut(Duration) -> io::Result<()>,
) -> Result<AdmissionOutcome, AdmissionError> {
    let mut budget = ReadBudget::new(
        deadline_instant(limits.header_deadline),
        &mut arm,
        HEADER_EXCEEDED,
    );
    let head = match read_request_head(reader, &mut budget) {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(AdmissionOutcome::Closed),
        Err(AdmissionError::Malformed(message)) => {
            return Ok(AdmissionOutcome::BadRequest {
                message: malformed_reply(&message),
                drain: DrainPlan::Unparsed,
            })
        }
        Err(error) => return Err(error),
    };
    // The head is complete: the body read gets the ordinary per-read timeout
    // back (a gate rejection re-arms its own short drain timeout later).
    arm(limits.read_timeout)?;
    gate_head(head, auth, origins, cors, is_public).into_admitted(reader, limits, &mut arm)
}

/// Phase 1 of the request read: the request line and headers, parsed by
/// `httparse` out of a `MAX_HEADER_BYTES`-bounded buffer (ADR-0234).
/// Returns `Ok(None)` on a clean EOF before any bytes (a client that
/// connected and went away). A malformed head is [`AdmissionError::Malformed`].
///
/// `budget` bounds the whole header phase (ADR-0451): before every
/// underlying read it arms the transport with the time remaining, so the
/// socket's per-read timeout can never exceed what is left.
fn read_request_head<F: FnMut(Duration) -> io::Result<()>>(
    reader: &mut BufReader<impl Read>,
    budget: &mut ReadBudget<'_, F>,
) -> Result<Option<RequestHead>, AdmissionError> {
    let mut buf = vec![0u8; MAX_HEADER_BYTES];
    let mut filled = 0usize;
    loop {
        if filled == buf.len() {
            return Err(malformed("headers too large"));
        }
        budget.arm_remaining()?;
        let n = budget.read_count(reader.read(&mut buf[filled..]))?;
        if n == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed mid-headers",
            )
            .into());
        }
        filled += n;
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(&buf[..filled]) {
            Ok(httparse::Status::Partial) => continue,
            Ok(httparse::Status::Complete(head_len)) => {
                let (Some(method), Some(path)) = (request.method, request.path) else {
                    return Err(malformed("malformed request line"));
                };
                let mut parsed = Vec::with_capacity(request.headers.len());
                for header in request.headers {
                    // httparse works on bytes; the wire contract keeps
                    // header values UTF-8 — a non-UTF-8 head is a 400.
                    let value = std::str::from_utf8(header.value)
                        .map_err(|_| malformed("headers are not UTF-8"))?;
                    parsed.push((header.name.to_ascii_lowercase(), value.trim().to_string()));
                }
                let version = match request.version {
                    Some(1) => HttpVersion::Http11,
                    Some(0) => HttpVersion::Http10,
                    // httparse parses only `HTTP/1.0` and `HTTP/1.1` request
                    // lines; any other token is its `Error::Version`, so no
                    // other value reaches here.
                    _ => return Err(malformed("unsupported HTTP version")),
                };
                return Ok(Some(RequestHead {
                    method: method.to_string(),
                    path: path.to_string(),
                    version,
                    headers: parsed,
                    // Bytes read past the head belong to the body.
                    leftover: buf[head_len..filled].to_vec(),
                }));
            }
            Err(_) => return Err(malformed("malformed request")),
        }
    }
}

/// Phase 2 of the request read: the body, per the head's `Content-Length`,
/// capped at `max_body_bytes` *before* allocation — and serving the bytes
/// the head read already buffered past the head/body boundary before
/// touching the reader. A malformed or over-cap body is
/// [`AdmissionError::Malformed`]; an expired deadline is
/// [`AdmissionError::Deadline`].
///
/// `deadline` bounds the whole body read (ADR-0451), the same discipline as
/// the header phase: before every underlying read the transport is armed
/// with the time remaining, so a peer trickling a declared `Content-Length`
/// body cannot pin the connection past the absolute deadline. `None` leaves
/// the transport's own timeout in charge.
fn read_request_body(
    reader: &mut BufReader<impl Read>,
    head: &mut RequestHead,
    max_body_bytes: usize,
    deadline: Option<Instant>,
    mut arm: impl FnMut(Duration) -> io::Result<()>,
) -> Result<Vec<u8>, AdmissionError> {
    let content_length = content_length(head)?;
    if content_length > max_body_bytes {
        return Err(malformed("body too large"));
    }
    let mut body = std::mem::take(&mut head.leftover);
    // Anything past Content-Length is pipelined junk; one request per
    // connection, so it is ignored.
    body.truncate(content_length);
    let mut rest = vec![0u8; content_length - body.len()];
    let mut budget = ReadBudget::new(deadline, &mut arm, BODY_EXCEEDED);
    let mut filled = 0usize;
    while filled < rest.len() {
        budget.arm_remaining()?;
        match budget.read_count(reader.read(&mut rest[filled..]))? {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed mid-body",
                )
                .into())
            }
            n => filled += n,
        }
    }
    body.append(&mut rest);
    Ok(body)
}

/// The gate decisions after a head has parsed — before any body byte is
/// read. The pre-body outcomes map to [`AdmissionOutcome`] through
/// [`GatedHead::into_admitted`], the one home of the gate-to-reply policy.
enum GatedHead {
    /// All gates passed — the body is read next.
    Pass {
        /// The parsed head.
        head: RequestHead,
        /// The surface's public-probe route matched (computed once by the
        /// gate — carried as data to the reply-encoding policy).
        public: bool,
    },
    /// A gate rejection: the [`Rejection`] payload rides whole — its status,
    /// message, extra headers and parsed head are the reply seam's inputs.
    Reject(Rejection),
    /// A CORS preflight (`OPTIONS`): the browser-facing surface answers
    /// 204 with the allow headers, unauthenticated and before any body.
    /// Only produced when the surface opted into CORS.
    Preflight {
        /// The parsed head — the same payload its siblings carry.
        head: RequestHead,
    },
}

impl GatedHead {
    /// Map the gate's decision and, for a pass, the bounded body read to the
    /// surface-facing outcome. This is the one place the gate's status,
    /// message, extra headers and drain plan are composed.
    fn into_admitted(
        self,
        reader: &mut BufReader<impl Read>,
        limits: &ReadLimits,
        arm: &mut impl FnMut(Duration) -> io::Result<()>,
    ) -> Result<AdmissionOutcome, AdmissionError> {
        match self {
            Self::Preflight { head } => Ok(AdmissionOutcome::Preflight { head }),
            Self::Reject(rejection) => {
                Ok(AdmissionOutcome::rejected(rejection, limits.max_body_bytes))
            }
            Self::Pass { mut head, public } => {
                let deadline = deadline_instant(limits.body_deadline);
                match read_request_body(
                    reader,
                    &mut head,
                    limits.max_body_bytes,
                    deadline,
                    &mut *arm,
                ) {
                    Ok(body) => Ok(AdmissionOutcome::Request {
                        request: head.into_request(body),
                        public,
                    }),
                    Err(AdmissionError::Malformed(message)) => {
                        let rejection =
                            Rejection::general(head, public, 400, malformed_reply(&message));
                        Ok(AdmissionOutcome::rejected(rejection, limits.max_body_bytes))
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }
}

/// The gate's instant form of a configured admission deadline (ADR-0451):
/// zero disables the deadline (`None`), a positive duration starts the clock
/// now. The one place a surface turns either deadline knob into the phase's
/// `Option<Instant>`, so both callers name the phase explicitly instead of
/// re-deriving the disabled case.
///
/// Panics only on an out-of-range value, which cannot arrive through a
/// public path: [`ReadLimits::new`] validates both deadlines, so its
/// private fields are the only way into this arithmetic.
fn deadline_instant(configured: Duration) -> Option<Instant> {
    if configured.is_zero() {
        return None;
    }
    Some(
        Instant::now()
            .checked_add(configured)
            .expect("admission deadlines are range-validated at resolution (ADR-0451)"),
    )
}

/// The re-arm-to-remaining discipline shared by the header and body phases
/// (ADR-0451): an optional absolute deadline, the callback that arms the
/// transport's per-read timeout, and the phase's exceeded message. The two
/// readers construct one budget and call its methods around every read; the
/// message and the deadline arithmetic have one home.
struct ReadBudget<'a, F> {
    deadline: Option<Instant>,
    arm: &'a mut F,
    exceeded: &'static str,
}

/// The header phase's deadline message.
const HEADER_EXCEEDED: &str = "request header deadline exceeded";
/// The body phase's deadline message.
const BODY_EXCEEDED: &str = "request body deadline exceeded";

impl<'a, F: FnMut(Duration) -> io::Result<()>> ReadBudget<'a, F> {
    fn new(deadline: Option<Instant>, arm: &'a mut F, exceeded: &'static str) -> Self {
        Self {
            deadline,
            arm,
            exceeded,
        }
    }

    /// Arm the transport with the time remaining before one read, or return
    /// the phase's typed timeout once the deadline has passed. `None` leaves
    /// the transport's own timeout in charge (the embedding path).
    fn arm_remaining(&mut self) -> Result<(), AdmissionError> {
        let Some(deadline) = self.deadline else {
            return Ok(());
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AdmissionError::Deadline(self.exceeded));
        }
        // `set_read_timeout` rejects a zero duration; sub-millisecond
        // remainders clamp to 1 ms, an overshoot far below any useful
        // measurement.
        (self.arm)(remaining.max(Duration::from_millis(1))).map_err(AdmissionError::from)
    }

    /// Translate a read that ended because its armed timeout elapsed into the
    /// phase's typed deadline error. Once [`Self::arm_remaining`] has set the
    /// socket timeout to the time remaining, a `WouldBlock`/`TimedOut` read
    /// means the absolute deadline passed; without a deadline, the
    /// transport's own error propagates unchanged.
    fn read_count(&mut self, result: io::Result<usize>) -> Result<usize, AdmissionError> {
        match result {
            Err(error)
                if self.deadline.is_some()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
            {
                Err(AdmissionError::Deadline(self.exceeded))
            }
            other => other.map_err(AdmissionError::from),
        }
    }
}

/// Which browser-CORS policy the gate applies, named at both call sites
/// (ADR-0451). The browser guard itself always runs; the policy decides
/// whether an `OPTIONS` preflight is answered unauthenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cors {
    /// The browser-facing Query API: answer CORS preflights.
    Browser,
    /// The agent-facing MCP transport: no browser surface, no preflight.
    Agent,
}

/// The gate decisions after a head has parsed — the order
/// [`read_admitted_request`] runs.
fn gate_head(
    head: RequestHead,
    auth: &relate_auth::AuthConfig,
    origins: &OriginAllowlist,
    cors: Cors,
    is_public: impl Fn(&RequestHead) -> bool,
) -> GatedHead {
    // The HTTP surfaces answer HTTP/1.1 only. A 1.0 request is a parsed 400
    // rejection — one-shot identity, never `Transfer-Encoding` — before any
    // gate or body read, so RFC 9112 §7.1's chunked framing, an HTTP/1.1-only
    // coding, cannot reach a peer that does not speak it. The parsed head
    // rides on the rejection like every other pre-body one, so the reply
    // seam reads the real `Origin` and `Accept-Encoding` and the caller
    // drains the declared body instead of guessing at an unparsed peer.
    // The public-probe classification is computed once, here, and carried
    // on every Pass/Reject as data.
    let public = is_public(&head);
    if head.version == HttpVersion::Http10 {
        return GatedHead::Reject(Rejection::general(
            head,
            public,
            400,
            "unsupported HTTP version HTTP/1.0: this surface answers HTTP/1.1".to_string(),
        ));
    }
    // A CORS preflight is answered unauthenticated and body-free; only
    // the browser-facing policy opts in.
    if cors == Cors::Browser && head.method == "OPTIONS" {
        return GatedHead::Preflight { head };
    }
    if let Err(rejection) = browser_guard(&head, origins) {
        return GatedHead::Reject(Rejection::general(
            head,
            public,
            rejection.status,
            rejection.message.to_string(),
        ));
    }
    // Every route the predicate did not mark public answers the challenge.
    // The MCP transport passes a predicate that always returns `false`.
    if !public {
        if let Err(err) = auth.check_authorization(head.header("authorization")) {
            if err == relate_auth::AuthError::Rejected {
                // The ADR-0139 delay bounds online guessing to one attempt per
                // connection per second.
                std::thread::sleep(FAILED_AUTH_DELAY);
            }
            return GatedHead::Reject(Rejection::auth(head, public, AuthFailure::new(auth, err)));
        }
    }
    GatedHead::Pass { head, public }
}

/// The delay before a `401` answers a failed credential check (ADR-0139's
/// failed-HELLO policy on the HTTP surfaces): bounds online guessing of a
/// preset credential to one attempt per connection per second. Only failed
/// auth is delayed; other rejections answer at once.
pub const FAILED_AUTH_DELAY: Duration = Duration::from_secs(1);

/// The grace period for draining an already-sent body after an early
/// rejection: long enough for bytes in flight, short enough that a peer
/// withholding the body cannot pin the connection thread.
pub const REJECT_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// What to drain after the rejection response has been written, so closing
/// the socket does not RST it away.
#[derive(Debug)]
pub enum DrainPlan {
    /// The response went out after the body was consumed (or there never
    /// was one): nothing to drain.
    None,
    /// Drain this many declared body bytes (already capped at the body
    /// limit when the plan was built).
    Declared(usize),
    /// The head never parsed: no Content-Length is known, so drain whatever
    /// the peer already sent, bounded by a fixed budget.
    Unparsed,
}

impl DrainPlan {
    /// The declared body's remaining bytes for `head`, capped at the body
    /// limit; `None` when there is nothing to drain. This is the one place
    /// a parsed head becomes a drain count, so no caller re-derives the
    /// `Content-Length`-minus-leftover arithmetic.
    pub fn for_head(head: &RequestHead, max_body_bytes: usize) -> Self {
        match content_length(head) {
            Ok(n) => {
                let remaining = n.saturating_sub(head.leftover.len()).min(max_body_bytes);
                if remaining == 0 {
                    Self::None
                } else {
                    Self::Declared(remaining)
                }
            }
            // The length is unknown: the drain policy (not this constructor)
            // owns what an unparsed body is worth.
            Err(_) => Self::Unparsed,
        }
    }
}

/// Discard the unread request body described by `plan`. This entry is
/// transport-agnostic and does not change the read timeout: the transport
/// that owns the socket arms its short drain timeout first (the HTTP
/// surface's drain closure, the MCP epilogue), or the stream has none. The
/// drain's reads stop on EOF, a read error, or the absolute budget; none of
/// those is an error to the caller.
pub fn drain_reader(reader: &mut BufReader<impl Read>, plan: DrainPlan, max_body_bytes: usize) {
    let remaining = drain_remaining(&plan, max_body_bytes);
    discard_bytes(reader, remaining, Instant::now() + REJECT_DRAIN_TIMEOUT);
}

/// Bytes drained when the body length was never parsed. Both the malformed
/// `Content-Length` arm and an unparsed head use this budget, capped by the
/// configured body limit.
const UNPARSED_DRAIN_BUDGET: usize = MAX_HEADER_BYTES * 2;

fn drain_remaining(plan: &DrainPlan, max_body_bytes: usize) -> usize {
    match plan {
        DrainPlan::None => 0,
        DrainPlan::Declared(remaining) => (*remaining).min(max_body_bytes),
        DrainPlan::Unparsed => UNPARSED_DRAIN_BUDGET.min(max_body_bytes),
    }
}

/// Discard up to `remaining` bytes; stop at EOF, the first read error, or
/// the total drain budget. The budget is absolute, not per read: a peer that
/// keeps trickling a byte inside every read window must not extend the
/// drain indefinitely (the same slowloris shape the admission deadlines
/// close for the request phases).
fn discard_bytes(reader: &mut BufReader<impl Read>, mut remaining: usize, deadline: Instant) {
    let mut scratch = [0u8; 8192];
    while remaining > 0 {
        if Instant::now() >= deadline {
            return;
        }
        let take = remaining.min(scratch.len());
        match reader.read(&mut scratch[..take]) {
            Ok(0) | Err(_) => return,
            Ok(n) => remaining -= n,
        }
    }
}
