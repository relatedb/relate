---
type: adr
area: surface
name: 0234-httparse-request-parsing
status: accepted
tags:
  - http
title: httparse for HTTP/1.1 request-head parsing
REFERENCES:
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0451-bound-unauthenticated-network-connections]]"
---

# ADR-0234: httparse for HTTP/1.1 request-head parsing

## Context

[ADR-0232](./0232-network-surfaces-charter.md)'s charter for the network surfaces picked a deliberately small,
hand-rolled HTTP/1.1 layer in `surfaces/cypherlite-http` (one request per
connection, `Content-Length` bodies, no keep-alive, no TLS) over pulling a
web framework into the CLI. The caps, connection policy, auth gates, and
accept loop stayed ours; only the byte-level request-line/headers reader
was hand-rolled.

Five review rounds on that layer (PR #274) kept landing on parser-class
bugs in the hand-rolled reader: an unbounded `read_until` line buffer that
let a pre-auth peer grow a per-connection allocation past the documented
64 KiB header cap, plus repeated divergence between the read loop and the
cap checks around it. Each fix was correct and test-pinned, but the class
of bug — incremental byte accumulation with limits enforced after the
fact — is exactly what a vetted parser makes impossible by construction:
`httparse` parses a fixed buffer you hand it, so the buffer *is* the cap.

`httparse` is already in the resolved dependency graph (transitively), has
zero dependencies of its own, comes from the hyper project, and does no
I/O of its own — it is a pure byte-slice parser, so it composes with our
sync, thread-per-connection stack instead of replacing it.

## Decision

`surfaces/cypherlite-http` delegates HTTP/1.1 request-head byte parsing
(request line + headers) to the `httparse` crate. The reader fills a
`MAX_HEADER_BYTES`-bounded buffer and hands it to
`httparse::Request::parse`: `Partial` reads more within the budget,
`Complete` yields the existing `RequestHead` (method, path, lowercased
header names, UTF-8-checked values), a full buffer without completion is
the existing "headers too large" 400, and any `httparse` error is the
existing malformed-request 400. Body bytes already read past the head ride
on the `RequestHead` and are served before the reader is touched again.

Everything around the parser stays ours and unchanged: the 64 KiB header
cap, the body cap, the connection cap and read timeout, the browser guard
and pre-body auth gates, the drain plan, `serve_capped`, and the public
shapes (`gated_head`, `read_request_body`, `RequestHead`, `Request`,
`GatedHead`, `DrainPlan`). The raw-socket protection tests in
`surfaces/cypherlite-http` and `surfaces/cypherlite-mcp`
(`http_protections`) are the contract and run unchanged.

`httparse` is pinned as a workspace dependency (`httparse = "1.10"`, the
version already resolved in `Cargo.lock`) and used as
`{ workspace = true }` in `cypherlite-http` only.

## Alternatives considered

- **Keep the hand-rolled reader.** Rejected: five review rounds of
  parser-class defects (unbounded line read, cap-check ordering,
  malformed-input handling) say the risk lives in byte accumulation we
  write ourselves; a fixed-buffer vetted parser removes the class.
- **tiny_http (a full sync server).** Rejected: it swaps the accept loop
  and connection handling too — the parts of our stack the caps, auth
  gates, and drain policy live in — and its own header/body limits are no
  better than ours; it is also less actively maintained than httparse.
- **hyper / axum.** Rejected: [ADR-0232](./0232-network-surfaces-charter.md)'s no-async-runtime rationale —
  both pull a tokio runtime into the CLI and the MCP binary.

## Consequences

- The request-head parser is no longer security-critical code we maintain;
  header-section malformedness is httparse's correctness, and our caps are
  enforced by buffer size, not by checks after reading.
- The MCP transport consumes the same `cypherlite_http::http` module, so
  it gains the change with no edits of its own.
- One new direct dependency for `cypherlite-http` (already in the lockfile
  transitively; zero transitive deps). Per the workspace dependency rules
  this is a surface crate, so no engine dependency changes.
- Header *values* must still be UTF-8 (httparse works on bytes); the
  reader rejects a non-UTF-8 head with the same 400 as before.

Later references: adopted in `surfaces/cypherlite-http/src/http.rs` on the
`serve-network-surfaces` branch.

## Later references

[ADR-0451](./0451-bound-unauthenticated-network-connections.md) folds the admission readers and gate into one public
`read_admitted_request` path in `surfaces/cypherlite-http/src/admission.rs`.
The public shapes named above are now `read_admitted_request` /
`AdmissionOutcome` / `AdmissionError`; `RequestHead`, `Request`, and
`DrainPlan` remain public.
