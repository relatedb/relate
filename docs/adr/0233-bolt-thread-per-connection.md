---
type: adr
area: surface
name: 0233-bolt-thread-per-connection
status: proposed
tags:
  - bolt
  - serve
title: Replace cypherlite-bolt's tokio core with thread-per-connection
REFERENCES:
  - "[[adr/0109-bolt-tls]]"
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0139-bolt-failed-hello-delay]]"
  - "[[adr/0140-bolt-route-advertises-dialed-address]]"
  - "[[adr/0141-bolt-single-handshake-deadline]]"
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0227-ontology-document-format]]"
  - "[[adr/0238-shared-accept-loop]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0264-bolt-signal-shutdown]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0269-bolt-route-self-routing-table]]"
  - "[[adr/0504-bolt-connection-thread-fast-path]]"
---

# ADR-0233: Replace cypherlite-bolt's tokio core with thread-per-connection

## Context

LABS-299. `surfaces/cypherlite-bolt` serves Bolt over TCP, TLS
(tokio-rustls) and WebSocket (tokio-tungstenite) on a tokio runtime:
`tokio::spawn` per accepted connection (server.rs), a first-byte peek
transport dispatch (transport.rs, [ADR-0109](./0109-bolt-tls.md)/[ADR-0266](./0266-bolt-websocket-transport.md)/[ADR-0141](./0141-bolt-single-handshake-deadline.md)), and a
`spawn_blocking` take/put-back seam (protocol.rs `spawn_engine` /
`offload_engine`) that exists only because the engine `Connection` is
`!Sync` and async worker threads are shared. The crate's protocol state
machine (`protocol.rs`) and PackStream codec (`packstream.rs`) are already
pure byte-buffer work; only their I/O framing is async.

The server's actual role is short-lived fast queries, mostly local, driven
by visual tooling (Neo4j Browser/Bloom over bolt+ws://) and scripts, under
a default cap of 128 concurrent connections. The runtime story makes the
crate heavy to embed (LABS-298's `--bolt` flag would drag a tokio runtime
into the CLI) and to reason about: `cargo tree -p cypherlite-bolt` today
spans 181 unique crates (163 with normal edges only), of which 30 are
reachable only through the tokio/tokio-rustls/tokio-tungstenite/futures
roots.

A review of every async piece found a sync replacement for each, with
nothing requiring a reactor:

- Per-connection concurrency: `std::thread` per connection; the
  `max_connections` cap keeps its accept-then-close semantics via an
  atomic counter.
- The `!Sync` `Connection` offload: deleted — the connection thread owns
  its `Connection` for the session's lifetime.
- Handshake/idle timeouts (the single [ADR-0141](./0141-bolt-single-handshake-deadline.md) accept-time deadline):
  `SO_RCVTIMEO` via `TcpStream::set_read_timeout`, recomputed as the
  remaining deadline budget before each negotiation phase.
- Query timeout ([ADR-0111](./0111-engine-bounded-execution.md)): a watchdog thread sleeps the budget then calls
  the existing engine `CancelToken::cancel`; completion is signalled over
  `std::sync::mpsc`. Engine cancellation is already cooperative.
- TLS: rustls has a sync API (`rustls::StreamOwned` over the peeked
  `TcpStream`); tokio-rustls is only a wrapper. The post-TLS first-byte
  re-sniff uses `std::io::BufReader::fill_buf`, the same non-consuming
  trick as today.
- WebSocket: the `tungstenite` crate's own API is sync;
  tokio-tungstenite is only its async wrapper. The 400/403 rejection
  control is hand-rolled over `create_response`/`write_response`, which
  is sync-compatible, and the close handshake is a sync Close-frame
  exchange under the read timeout. WebSocket stays: Browser/Bloom speak
  bolt+ws:// from a browser, and raw-TCP-only would cut visual tooling
  out.
- SIGINT/SIGTERM ([ADR-0264](./0264-bolt-signal-shutdown.md)): a `signal-hook` iterator thread (one tiny
  crate added). Platform caveat: signal-hook's iterator module is
  source-gated to non-Windows, so the Windows shutdown variant waits
  on the flag module instead (`signal_hook::flag::register` plus an
  atomic poll) — same wait-then-report contract, no runtime either
  way, and no extra crate.
- The entrypoint fleet supervisor: `std::process::Command` plus one
  `wait()` thread per child feeding an mpsc; the `select!` becomes a
  `recv()`.
- The [ADR-0139](./0139-bolt-failed-hello-delay.md) failed-HELLO delay: `std::thread::sleep` — blocking the
  offending connection's own thread is the correct semantics.

## Decision

Replace the tokio async core of `surfaces/cypherlite-bolt` with a
synchronous thread-per-connection design:

1. Accept loop on `std::net::TcpListener`; one `std::thread` (with an
   explicit `stack_size`) per accepted connection; accept-then-close
   over the `max_connections` cap, tracked by an atomic counter.
2. `protocol.rs` / `packstream.rs` / `transport.rs` become generic over
   `std::io::Read + Write` instead of `AsyncRead + AsyncWrite`; the
   `spawn_engine`/`offload_engine` seam and the
   `with_timeout`/`with_deadline` future plumbing are deleted. Timeouts
 are socket read deadlines derived from the one [ADR-0141](./0141-bolt-single-handshake-deadline.md) negotiation
   deadline, then the per-read idle timeout.
3. TLS via `rustls` sync `StreamOwned` over the peeked `TcpStream`
   (unchanged); WebSocket via sync `tungstenite`, reached directly with
   `default-features = false` and only its `handshake` feature (no tokio
   wrapper, no TLS features — TLS is already terminated by rustls).
4. Query timeout via watchdog thread + `CancelToken` + mpsc, preserving
 the [ADR-0111](./0111-engine-bounded-execution.md) semantics (cancel, then await the engine's prompt
   return so the `Connection` stays usable).
5. Signals via `signal-hook`; the fleet supervisor via
   `std::process::Command` + wait-threads + mpsc.
6. Dependencies: drop `tokio`, `tokio-rustls`, `tokio-tungstenite`,
   and `futures`; add `signal-hook`; depend on `rustls` and
   `tungstenite` directly. Tests become sync (`#[tokio::test]` and
   `tokio::time::pause` uses are replaced by real short timeouts and
   sync client helpers).

Behavioral contract — [ADR-0260](./0260-trust-boundary-error-sanitization.md)/[ADR-0110](./0110-bolt-connection-query-governance.md)/[ADR-0111](./0111-engine-bounded-execution.md) (limits and failure
semantics), [ADR-0126](./0126-bolt-basic-auth.md) (auth), [ADR-0264](./0264-bolt-signal-shutdown.md) (signal exit), [ADR-0139](./0139-bolt-failed-hello-delay.md) (failed-HELLO
auth + failed-HELLO delay), [ADR-0266](./0266-bolt-websocket-transport.md) (WebSocket upgrade, Origin policy,
400/403 control), [ADR-0269](./0269-bolt-route-self-routing-table.md)/[ADR-0140](./0140-bolt-route-advertises-dialed-address.md) (ROUTE), [ADR-0141](./0141-bolt-single-handshake-deadline.md) (single
negotiation deadline), [ADR-0177](./0177-bolt-parameter-conversion.md) (temporal param decode) — is unchanged;
the existing protocol/auth/websocket test suites must pass with only
sync-driving edits.

## Alternatives considered

- **Hand-rolled RFC 6455 codec (over the workspace `httparse` for the
  Upgrade request parse).** Would drop tungstenite plus its
  `sha1`/`data-encoding`/`utf-8`/`http` subtree (~5 crates), but an RFC
  6455 server codec is security-sensitive surface — masked-frame
  enforcement, RSV-bit rejection, control-frame rules (FIN set, length
  ≤ 125, close-code validation), fragmentation bounds, and the close
  handshake all sit exactly where a vetted crate earns its keep — and
 the workspace `httparse` adoption ([ADR-0227](./0227-ontology-document-format.md)) covers the HTTP
  surfaces' request-head parse job, not this one. Rejected: ~5 crates
  is not worth owning that correctness and security surface.
- **Keep tokio.** The runtime is a heavy embed for LABS-298, and the
  review found no piece that needs it: every async construct in the
  crate has a standard-library or sync-crate counterpart, and the
  offload seam is an artifact of async rather than a requirement.
  Rejected: it forecloses the dependency and code-volume win with no
  compensating behavior.
- **Single-threaded poll/epoll event loop (e.g. `polling` or
  hand-rolled mio).** Fewer threads, but re-introduces exactly the
  shared-state problem that forced the `!Sync` offload seam, and
  hand-rolled event loops are harder to reason about than blocking
  threads. Rejected: complexity returns without the tokio ecosystem.
- **Channel-driven single engine thread (the [ADR-0259](./0259-query-execution-stack-provisioning.md) query-worker
  pattern) with I/O threads per connection.** Sound, but the engine
  already supports concurrent `Connection`s and the Bolt workload is
  few-connection; one thread per connection doing its own engine calls
  is simpler and keeps per-connection backpressure local. Rejected as
  unnecessary indirection for this workload.
- **Drop TLS along with tokio.** Loopback plaintext is already the
 default, but the [ADR-0109](./0109-bolt-tls.md) plaintext policy (refuse non-loopback
  without TLS or `--allow-plaintext`) presupposes TLS as the sanctioned
  encrypted path. Rejected: TLS stays, via sync rustls.

## Consequences

- Dependency tree for `cypherlite-bolt` shrinks from 181 unique crates
  (163 normal-edge) to 150 normal-edge as landed (149 plus
 `cypherlite-serve`, the shared accept-loop crate of [ADR-0238](./0238-shared-accept-loop.md); 165 with dev and
  build edges — the default `cargo tree` edge set; 158 with dev edges
  only; measured with `cargo tree -p cypherlite-bolt [-e normal]
  --prefix none`, unique names): the tokio, tokio-macros, tokio-rustls,
  tokio-tungstenite, futures×9, mio, socket2, slab and pin-project-lite
  removals land offset by +signal-hook and by unrelated tree growth
  since the 163 baseline was measured. `signal-hook-registry`
  stays — it is signal-hook's own dependency. rustls and tungstenite
  remain, reached directly; tungstenite is pinned to
  `default-features = false, features = ["handshake"]`, so its TLS
  feature trees never enter the graph. Invert-tree checks on the landed
  lockfile (`cargo tree -p cypherlite-bolt -e normal -i <crate>`)
  confirm block-buffer, digest and crypto-common stay reachable via
  rustls's sha2 and are not removals.
- Code volume drops materially: the offload seam, the future plumbing,
  and the async generics disappear; `#[tokio::test]` suites become sync
  tests.
- The `--bolt` flag on LABS-298 is re-priced: embedding the Bolt server
  in the CLI carries no async runtime dependency.
- Thread-per-connection caps connection count at what thread stacks
  afford; the default `max_connections` of 128 is well inside that.
- The paused-time tests for the [ADR-0139](./0139-bolt-failed-hello-delay.md) failed-HELLO delay lose
  `tokio::time::pause`; they use short real delays instead.

## Later references

[ADR-0504](./0504-bolt-connection-thread-fast-path.md): the connection thread no longer *waits* on the worker for
every statement — on a connection whose thread carries the worker's own
64 MiB stack (which this ADR's explicitly stacked connection threads
already did, matched to [ADR-0259](./0259-query-execution-stack-provisioning.md)), bounded-work statements now run
directly on that thread, and only the statements that can outlive a
keep-alive interval escalate to the worker. The thread-per-connection
architecture, the sync transport, and every behavioral contract here are
unchanged.
