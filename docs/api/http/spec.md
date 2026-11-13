# HTTP Query API v2 — spec

The HTTP surface is the Neo4j Query API v2 **statement endpoint** subset of
ADR-0232 §3, implemented by `surfaces/relate-http` (package
`relate-http`). The transport is HTTP/1.1 and deliberately minimal
(request-head parsing via `httparse`, ADR-0234): one request per connection,
request bodies are `Content-Length`-delimited only, responses carry
`connection: close` and are framed under `Content-Length` or, for large
bodies, `Transfer-Encoding: chunked` (ADR-0386, see
[Response framing](#response-framing-adr-0386)). Only HTTP/1.1 is answered: a
request whose request line declares any other version (HTTP/1.0, say) is
refused with a **400** before any gate or body read — the HTTP/1.0 refusal is
a parsed rejection, not an unparsable head — and is never answered with
chunked framing (ADR-0386 §8).

## Entry points (Rust)

```rust
use std::net::TcpListener;
use std::sync::Arc;
use relate::Database;
use relate_auth::AuthConfig;
use relate_http::{serve, serve_handler, HttpConfig};

let db = Arc::new(Database::open(":memory:")?);
let listener = TcpListener::bind("127.0.0.1:7474")?;
let config = HttpConfig::new(AuthConfig::basic("neo4j", "s3cret"));
serve(db, listener, config)?;           // blocking accept loop (diverges)
// or, per accepted stream in an embedding host:
// serve_handler(&db, stream, &config)?;
// A host that already terminated TLS calls the same protocol on the
// decrypted stream (ADR-0428):
// relate_http::serve_connection(&db, tls_stream, &config)?;
```

- `HttpConfig { auth: AuthConfig, max_body_bytes: usize, max_connections:
  usize, connection_cap: Option<Arc<ConnectionCap>>, read_timeout: Duration,
  header_deadline: Duration, body_deadline: Duration }` —
  `HttpConfig::new(auth)` uses the
  default 16 MiB body cap (`relate_serve::DEFAULT_MAX_BODY_BYTES`,
  re-exported from `server`), a
  **128-connection cap** (`relate_serve::DEFAULT_MAX_CONNECTIONS`; `0`
  is the explicit unlimited spelling, ADR-0451), the 30 s per-read timeout
  (`DEFAULT_READ_TIMEOUT`), a **10 s absolute header deadline**
  (`relate_serve::DEFAULT_HEADER_DEADLINE`; `Duration::ZERO` disables
  it, ADR-0451), and a **60 s absolute body deadline**
  (`relate_serve::DEFAULT_BODY_DEADLINE`; `Duration::ZERO` disables it,
  ADR-0451). Both deadlines are bounded at one day; the resolvers reject a
  larger configured value at startup.
  The `auth` value comes from
  `relate_auth::resolve` (resolution order: `--auth user:password` flag,
  then `RELATE_AUTH`, else a generated token; `none` disables auth —
  ADR-0232 §1).
- `serve(db: Arc<Database>, listener: TcpListener, config: HttpConfig) ->
  io::Result<()>` — the accept loop (`relate_http::http::serve_capped`),
  one thread
  per connection, up to the connection cap (over-cap: accept-then-close).
  Accept errors are logged and retried, never fatal (ADR-0110's policy on
  sync primitives); the loop diverges — there is no shutdown condition, the
  process owns the listener's lifetime. `relate serve` passes one
  process-wide `ConnectionCap` shared by the Bolt, HTTP, and MCP listeners
  (ADR-0333); the default is 128 sessions in total, not per listener. An
  out-of-range deadline in a directly constructed config is returned as
  `io::ErrorKind::InvalidInput` before the loop starts; on success the
  accept loop diverges (the process owns the listener's lifetime).
- `serve_handler(db: &Database, stream: TcpStream, config: &HttpConfig) ->
  io::Result<()>` — one plaintext connection. Validates the config on
  entry (an out-of-range deadline is `InvalidInput`), sets the per-read
  timeout, arms the absolute header deadline before every header read and the
  absolute body deadline before every body read (the socket's timeout
  becomes the time remaining, so a peer dribbling one byte per window
  cannot hold the worker), writes the reply, then drains a rejected body
  for `REJECT_DRAIN_TIMEOUT` (500 ms).
- `serve_connection(db, stream, config)` — one request on any blocking
  `Read + Write` (ADR-0428). Validates the config on entry like
  `serve_handler`. The host owns TLS and the read timeout: this
  function sets neither. The absolute deadlines are still checked between
  reads, but they cannot interrupt a blocking read on a transport this
  crate cannot re-arm; a host that wants one to hold *during* a read must
  set a read timeout no longer than that deadline.
  `HttpConfig.extension` serves paths that are not query routes;
  `is_public` names those paths that skip the credential check. A query
  route stays authenticated even when `is_public` matches it.
- Engine `Capacity` (the graph cap, the per-allocation 256 MiB expression
  cap, ADR-0463, or the per-record 64 MiB WAL payload cap, ADR-0494) is
  **507**. A `concurrent-logical-workspace-limit` refusal,
  and any retryable `ResourceLimit`, is **429**. A receipt adds
  `metrics.peakLogicalBytes` and `metrics.retryable`
  (ADR-0440) on the error object — the one way to tell a retryable
  logical-workspace refusal from a non-retryable one on the wire, since both
  share the same 429 status, code, and message text.
- `AuthConfig::Custom` checks a bearer token or Basic password through
  the host's `Authenticator` and binds each transaction to the owner
  derived from that credential. A header is an owner only when a trusted
  proxy stripped the client-supplied value and set it itself. `None` and
  `Basic` leave transactions unscoped: they have one principal, or none. A malformed request (bad request line, over-cap
  `Content-Length`) and a
  request line that is not HTTP/1.1 are answered **400** with an error
  envelope before the connection closes. A read stalled past
  `read_timeout`, a header block unfinished at `header_deadline`, and a
  body unfinished at `body_deadline` all drop the connection instead of
  pinning its thread (a body deadline timeout closes with no reply). The
  version refusal is a parsed rejection (the head parsed, its version is
  wrong), so it still drains the declared body, echoes an allowlisted
  `Origin` and runs the query reply's encoding policy; the `GET /`
  discovery probe is the one exception, remaining identity with no `Vary`
  (ADR-0386 §8).

### Connection admission

Three independent limits bound unauthenticated work before a request is
served (ADR-0451):

- **Connection cap** — `max_connections` (or the shared
  `connection_cap`). One thread and one socket per admitted connection;
  over-cap connections are accepted then immediately closed (ADR-0110's
  policy). The default is 128, half the engine's process-wide
  snapshot-pin table — the same ratio ADR-0435 uses for managed
  transactions, leaving the other half for auto-commit and every other
  pin consumer. Measured loopback capacity serves 256 concurrent
  requests in ~20 ms, twice the cap. `0` restores the old unlimited
  behavior and warns on a non-loopback bind (ADR-0333).
- **Absolute header deadline** — `header_deadline`, measured from the
  start of the connection handler. A connection that has not sent a
  complete header block by the deadline is dropped with no reply. Every
  header read re-arms the socket timeout to the time remaining, so the
  fixed per-read `read_timeout` cannot be defeated by trickling bytes.
  The default is 10 s; a normal loopback request completes in well under
  a millisecond, so the default leaves more than five orders of magnitude
  of headroom for slow links while a 10 Hz trickle is dropped at the
  deadline.
- **Absolute body deadline** — `body_deadline`, measured from the start of
  the body read and re-armed per read like the header phase. It applies to
  every body (public routes and the effectively unauthenticated no-auth
  server are the attack surface; an authenticated upload gets the same
  generous bound), so a declared `Content-Length` trickled one byte at a
  time cannot pin a worker. The default is 60 s: the 16 MiB body cap
  uploads over loopback in 7-11 ms (measured), while 60 s still admits a
  full-cap upload at roughly 2.2 Mbit/s.

The knobs resolve flag → environment → default: `--max-connections` /
`RELATE_MAX_CONNECTIONS`, `--header-deadline-secs` /
`RELATE_HEADER_DEADLINE_SECS`, `--body-deadline-secs` /
`RELATE_BODY_DEADLINE_SECS`. A malformed value or a deadline above
86400 seconds (one day) is a loud startup error; a blank environment value
counts as omitted. `0` disables each control. `HttpConfig::validate()`
applies the same one-day bound to a directly constructed config, and the
serve entry points run it before accepting connections.

## Authentication

Requests to the query route must carry an RFC 7617
`Authorization: Basic base64(user:password)` header when `AuthConfig` is
`Basic`; `AuthConfig::None` skips the check. `AuthConfig::Custom` accepts
`Authorization: Bearer <token>` or a Basic password and asks the host
authenticator; a failed custom check challenges with `Bearer`. A missing or wrong credential is
rejected with **401** and the error envelope (code `42NFF`), plus a
`WWW-Authenticate: Basic realm="relate"` challenge (the shared
`relate_auth::REALM`, one realm for every HTTP surface). The check runs
on the parsed headers **before the body is buffered** (an unauthenticated
request never gets its body read or allocated), and a failed check waits
`relate_http::http::FAILED_AUTH_DELAY` (1 s, ADR-0139's failed-HELLO
policy) before the 401 goes out, bounding online guessing to one attempt
per connection per second. The route handler authenticates **before
routing** — the same gate order as the MCP surface — so an unauthenticated
request gets 401 even for an unknown path or wrong method (no route-shape
leak). The one exception is `GET /`: the discovery document is public, as
in Neo4j.

## Browser-request guards (CSRF / DNS rebinding)

Both HTTP surfaces apply the same guard to the parsed headers, before auth
and before the body read, via the shared pre-body pipeline
`relate_http::http::read_admitted_request` (the one request-reading entry
point; `browser_guard` is the gate inside it). The guard applies to **every
state-changing method** — every method outside RFC 9110 §9.2.1's safe set
(`GET`, `HEAD`, `OPTIONS`, `TRACE`) — so `POST` (the statement and
transaction routes) and `DELETE` (the rollback route, ADR-0237) are guarded
alike (ADR-0455). Safe methods are exempt: a `GET` carries no body and is
CSRF-safe, and the public `GET /` discovery document must stay reachable
from browser tooling on loopback. On a state-changing method:

- A present `Origin` header must be allowlisted: by default loopback —
  `http(s)://localhost[:port]`, `http(s)://127.0.0.1[:port]`,
  `http(s)://[::1][:port]`, ADR-0266's loopback set, mirrored from the Bolt
  WebSocket policy — plus the hosted Neo4j Browser and Console origins on
  this surface (see the next section); the MCP transport pins loopback
  only. A missing Origin means a non-browser client and is allowed;
  anything else is **403**.
- A `POST` must not carry a browser-"simple" content type
  (`text/plain`, the form types, or none at all) — the types a
  cross-origin form can send without a preflight, which is the CSRF
  property this guard owns (ADR-0425). Every non-simple type passes to
  the statement route, which owns the media-type **vocabulary**: the
  accepted request types and their dialects are `application/json`
  (plain parameters) and `application/vnd.neo4j.query.v1.2` (typed
  envelopes); anything else — the unversioned
  `application/vnd.neo4j.query`, which no real server accepts, included —
  is a route-level **400** naming the two accepted spellings. The
  content-type rule is **POST-specific** (ADR-0425, ADR-0455): a
  cross-origin form can only submit as `POST`, and the body-less `DELETE`
  rollback carries no body.

A DNS-rebinding request — the browser addressing an attacker hostname that
resolves to the loopback server, so `Host` names the attacker while the page
`Origin` is the attacker origin — is refused by the `Origin` verdict above
(**403**) under every policy that does not explicitly allowlist that origin
(the wildcard's documented risk is the exception). Host-header validation
for unauthenticated deployments is deliberately not applied (ADR-0455): a
loopback-only `Host` rule would refuse the sanctioned `--host`/proxy
deployments that ADR-0248 warns about, while the rebinding shape is already
closed by the Origin verdict.

Guard rejections carry the error envelope with code `42000`.

## CORS and the Origin allowlist

The HTTP surface is browser-facing; the MCP transport is agent-facing and
keeps the fixed loopback-only guard with no CORS (see the MCP spec).

The allowlist is `HttpConfig::allowed_origins` (`--origin` on `relate
serve`, repeatable):

- **Default** (no `--origin`): the loopback set (`http(s)://localhost`,
  `127.0.0.1`, `[::1]`, any port) **plus `https://browser.neo4j.io` and
  `https://console.neo4j.io`**, so
  the hosted Neo4j Browser works out of the box.
- An explicit `--origin URL` list **replaces** the default — loopback
  origins are no longer implied.
- `--origin *` allows every Origin. Risk: with `--auth none`, any website
  the operator visits can drive the loopback server; prefer named origins.

The flag value's validity grammar is the shared origin grammar
(`relate_serve::origin::OriginFlagValue::parse`, reached through
`OriginAllowlist::parse_flag_value`): one value is an exact origin URL
(`http://…` / `https://…`) or `*`, and the parse returns the typed value
(`OriginFlagValue`) that `OriginAllowlist::from_flag_values` builds the
policy from — the wildcard spelling is stated once, in that parse, and
bolt's `--ws-allow-origin` parses through the same value. A URL
value is stored in its canonical serialization — the scheme and host
lowercased (both are case-insensitive: RFC 3986 §3.1, §3.2.2; IPv6
brackets kept) — so the exact matcher byte-compares one spelling. The
CLI's `serve --origin` parse delegates to it, so a value outside the
grammar is a usage error before the server starts.

CORS behavior:

- Every response (query results, discovery, and error rejections alike —
  the browser must be able to read errors) carries
  `Access-Control-Allow-Origin: <origin>` echoing the request's `Origin`
  when that origin is allowlisted, and no such header otherwise (a 403 for
  a non-allowlisted origin stays unreadable by the browser).
- `OPTIONS` on any route is a preflight: **204**, unauthenticated and
  body-free, with `Access-Control-Allow-Methods: GET, POST, OPTIONS` and
  `Access-Control-Allow-Headers: Authorization, Content-Type, sentry-trace, baggage`, plus the
  ACAO echo when the origin is allowlisted.

The HTTP/1.1 wire layer is the `http` module of this crate, shared with the
MCP streamable-HTTP transport — one minimal reader/writer (head parsing via
`httparse`, ADR-0234) for both HTTP surfaces.

## Route

| Method | Path | Behavior |
| --- | --- | --- |
| `GET` | `/` | The discovery document (public — no auth, as in Neo4j; tooling probes it before credentials): `{"query": <query endpoint URL>, "transaction": <tx endpoint URL>, "neo4j_version": <crate version>, "neo4j_edition": "community", "auth_config": {"oidc_providers": []}}` in that serialized order (the fixed empty OIDC list keeps Browser from probing SSO discovery — there is no SSO here), plus `bolt_direct` last only when this server actually has a Bolt endpoint (`HttpConfig::bolt_direct`; `serve` populates it from the **bound** `--bolt` port — for `--bolt 0`, the OS-assigned ephemeral port — and omits the field when no bolt surface serves). `query` is absolute via the `Host` header when present, root-relative (`/db/{databaseName}/query/v2`) otherwise; `transaction` is the same URL with `/tx` appended to the v2 route. The document is outside the compression and framing seams: one-shot identity, never gzipped or chunked and carrying no `Vary`, whatever its size (ADR-0385 §2, ADR-0386 §7). |
| `POST` | `/db/{databaseName}/query/v2` | Run one auto-commit statement |
| `POST` | `/db/{databaseName}/query/v2/tx` | Begin an explicit transaction (ADR-0237): opens the engine's `Connection::begin`, runs the body's optional statement, answers **201** with the envelope plus `transaction: {id, expires}` (`expires` is omitted when idle expiry is disabled, ADR-0435) |
| `POST` | `/db/{databaseName}/query/v2/tx/{id}` | Run a statement in the open transaction (200 + `transaction` block); an empty body is the keep-alive probe |
| `POST` | `/db/{databaseName}/query/v2/tx/{id}/commit` | Run the optional final statement, commit, close (200, no `transaction` block) |
| `DELETE` | `/db/{databaseName}/query/v2/tx/{id}` | Roll back, close (**202**) |
| any | unknown tx id on the tx routes | **404** error envelope (an id the operation's lease took and found idle past the deadline answers `42N90`; an unknown, already-swept, or other-owner id `42000`, ADR-0435) |
| any other method | a known route | **405** error envelope |
| any | any other path | **404** error envelope |

## Managed transactions (ADR-0237, ADR-0435)

Tx ids are process-local (`tx-N`, never reused) and live in a per-server
registry shared by all connections (`HttpConfig::transactions`). The
registry's mutex guards its bookkeeping only — it is never held across an
engine statement: every route's statement runs on a leased entry, out of
the registry with the lock released (the engine is single-writer, so
statements serialize at its commit loop). The lease has two exits: a run
returns its entry to the registry, while a commit or rollback takes its
entry out one-way and closes the transaction. A slow statement in one
transaction therefore never stalls another transaction's routes. While a
statement is in flight, the same id names no registry entry, so a
concurrent run, commit, or rollback on it answers the generic
**404/`42000`** not-found — the transaction is not closed under a running
statement, a run's entry is back once its statement completes, and a
commit's or rollback's entry is gone for good.
Transactions are the engine's real explicit transactions (ADR-0028) —
commit persists, rollback discards. There is no cross-server affinity
(`tx_host` is omitted).
`accessMode: "READ"` is honored on tx routes: write and schema
statements are rejected (classified through `EXPLAIN`) and the
transaction stays open for the client to close; it is still ignored on
the auto-commit route (a documented charter exclusion). A failing begin
statement rolls the fresh transaction back instead of leaking it.

### Idle expiry and the registry cap (ADR-0435)

Two enforced limits bound an open transaction: an idle timeout and a cap
on open registry entries. Both protect by default and both are
configurable on `serve` (the [CLI spec](../cli/spec.md) lists the flags):

| Limit | Flag | Environment | Default | `0` means |
| --- | --- | --- | --- | --- |
| Idle timeout | `--tx-idle-timeout-secs N` | `RELATE_TX_IDLE_TIMEOUT_SECS` | `60` s | expiry disabled |
| Open-transaction cap | `--max-open-transactions N` | `RELATE_MAX_OPEN_TRANSACTIONS` | `128` | unlimited |

Resolution is flag → environment → default; a blank environment value
counts as omitted and a malformed value is a loud startup error (never a
silent default). So is an idle timeout too large for the reply's
`chrono` deadline rendering: `TxLimits::new` refuses it, so the check
covers `serve` startup (naming both the flag and the environment
variable) and an embedder constructing limits for
`TxRegistry::with_limits` alike, rather than failing a request later.
With expiry enabled `expires` is therefore always rendered; it is
omitted only when expiry is disabled.

Idle expiry is enforced lazily, not by a background thread: every
registry operation reclaims expired entries under the registry mutex in
the same critical section it acts in. Begin sweeps; run, commit, and
rollback (the empty-body keep-alive probe included) take their target
out of the map first and then sweep the rest, so the operation that
found its own target expired answers the typed expiry. Begin and run —
including the empty-body keep-alive
probe — are the operations that keep the transaction registered: they
record `last_activity` on the monotonic clock and refresh it **after**
their statement completes, even when a run statement failed (the
transaction survives for the client to close, so the client is alive).
Commit and rollback close the transaction — the entry leaves the
registry — so they have no deadline left to refresh. An expired entry's
`Connection` is dropped, which rolls the transaction back and releases
its engine snapshot pin.

`transaction.expires` is the honest wall-clock rendering (RFC 3339 with
milliseconds, UTC) of `last_activity + idle timeout`, recomputed on
every reply that carries a `transaction` block. Enforcement uses the
monotonic clock, so a wall-clock step moves only the rendering. With
expiry disabled the member is omitted and the block is `{"id": …}`.

The cap counts open transactions, not statements: a transaction whose
statement is in flight — a run, or a commit or rollback running its
final statement — has its entry leased out of the registry but still
counts, because it still holds its snapshot pin, so concurrent long
statements cannot push a registry past its cap. A begin reserves its slot
before the engine is asked for a pin and holds it until its entry
registers, so a burst of concurrent begins cannot exceed the cap either.
Every in-flight state is that one reservation: a leased statement
(whatever its exit) and an admitted-but-unregistered begin alike.
The cap is checked after the sweep and **before** `Connection::begin`, so
an over-cap begin never takes a snapshot pin: it answers **429** with
code `42N91`. The cap is per registry — an embedder running several
HTTP servers divides the process's 256 pin slots between them. A transaction
an operation's lease takes and finds idle past the deadline answers
**404** with code `42N90`, naming the id and the idle timeout; an id that
is unknown, was already reclaimed by an earlier operation's sweep, or
belongs to another owner
keeps the generic 404/`42000` not-found (ADR-0428 — expiry never
discloses that the id existed).

```json
{"errors": [{"code": "42N91", "message": "transaction registry full: 128 open transactions (close one or raise --max-open-transactions)"}]}
{"errors": [{"code": "42N90", "message": "transaction tx-7 expired after 60s idle"}]}
```

A `serve` with both protections off (`--tx-idle-timeout-secs 0` and
`--max-open-transactions 0`) warns loudly at startup on a non-loopback
bind. The sweep runs only on transaction-route operations: a process
whose transaction traffic stops keeps its expired entries (bounded by
the cap) until the next such operation or shutdown.

## Namespaces and expected client noise

`{databaseName}` selects one of the fixed namespaces `neo4j`, `system`,
`system_ontologies` (ADR-0116). `system_ontologies` is visible only under
`RELATE_PREVIEW=ontology` (ADR-0593): without the opt-in selecting it fails
the same way. An unknown name fails with **404** and code
`42N00` in the error envelope.

Expected client noise: Neo4j Browser probes `https://<host>:<port>/` before
`http://` (its discovery prefers TLS, falling back to plain HTTP) — against
this plaintext-only server that is a failed TLS handshake in its logs, not
a server error. No server-side flag silences it.

## Request body

```json
{
  "statement": "MATCH (c:City {name: $name}) RETURN c",
  "parameters": {"name": "Oslo"},
  "includeCounters": false
}
```

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `statement` | string | required | One Cypher statement |
| `parameters` | object | none | `$name` bindings. The request's **Content-Type selects the parameter dialect** (ADR-0425): under `application/vnd.neo4j.query.v1.2` values may be plain JSON **or** the typed form `{"$type": …, "_value": …}` the official clients send (decoded per ADR-0236's encoding, recursively; an object is typed only when its keys are exactly `$type` and `_value` — a user map with a `$type` key among others stays a map); under plain `application/json` values are plain JSON — an envelope-shaped object stays a user **map**, byte-for-byte what a real server delivers. Temporal payloads decode to their ISO strings (build typed values in Cypher, e.g. `date($p)`), a Point to its `{coordinates, crs}` map, a Vector to its number list; graph entities and unknown `$type`s are a 400. Any other request Content-Type — the unversioned `application/vnd.neo4j.query` included, which no real server accepts — is a **400** naming the two accepted spellings |
| `includeCounters` | boolean | `false` | Force the `counters` object onto the response |

A malformed body (not JSON, or missing `statement`) is **400** with code
`42N06`; parameters that do not decode — an unknown `$type` under the
typed request dialect, an unrepresentable number under either — are
**400** with the class-less general code `42000` (ADR-0425). A
statement that references a parameter missing from `parameters` is **400**
with `42N02` — it is never bound as `null` — and the message
`semantic error: ParameterMissing: Expected parameter(s): <names>`
(ADR-0308).

Content negotiation: the response content type is negotiated from the
request's `Accept` header, honoring client order — the first servable
entry wins. The **bare** `application/vnd.neo4j.query` answers with that
type and the **typed** encoding (every value `{"$type":…,"_value":…}`);
a **versioned** variant (`…vN.N+json`, `…vN.N+jsonl`) is answered with the
same bare vnd type and the same typed encoding — Browser's client REQUIRES
that content type (it rejects `application/json` with "Wrong content-type")
and then decodes with its typed codec. `application/json`, wildcards or no
`Accept` answer `application/json` with the plain encoding — the media type
selects the client's decoder. A client asking **only** for something we
cannot produce (a list with no vnd, json or wildcard entry) is **406**
with the error envelope. JSONL response encoding is a future
enhancement, deliberately not implemented. Error responses keep
`application/json` (Browser's Accept list includes it, so it can read
errors).

## Response compression (ADR-0385)

The query route negotiates an optional gzip **response body encoding** in
addition to the media-type negotiation above. This is a Relate transport
extension — Neo4j's Query API v2 documents no response compression — and it
never changes the JSON envelope, the status, or the content type.

The request may carry the standard `Accept-Encoding` header (RFC 9110
§12.5.3 list syntax). The policy:

- `gzip` (and its legacy alias `x-gzip`) is the only coding the server
  produces; every other coding (`br`, `zstd`, `deflate`, `compress`, …) is
  parsed and never selected. `identity` is always available.
- An explicit `gzip`/`x-gzip` entry decides gzip's weight; without one, a
  `*` entry decides it. `gzip;q=0, *;q=1` is identity (the specific entry
  wins), and `gzip;q=1, *;q=0` is gzip.
- A positive `q` accepts gzip; `q=0`, a malformed weight (an unparsable or
  out-of-range `q` is treated as `q=0`), and a missing, empty or
  whitespace-only header all select identity.
- Gzip is used only when the serialized body is **at least 1024 bytes**
  (inclusive) *and* — below the streaming threshold — the compressed output is
  smaller than the input. Below the gzip threshold the small control and error
  envelopes stay identity. At or above 65536 bytes the shrink check cannot run
  before the head, so weight and size alone select gzip (see
  [Response framing](#response-framing-adr-0386)).
- Encoding never fails a request: there is no `406` from `Accept-Encoding`
  (the media-type negotiation above owns that status), because identity is
  always acceptable.

On gzip the reply carries `Content-Encoding: gzip` and `Vary: Accept-Encoding`.
`Vary` is on **every** reply of the query seam — identity responses included —
so a shared cache keys both variants; the `GET /` discovery document and the
body-less `204` preflight carry neither `Content-Encoding` nor `Vary`, and the
MCP transport is unchanged. The encoder is gzip level 1
(`flate2::Compression::fast`): the encode runs on the connection thread, where
latency beats the last few percent of ratio.

**Below 65536 serialized bytes** the gzip reply is one-shot: `Content-Length`
carries the **compressed** body's length and the body is written in one call.
At or above that threshold the body streams and the compressed length is
unknown when the head goes out, so gzip is selected by `Accept-Encoding`
weight and size alone and wraps the chunked writer — see
[Response framing](#response-framing-adr-0386).

Compression applies to every query-route reply — the `200`/`201`/`202`
answers, the `4xx`/`5xx` error envelopes, and the pre-body `400`/`401`/`403`/
`415` gate answers — whose serialized body reaches the threshold. Example
(a below-threshold body, hence the one-shot `Content-Length`):

```text
POST /db/neo4j/query/v2 HTTP/1.1
Host: 127.0.0.1:7474
Content-Type: application/json
Accept-Encoding: gzip

{"statement": "RETURN range(1, 500) AS r"}

HTTP/1.1 200 OK
content-type: application/json
content-length: 934
content-encoding: gzip
vary: Accept-Encoding
connection: close

<gzip bytes>
```

```sh
curl --compressed -u neo4j:<password> \
  -H 'Content-Type: application/json' \
  -d '{"statement": "RETURN range(1, 500) AS r"}' \
  http://127.0.0.1:7474/db/neo4j/query/v2
```

`curl --compressed` advertises `Accept-Encoding: gzip` and decodes the
answer, so the shell sees the same JSON envelope an identity request returns.

## Response framing (ADR-0386)

A reply body's framing is a deterministic function of its **uncompressed
serialized size**, decided before any byte of the head is written, so a
response can never change framing mid-stream. There is no flag or config
knob: `relate serve` has no framing option. Embedders that own the HTTP
frames use `relate_http::framing`, whose `FramedJson` and `write_stream`
carry `framing::BodyEncoding` (`identity` or `gzip`).

- **Below 65536 bytes** (`STREAM_THRESHOLD_BYTES`,
  `surfaces/relate-http/src/http.rs`): the body is written one-shot with
  `Content-Length` — the compressed length when gzip is selected — and no
  `Transfer-Encoding`. This head and framing are the pre-ADR-0386 path; the
  body itself is the envelope in the wire order below (`data` first), not the
  former sorted-key serialization of one JSON value.
- **At or above 65536 bytes** (inclusive): the body is written with
  `Transfer-Encoding: chunked` and **no** `Content-Length`; `Connection:
  close` is unchanged. Frames follow RFC 9112 §7.1 — a lowercase-hex chunk
  `size` line, the chunk bytes, CRLF, and one zero-size last chunk followed by
  a single CRLF, with no trailer section and no `Trailer` header. A
  close-delimited body is never used: only the last chunk signals a complete
  response, so a truncated stream surfaces as a transport error instead of a
  clean EOF.
- **A non-HTTP/1.1 request is refused before any framing decision.** The
  parsed request head records its request-line version; a request that is not
  HTTP/1.1 answers a **400** — a parsed rejection, not the malformed-head
  path, so it is one-shot `Content-Length` and identity — instead of being
  served, so the HTTP/1.1-only chunked framing (RFC 9112 §7.1) never reaches
  an HTTP/1.0 peer that need not understand it (ADR-0386 §8).
- The **success envelope serializes in its wire order**: `data.fields`
  first, then the `data.values` rows as the serializer pulls them from the
  engine's materialized result, then the plan / counters / notifications /
  bookmark tail, then the LABS-1120 timing block — a `timings` object with
  the `planCache` outcome string beside it — and a `transaction` block on
  the tx routes. The order is
  the same for the one-shot and the chunked path, so a below-threshold reply
  carries exactly the bytes a streamed one does. The serializer feeds the
  rows to the writer one at a time and never rebuilds the rows array; what
  the pre-ADR-0386 writer did with `serde_json::to_vec` — a body-sized
  serialized copy on top of the reply — is gone above the threshold, where
  the body goes straight into the frame writer. The rows, the envelope's
  parts and the whole-object replies are still materialized before the reply
  seam (ADR-0386 §6). Every other in-seam body (error envelopes, a
  statement-less transaction answer) is serialized as one JSON object,
  chunked when it clears the threshold.
- **The `GET /` discovery document is excluded from the seam**: it is
  one-shot identity — never gzipped and never chunked, whatever its
  serialized size — and carries no `Vary` (ADR-0385 §2, ADR-0386 §7). Its
  reply policy is `ReplyEncodingPolicy::Excluded`, decided from the request
  head, so the discovery document is the one Query API reply whose framing
  and encoding depend on neither `Accept-Encoding` nor the body's size.
- **Chunk boundaries are a transport detail; do not depend on them.** Every
  chunked body is framed through one fixed-size (8 KiB) frame buffer, and no
  frame payload ever exceeds those 8 KiB: each buffer flush becomes one chunk
  frame, and a producer write larger than the buffer (a large JSON string
  scalar, the gzip encoder's own output buffer) is sliced into further frames
  of at most 8 KiB. Frames follow the bytes, never the rows or envelope
  members. The frame count tracks the body's size, so a large answer does not
  arrive as one frame per row — and a client must not read any meaning into
  where one frame ends.
- **Gzip composes above the frame writer** when the request negotiates it: the
  serialized envelope feeds the gzip encoder and the encoder's output becomes
  the chunk payloads. Above the threshold the "compressed output is smaller
  than the input" check does not run — the compressed size cannot be known
  before the head (ADR-0386 §4) — so an incompressible large answer may be
  marginally larger than identity.

Clients that decode chunked and gzip transparently (`ureq`, `curl`, the Neo4j
drivers) see the same JSON envelope as a one-shot reply. A raw client must
de-chunk the frames first, and inflate when gzip was negotiated.

## Response envelope (success, 200)

```json
{
  "data": {
    "fields": ["c"],
    "values": [[{"elementId": "0", "labels": ["City"], "properties": {"name": "Oslo"}}]]
  },
  "counters": {"nodes_created": 0, "nodes_deleted": 0, "relationships_created": 0,
               "relationships_deleted": 0, "properties_set": 0, "labels_added": 0,
               "labels_removed": 0, "indexes_added": 0, "indexes_removed": 0,
               "constraints_added": 0, "constraints_removed": 0},
  "bookmarks": []
}
```

- `counters` — the canonical 11 mutation counters with Neo4j's counting
  (ADR-0309): `labels_added`/`labels_removed` count labels per node,
  `properties_set` counts every property write (a `REMOVE n.p` included,
  nothing for a delete), and the index and constraint counters count
  schema DDL.
- `data.fields` — the column names.
- `data.values` — rows as arrays aligned with `fields`; each value is the
  **Neo4j Query API v2 encoding** (the server's `DefaultResponseModule`
  contract), which this surface follows instead of ADR-0223's kind-tagged
  wire codec (that codec stays the snapshot/CLI/MCP encoding). Under
  `application/json` values are plain JSON and entities are plain objects
  (`Node`: `{"elementId", "labels", "properties"}`; `Relationship`:
  `{"elementId", "startNodeElementId", "endNodeElementId", "type",
  "properties"}`; `Path`: a JSON array alternating node/relationship;
  temporals are ISO-8601 strings; points are `{"type": "Point",
  "coordinates", "crs"}`). Under the negotiated typed form
  (`application/vnd.neo4j.query`) every value is
  `{"$type": <Cypher type>, "_value": …}` with the Neo4j field names
  (`_element_id`, `_labels`, `_properties`, …), integers/floats as strings
  inside `_value`, and graph entities under their `_value`. Geometry (a
  Relate extension the Neo4j codecs do not know) is
  `{"$type": "Unsupported", "_value": "…"}` when typed.
- Byte arrays (ADR-0327) — the engine's statement-scoped byte array enters
  only through Bolt or a Rust `Value::Bytes` parameter, never through this
  surface's JSON parameters. A result value holding one is a list of
  integers 0–255 (a `List` of `Integer` when typed); byte arrays cannot be
  stored as properties.
- `counters` — the 11 snake_case mutation counters (zeros included); present
  when `includeCounters` was true **or** the statement updated the graph.
- `bookmarks` — always the empty array; there is no bookmark manager.
- `notifications` — the query notifications of an `EXPLAIN`/`PROFILE`
  statement (ADR-0363), absent entirely when the list is empty (a plain run
  carries none). Each entry is the Neo4j-shaped record with keys `code`,
  `description`, `severity`, `title`, `category`, built through the engine's
  one Neo4j serializer (`relate_model::wire::notification_to_neo4j_json`).
  Neo4j's key order also has a `position` slot: no position value exists, so
  the slot is **omitted**, never emitted as null. `description` is the
  diagnosis with the effective remedy appended as ` (remedy: <remedy>)`; the
  engine-native `gql_status`, `remedy` and `operator_id` keys are not
  emitted. Entries are deterministic: operator-scoped kinds in the plan's
  pre-order, then statement-scoped kinds, with identical entries collapsed.
  Example entry:

  ```json
  {
    "code": "Neo.ClientNotification.Statement.CartesianProduct",
    "description": "the MATCH patterns share no join anchor (remedy: Give the patterns a join anchor: …)",
    "severity": "INFORMATION",
    "title": "Cartesian product",
    "category": "PERFORMANCE"
  }
  ```
- `queryPlan` / `profiledQueryPlan` — the structured plan tree (ADR-0344/0345,
  ADR-0357)
  when the statement was `EXPLAIN` / `PROFILE`, in the Neo4j Query API v2
  node shape (LABS-376, verified against Neo4j 5.26's own response): the
  node's metrics ride as FIELDS — `dbHits`, `records`, `time` (ms),
  `hasPageCacheStats` (always `false`; the engine is in-memory),
  `pageCacheHits` / `pageCacheMisses` (always 0) and `pageCacheHitRatio`
  (0.0) — the args map is named **`arguments`** (never the Bolt wire's
  `args`, which is the driver's shape), and `operatorType` carries this
  engine's suffix (`ProduceResults@relate`; Neo4j writes `@neo4j`).
  The operator vocabulary is the engine's one set of names under that
  suffix, including the index-seek starts (ADR-0486): `NodeIndexSeek` for
  an anchored-equality point read on a range-indexed `(label, property)`
  pair, `NodeIndexSeekByRange` for a comparison window, the `Details`
  string reading `RANGE INDEX <var>:<Label>(<prop>) WHERE <prop> <op>
  <bound>` and `EstimatedRows` carrying the seek's own count — the same
  names and details every plan surface renders.
  `arguments` carries the engine's own args map as-is — the richer internal
  form: `Id` (the pre-order operator number), `Details` (the one-line
  details string), `EstimatedRows` (an integer on children; on the root the
  Neo4j-facing float the decoration writes, ADR-0357), the PROFILE metric
  entries (`Rows`, `TimeNs`, `DbHits`, `MemoryBytes`, `ExpressionBuiltBytes` —
  Neo4j's own
  `arguments` keeps its `Rows`/`DbHits` there too), the ADR-0538
  traversal-rung entries on a `MATCH` operator (`PatternEngine`, the
  store's `RELATE_PATTERN_ENGINE` mode; and on a PROFILE tree the served
  clause's `KernelRung` — `chain` with `KernelHops`, `hub-star`,
  `frontier-nfa`, or `walker`), and on the root the
  Neo4j-facing args (`version`, `planner*`, `runtime*`, `Memory`,
  `GlobalMemory`, `string-representation`). The metric fields above are
  projected from the same values, so both carry the metric (Neo4j's own
  response does exactly this). Each node also carries `identifiers` (list
  of strings, possibly empty), `children` (a list of child nodes; a
  childless node carries the empty list, matching Neo4j 5.26's own
  response), and `warnings` (the warning maps — `{"name": …}` plus the
  `index` payload when the warning has one — on warned nodes, absent
  otherwise). The node is read from the engine's
  own wire form (`PlanNode::to_value`) through the canonical typed accessor
  (`wire::plan_node_wire`) and encoded through the same `v2_value_json`
  the row values use; the root's Neo4j-facing args (ADR-0357) come from
  `PlanNode::to_neo4j_value`. The Bolt summary (`plan`/`profile`, on the
  final PULL SUCCESS) keeps the driver shape (`args`) — ADR-0365 records
  why the two surfaces differ. The plan
  root's `args` additionally carry the Neo4j-facing args (ADR-0357):
  `string-representation` (the plan text rendered at this boundary through
  `relate-formatter` (ADR-0359: the engine emits the tree only), headed
  by the `Cypher`/`Planner`/`Runtime` lines tooling parses and showing the
  boxed operator table (`+---+` grid) the CLI's table mode prints),
  `version`, the
  `planner*`/`runtime*` identity args (the planner and runtime named
  honestly as `relate`) and `Memory` (the root's row-materialization
  bytes). A plain
  run (no `EXPLAIN`/`PROFILE` preamble) carries neither key. An `EXPLAIN`
  statement carries **no result rows** — its `data.fields` is empty and
  `data.values` an empty array — so the plan rides the envelope alone; a
  `PROFILE` statement's `data` is the wrapped statement's own, with the
  plan beside it. Pinned by the
  test `explain_carries_query_plan_and_profile_carries_profiled_query_plan`
  (`surfaces/relate-http/src/query_api.rs`).
  Embedders that build their own HTTP envelope call
  `relate_http::query_plan_field` (ADR-0373). It returns this same
  plain-JSON node and the same key (`queryPlan` or `profiledQueryPlan`).
  The typed `{"$type","_value"}` form stays inside this server's Accept
  negotiation.

Result-metadata note (cross-surface alignment): the engine-stamped column
types (ADR-0224) stay off this envelope — under the negotiated typed media
type the per-value `$type` tags carry the same information row-by-row, and
clients needing the stamped `columnTypes` use the CLI/FFI wire envelope or
the MCP surface. The statement's timing, however, now rides the reply
(LABS-1120, reversing ADR-0491's exclusion of it): after the Neo4j members,
every success envelope carries a `timings` object — `elapsed_ms` with its
always-on `parse_ms` / `plan_ms` / `execute_ms` split (ADR-0491) and the
fine sub-split `lex_parse_ms` / `rewrite_ms` / `lower_ms` /
`result_generation_ms`, all in milliseconds — plus a `planCache` string,
the plan-cache outcome (`miss` / `hit-raw-fastpath` / `hit-template` /
`replan-generation-mismatch` / `bypassed`), which explains a hit's
near-zero parse window. These are Relate extension keys the reference
server does not send: they sit after `bookmarks` (the Neo4j members keep
their exact reference order), official drivers ignore unknown members, and
a client that reads only the reference shape sees an unchanged envelope.
The keys are per-request — two requests for the same statement differ in
them, so byte-identity comparisons (caching, fixtures) must exclude the
timing block.

## Error envelope

```json
{"errors": [{"code": "42N00", "message": "semantic error: DatabaseNotFound: Graph not found: movies"}]}
```

HTTP status and `code` by failure:

| Status | Code | Failure |
| --- | --- | --- |
| 401 | `42NFF` | missing or wrong credential |
| 404 | `42N00` | unknown `{databaseName}`, or any unknown route |
| 404 | `42N90` | managed transaction the operation's lease found idle past the deadline (ADR-0435) |
| 429 | `42N91` | begin refused: the open-transaction cap (`--max-open-transactions`) is reached (ADR-0435) |
| 405 | `42000` | wrong method on the route |
| 400 | `42N06` | malformed request body / Cypher syntax error |
| 400 | `42000` | undecodable `parameters` (an unknown `$type`, an unrepresentable number — the request dialect's folds) |
| 400 | `42N02` | a statement references a parameter missing from `parameters` |
| 400 | `22G03` | type error |
| 400 | `51N27` | unsupported database-administration command (ADR-0116) |
| 400 | `42N18` | write against the read-only `system_ontologies` namespace |
| 400 | `42000` | any other caller-facing engine error |
| 500 | `50N42` | engine-internal error; the message is genericized per the ADR-0260 trust-boundary rule (OPLANE_REQ-00340359) |
| 500 | `42000` | IO, storage and locked failures — the server's own failure, previously mis-filed as a 400; the message is genericized the same way |

## Lifecycle

One request per connection; the response closes it
(`connection: close`). There is no session state: each statement is
auto-commit, and no state carries between requests.

## Running in Docker

The shared server image (ADR-0128, unified runtime per ADR-0249) serves
this API from the same `relate serve` process as Bolt and MCP:
`make docker && make docker-run` publishes HTTP on **7474** (Bolt 7687,
MCP 7475). Auth is the one shared credential of ADR-0232 §1 —
`RELATE_AUTH`, `--auth`, or the generated token printed in `docker
logs`; the one token unlocks all three surfaces. The container binds
`--host 0.0.0.0` (plaintext; serve warns loudly per ADR-0248 — front
with TLS for anything beyond dev/test). Details, seeding, and the fleet
mode: [Bolt spec — Docker](../bolt/spec.md#docker).
