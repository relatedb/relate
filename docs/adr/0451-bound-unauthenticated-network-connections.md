# ADR-0451: Bound unauthenticated HTTP and MCP connections

Status: Proposed
Claim: 2870d9f13f55e580f3160c1525c0674b6332ad2a

## Context

The 27 October security review's finding 3 (LABS-604) is that the HTTP and
MCP listeners admit unauthenticated connections without any bound. Two
separate gaps produce it.

**No absolute header deadline.** `read_request_head`
(`surfaces/cypherlite-http/src/http.rs`) parses the request line and
headers out of a 64 KiB-bounded buffer (ADR-0234) under a single per-read
socket timeout (`DEFAULT_READ_TIMEOUT`, 30 s). The timeout is armed once,
before the head read, and every successful read restarts it. A peer that
opens a connection and sends one byte per timeout window is therefore never
stalled long enough to be dropped: it holds a connection thread, and one
connection-cap slot, for as long as it keeps dribbling. The body read has
the same shape but is bounded by `Content-Length` and the body cap; the
header phase has no total bound at all.

**The connection cap defaults to unlimited.** ADR-0333 removed the earlier
per-listener default of 128 and made the cap an operator opt-in: one
`ConnectionCap` shared by every listener, `--max-connections`, `0`
unlimited, with a startup warning on a non-loopback bind. That reasoning
was about the RSS ceiling — three independent per-listener caps of 128 hid
the process session count the ceiling had to use — and it was correct about
*one shared cap*. It also left an unconfigured server accepting unbounded
sessions, each taking a thread and a file descriptor before the credential
check. A warning is not a bound.

The report's suggested numbers were not tested. This ADR fixes the
mechanism first and then picks defaults from measurements on the product
path (loopback): a warm discovery request completes in
about 79-104 µs, 128 concurrent discovery requests are all served in 8 ms,
256 concurrent requests in 14-19 ms (twice the chosen cap), and 128 idle
connections add about 3.7 MiB RSS in a process that also holds the 128
client sockets — about 29 KiB per connection including the client side.
A 10 Hz trickle is dropped at 10.001 s with the default below.

ADR-0238 put the accept policy in `cypherlite-serve` so the surfaces
cannot drift on it; the same home holds the new defaults. ADR-0435 is the
shape precedent for the configuration surface: shared flag → environment →
default resolution, a non-zero security default, `0` as the explicit
off spelling, and a startup warning only for the genuinely unguarded
posture.

## Decision

### 1. An absolute header deadline, enforced per read

Every HTTP-based listener (the Query API and the MCP streamable-HTTP
transport) reads a request header block under an absolute wall-clock
deadline measured from the start of the connection handler. When the
deadline passes before the headers are complete, the connection is
dropped with no reply — a peer that has not completed a request line and
headers is not speaking the protocol, so a 4xx would be a guess.

The per-read timeout is **re-armed to the time remaining before every
underlying read**, not fixed. The header reader (`read_request_head`) is
the one loop that performs those reads; the admission reader passes it a
`ReadBudget` carrying the `deadline: Option<Instant>` and the
`arm(Duration)` callback, and the surfaces supply the callback that calls
`TcpStream::set_read_timeout` on a cloned handle of the same socket. This is what defeats trickling: a byte
that arrives 50 ms into a 10 s deadline leaves 9.95 s armed, so the next
read still cannot outlive the deadline.

The shared gate and the defaults live in one place: the one public
admission path `read_admitted_request` in `cypherlite-http::admission`
(both surfaces consume it), and `DEFAULT_HEADER_DEADLINE` /
`DEFAULT_BODY_DEADLINE` / `DEFAULT_READ_TIMEOUT` with their resolvers in
`cypherlite-serve`.

**The default is 10 s.** A normal loopback request completes in about
79 µs (measured; more than five orders of magnitude of headroom for slow links),
while one dribbled byte per window cannot stretch a worker past 10 s.
`--header-deadline-secs N` / `CYPHERLITE_HEADER_DEADLINE_SECS` override it
through the shared resolution ladder; `0` disables the deadline.

**The body phase has its own absolute deadline.** Round 1 of the security
review found the header-only rule incomplete: after the gate, the body
read still ran under the resettable 30 s per-read timeout, so an
unauthenticated peer could declare a `Content-Length` and trickle the body
on a public route, pinning a worker until the connection cap filled.
`read_request_body` now takes the same `deadline`/`arm` pair and re-arms
per read. The clock starts at the body read and the rule applies to every
body, public or authenticated: a generous bound costs a legitimate upload
nothing, and one policy closes the unauthenticated case without exempting
the route class the attacker chose.

**The default is 60 s.** The body cap is 16 MiB; a full-cap loopback
upload completes in 7-11 ms (measured), and 60 s still admits a full-cap
upload at roughly 2.2 Mbit/s while dropping a trickled body at a fixed
wall-clock bound. `--body-deadline-secs N` /
`CYPHERLITE_BODY_DEADLINE_SECS` override it, `0` disables it, and the
unbound-surface policy rejects it on a run that binds no HTTP-based
listener.

**The outcome policy is typed at the same seam.** A failed admission read
is `AdmissionError::{Malformed, Deadline, Io}`, so a surface matches a
class instead of sniffing `io::ErrorKind` or a message string; a parsed
gate decision becomes `AdmissionOutcome`, which carries a rejection's
status, message, extra headers and `DrainPlan` together. The one public
admission path, `read_admitted_request`, owns gate → re-arm → bounded body
read, and both surfaces only render the outcome. The drain plan is a byte count
computed once from the parsed head (`DrainPlan::for_head`), because the
outcome also carries that head for the reply's Origin echo and encoding
policy.

### 2. A non-zero process-wide connection cap

`resolve_connection_cap`'s default becomes **128** instead of `0`, and
`DEFAULT_MAX_CONNECTIONS` in `cypherlite-serve` carries the same value for
embedders and the MCP transport, which import it directly. `cypherlite serve` still creates **one** cap for every listener
(ADR-0333), so 128 is a process total, not a per-surface number.

128 is chosen from two measurements and one hard bound:

- **Serviceability above the cap**: 128 concurrent discovery requests
  were all served in 8 ms, and 256 — twice the chosen cap — were all
  served in 14-19 ms, on loopback. The default therefore sits well below
  measured capacity rather than at its edge. The old per-listener 128
  was a per-surface number (384 across three listeners, never 384 on
  one).
- **Cost at the cap**: 128 idle connections added ~3.7 MiB RSS in a
  process that also held the 128 client sockets — about 29 KiB per
  connection including the client side, so the server share is smaller.
  Single-digit MiB, not a memory ceiling.
- **Engine bound**: the executor has 256 process-wide snapshot-pin slots
  (ADR-0435). Half of them is the same ratio ADR-0435 chose for managed
  transactions; a full connection table leaves the other half for
  auto-commit, Bolt sessions, and every other pin consumer.

`0` remains the explicit unlimited spelling and keeps ADR-0333's
non-loopback warning. The standalone Bolt binary keeps its own flag
default (`surfaces/cypherlite-bolt/src/cli_args.rs`); it is outside this
finding's surface set. A `cypherlite serve` that binds Bolt does consume
the shared 128 cap.

`HttpConfig` and `McpServerConfig` gain `header_deadline` and
`body_deadline` (defaults `DEFAULT_HEADER_DEADLINE` 10 s and
`DEFAULT_BODY_DEADLINE` 60 s); `HttpConfig::max_connections` and
`McpServerConfig::max_connections` inherit the new non-zero default. The
CLI resolves `--max-connections` and both deadline flags before auth, so
a malformed environment value fails startup before the generated-token
path persists anything. Both deadline flags follow the unbound-surface
policy of their siblings: a run that binds neither `--http` nor an MCP
listener (`--mcp stdio`, or a bolt-only serve) rejects them loudly
instead of discarding the value.

Both deadline resolvers also bound the value at `MAX_ADMISSION_DEADLINE`
(one day) at **resolution time**. The value feeds `Instant::now() +
duration`, which panics on overflow; a loud startup error naming the
flag, the environment variable, and the accepted range replaces a
per-connection panic for a value that could never be a usable deadline.
The HTTP and MCP entry points also run the same bound on a directly
constructed `HttpConfig` / `McpServerConfig` (`validate()`) and return the
typed error (`io::ErrorKind::InvalidInput` / `ServeError::Io`), so the
invariant holds for embedders that never pass through the CLI resolver.
Only the CLI turns a startup validation failure into a process exit; a
library entry never ends its embedder's process.

### 3. The embedding path states its limit

`serve_connection` accepts any blocking `Read + Write` (ADR-0428), so this
crate cannot re-arm a timeout it does not own. It still checks both
deadlines between reads. A host that wants a deadline to hold during a
blocking read must set a read timeout no longer than that deadline; the
spec states this.

## Alternatives considered

- **Rely on the existing per-read timeout only.** Rejected: every
  successful read resets it, so one byte per window holds the worker
  indefinitely — the exact attack.
- **Armed-then-fixed deadline with a watchdog thread.** Rejected: a
  background thread (or timer wheel) per listener, a second clock path,
  and time-dependent teardown for a bound the read loop can enforce with
  the timeout it already owns.
- **Leave the body on the resettable per-read timeout.** Rejected: that
  is the incomplete state round 1 of the security review found — a
  declared body trickled one byte per window holds a worker exactly as a
  trickled header did.
- **Exempt authenticated bodies from the body deadline.** Rejected: a
  no-auth server treats every route as effectively unauthenticated, the
  bound is generous enough that a legitimate upload never notices it, and
  a route-class exemption is a second policy to keep aligned.
- **Enforce the deadline separately in the Query API and MCP.**
  Rejected: both already share `gated_head`/`read_request_head`
  (ADR-0238's one-policy rule); a second copy is where the two would
  drift.
- **Keep the cap default at 0 and rely on the ADR-0333 warning.**
  Rejected: the finding is that an unauthenticated connection is
  unbounded by default; a warning leaves the bound to an operator who
  does not know the load.
- **Default the cap to 256.** Rejected: 256 is the measured capacity,
  not below it, so the default would sit at the measurement's edge; and
  if every session pins, it consumes the whole snapshot-pin table and
  leaves nothing for auto-commit — the headroom ADR-0435 reserved.
- **Default the cap to 1024.** Rejected: no measured need, and it
  approaches common file-descriptor limits (a stock macOS soft limit is
  256) without buying serviceability the pin table can use.
- **Set the header deadline to the 30 s read timeout.** Rejected: a
  legitimate loopback header takes microseconds; 30 s pins a worker 3000x
  longer than needed. **2 s** was rejected as too tight for high-latency
  links and TLS-terminating proxies in front of the plaintext surface.
- **Drop the connection with a 408 on deadline expiry.** Rejected: the
  peer has not sent a complete request line, so there is no parsed head
  to frame a reply against; closing is the honest outcome.

## Consequences

- An unconfigured serve now refuses the 129th simultaneous connection
  (accept-then-close) and drops a header block unfinished after 10 s.
  Deployments that legitimately need more open sessions or tolerate
  slower header senders raise `--max-connections` /
  `--header-deadline-secs`; `0` restores the pre-ADR behavior explicitly.
- A stock `ulimit -n` smaller than the cap can fail before the cap does;
  the cap is a policy ceiling, not a file-descriptor guarantee.
- `serve_connection` hosts must set a read timeout no longer than the
  relevant deadline for in-read enforcement; the documented contract
  changes from "host owns the read timeout" to "host owns the read
  timeout *the deadline arms*".
- A body upload slower than 60 s (or the configured value) is dropped at
  the deadline even though it is making progress; operators serving large
  bodies over slow links raise `--body-deadline-secs`. A deadline outside
  `0 ..= 86400` seconds is a startup error, not a runtime panic.
- The Query API and MCP share the deadline machinery, so a single
  real-socket test on each surface proves the trickle defense; the
  connection-cap tests prove serviceability at the cap and over-cap
  refusal.
- `docs/api/http/spec.md`, `docs/api/mcp/spec.md`,
  `docs/api/cli/spec.md`, and `serve --help` carry the new knobs and
  defaults in the same change (AGENTS.md §3).
