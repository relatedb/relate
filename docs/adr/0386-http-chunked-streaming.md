---
type: adr
area: surface
claim: 3b0a29a7389ff920fb6afbbb6bd8428339f77a4c
name: 0386-http-chunked-streaming
status: proposed
tags:
  - http
title: chunked response streaming on the HTTP Query API
REFERENCES:
  - "[[adr/0158-streaming-single-process-pull]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0234-httparse-request-parsing]]"
  - "[[adr/0385-http-gzip-response-compression]]"
  - "[[adr/0451-bound-unauthenticated-network-connections]]"
---

# ADR-0386: chunked response streaming on the HTTP Query API

## Context

[ADR-0232](./0232-network-surfaces-charter.md) charters CypherLite's HTTP Query API v2 reply seam and [ADR-0385](./0385-http-gzip-response-compression.md)
added opt-in gzip on it, but every Query API reply is still built as one
in-memory body. `write_reply` (surfaces/cypherlite-http/src/server.rs:100-116)
destructures the `Reply` and serializes the whole JSON envelope with
`serde_json::to_vec` (:107) into one `Vec<u8>`; `http::write_response`
(surfaces/cypherlite-http/src/http.rs:502-521) writes that body in one call
under a `content-length` head and closes. `Reply.body` is already a
materialized reply body (one whole JSON object or the success envelope's
parts; `surfaces/cypherlite-http/src/v2_envelope.rs`), so the `to_vec` copy is
one body-sized allocation on top of it: peak per-connection memory scales
with the answer once more, hundreds of MB for a multi-million-row result.

The engine cannot remove the first copy: `cypherlite::Connection::query`
returns a materialized `QueryResult`
(cypherlite/cypherlite/src/connection/mod.rs:144) whose rows are
`Vec<Vec<Value>>` (cypherlite/cypherlite/src/result.rs:90) — there is no
cursor or iterator entry point. What a streaming writer can bound is the
serialization copy, and that is the decision here.

Facts that bound the change:

- One reply seam, two writers: every Query API reply goes through `write_reply` (server.rs:100), while the MCP streamable-HTTP transport calls `http::write_response` directly (surfaces/cypherlite-mcp/src/http.rs:192). MCP has no `Accept-Encoding` negotiation and is a buffered identity responder, so the shared writer's wire behavior is the seam not to move.
- The wire is HTTP/1.1 with one request per connection and `Connection: close` ([ADR-0234](./0234-httparse-request-parsing.md); `docs/api/http/spec.md:5-6`). `surfaces/cypherlite-http/src/http.rs:5` documents the module as "no chunked bodies" today, and the implementation change must correct that sentence and the matching wire-layer wording in `docs/api/http/spec.md`.
- Chunked is the only length-less HTTP/1.1 framing that still signals a clean end of body: RFC 9112 section 7.1 delimits it with a zero-size last chunk (lowercase-hex `chunk-size` CRLF, chunk bytes CRLF, and an optional trailer section this server omits entirely), which a close-delimited body cannot do — closing the socket cannot distinguish a complete body from a truncated one.
- [ADR-0385](./0385-http-gzip-response-compression.md) already fixed the gzip policy — 1024-byte threshold, `Accept-Encoding` `q` parsing with `Vary: Accept-Encoding`, one-shot `Content-Length` bodies, `flate2::Compression::fast` (level 1) — and its Decision item 1 selects gzip only when the gzip output is smaller than the input, which requires the compressed body before the head goes out. This ADR does not re-open that policy; it decides framing above a second, larger threshold and how gzip composes with it.
- In-repo clients already read chunked: ureq 3.4 unchunks a `Transfer-Encoding: chunked` body in its reader (`ureq-3.4.2/src/body/mod.rs:745`, "the returned reader will unchunk it"), and Cargo.toml:124 pins the workspace on `ureq = "3.4"`; the [ADR-0158](./0158-streaming-single-process-pull.md) pull path hands that reader to the incremental row scanner (surfaces/cypherlite-load/src/http.rs:252, surfaces/cypherlite-load/src/response.rs:71). The raw loopback helper reads to EOF (testing/cypherlite-test-support/src/loopback.rs:38-52), so tests see chunk framing verbatim.
- Neo4j Query API v2 documents `Transfer-Encoding: chunked` for its JSONL streaming form (https://neo4j.com/docs/query-api/current/streaming/), but this server's JSONL encoding is not implemented (`docs/api/http/spec.md:194-195`): the chunked framing here applies to the typed JSON envelope, so it is a CypherLite transport property, not a JSONL or Neo4j-parity claim.

## Decision

Chunked framing starts at a second, larger threshold; below it the gzip
policy's one-shot body stands untouched. Each item reproduces the pinned
policy verbatim where it is quoted.

1. **Threshold (`stream_threshold_bytes = 65536`).** Chunked streaming starts
 at 65536 uncompressed serialized bytes, inclusive — 64x [ADR-0385](./0385-http-gzip-response-compression.md)'s
   1024-byte gzip threshold, so small and page-sized answers keep a known
   length while the buffered path's extra copy stays at most 64 KiB per
   connection. Declare it once as `STREAM_THRESHOLD_BYTES: usize = 65536` in
   `surfaces/cypherlite-http/src/http.rs` beside `MAX_HEADER_BYTES` (:100),
   and derive both the `write_reply` framing decision and its boundary tests
   from that constant; do not re-derive `64 * 1024` at call sites.

2. **Framing.** Bodies at or above 65536 uncompressed serialized bytes use Transfer-Encoding: chunked (RFC 9112 §7.1: lowercase-hex chunk-size CRLF, chunk bytes CRLF, one terminating 0 chunk with no trailer section and no Trailer header), carry no Content-Length and keep Connection: close; bodies below 65536 keep the single-write Content-Length path (the compressed length when gzip is selected there), and framing is chosen from the uncompressed serialized size before the head is written. This bounds serialization memory only — cypherlite::Connection::query returns a materialized QueryResult (connection/mod.rs:144, rows at result.rs:90) — so engine cursors are out of scope. [ADR-0385](./0385-http-gzip-response-compression.md) Decision item 1's one-shot-body clause and its output-smaller check apply only below 65536: above it gzip is selected by q and size alone, because the final compressed size cannot be known before the head goes out, and the chunked ADR amends [ADR-0385](./0385-http-gzip-response-compression.md) in place (it is still Proposed). The shared http::write_response the MCP surface calls directly stays a buffered identity + Content-Length writer (its wire bytes untouched).

3. **Composition with [ADR-0385](./0385-http-gzip-response-compression.md).** gzip wraps chunked above the stream threshold: when the gzip policy (threshold_bytes = 1024, Accept-Encoding negotiation) selects gzip, the serialized envelope feeds the gzip encoder and the encoder's output feeds the one fixed 8 KiB frame buffer whose flushes become the RFC 9112 chunk frames, so the compressor sits above the frame writer and a chunk boundary is a frame-buffer flush — a transport detail, never a row boundary; identity above the threshold feeds the same frame writer, while below it gzip keeps that policy's one-shot Content-Length body. The 8 KiB bound is enforced, not just the buffer's capacity: `ChunkWriter::write` slices every producer write larger than `FRAME_BUFFER_BYTES` (a large JSON string scalar, flate2's 32 KiB output buffer) into frames of at most that size, so no frame payload of either representation ever exceeds 8 KiB — `surfaces/cypherlite-http/src/stream.rs` enforces it on every frame.

4. **Composition scope.** Below `stream_threshold_bytes`, gzip keeps
 [ADR-0385](./0385-http-gzip-response-compression.md) Decision item 1 exactly as written: the serialized envelope is
   compressed in one shot, `Content-Length` carries the compressed length,
   there is no `Transfer-Encoding`, and `Connection: close` is unchanged.

   At or above it, gzip wraps the chunked writer through that same fixed
   8 KiB frame buffer: the compressor's output fills the buffer and each
   flush becomes a chunk payload, so a chunk boundary is a buffer flush and
   never a row boundary, no `Content-Length` is written, and the compressed
   size is never computed before the head. The gzip ADR's
   one-shot/no-`Transfer-Encoding` clause and its "gzip output is smaller
   than the input" selection check are therefore scoped to the
   below-threshold path; above it gzip is selected by effective `q` and body
   size alone. That reconciles the two policies instead of leaving them
 contradictory, and it extends — does not contradict — [ADR-0385](./0385-http-gzip-response-compression.md), which is
   amended in place with this scoping and a Later references note (it is
 still Proposed). Every other [ADR-0385](./0385-http-gzip-response-compression.md) pin — the 1024-byte gzip threshold,
   `Vary: Accept-Encoding`, the `q=0` and wildcard rules,
   `flate2::Compression::fast`, and the Query-API-only scope — is unchanged.

5. **Measure before the head, without a second copy.** Framing is decided
   from the uncompressed serialized body size before any byte of the head is
   written, so a response can never change framing mid-stream. Serialization
   counts into a writer bounded at `stream_threshold_bytes`: below the
   threshold those bytes are the response body; on overflow the reply
   re-serializes straight into the chunked (and, when selected, gzip) writer
   instead of materializing the whole body with `serde_json::to_vec` just to
   measure it — that unbounded copy is the memory bug the threshold exists to
   avoid.

6. **Engine boundary.** `cypherlite::Connection::query` returns a
   materialized `QueryResult`
   (cypherlite/cypherlite/src/connection/mod.rs:144) whose rows are
   `Vec<Vec<Value>>` (cypherlite/cypherlite/src/result.rs:90); there is no
   cursor or iterator entry point. This decision bounds **serialization**
   memory only: the rows and `Reply.body` are still materialized before the
   reply seam. Engine cursors are out of scope — they need their own ADR and
   executor work, and cannot stream an `ORDER BY` or aggregation result
   anyway.

7. **Shared writer behavior unchanged.** `http::write_response`
   (surfaces/cypherlite-http/src/http.rs:502-521) keeps its one-shot
   `Content-Length` + `Connection: close` body and its wire bytes; the head
   writer derives the reason phrase from the status, so the writer no longer
   takes a redundant `reason` argument (`write_response`, its one MCP call
   site and the Query API seam shed the argument in the same change). The
   `cypherlite_http::http` writer helpers (`write_response`, `write_head`,
   `reason`) are the crate's internal wire wiring, not a stability surface:
   `reason` narrows from `pub fn` to `pub(crate) fn` in the same change, and
   the two signature changes are called out here rather than treated as
   silent breakage. Streaming is a new sibling that only the Query API's
   `write_reply` selects. The MCP streamable-HTTP transport
   (surfaces/cypherlite-mcp/src/http.rs:192) calls `write_response` directly,
   has no `Accept-Encoding` negotiation, and stays a buffered identity
   responder that never sends `Transfer-Encoding`. The `GET /` discovery
 document and the body-less 204 preflight stay one-shot as [ADR-0385](./0385-http-gzip-response-compression.md) scopes
   them.

8. **Version floor.** The surface answers HTTP/1.1 only. `RequestHead` records
   the parsed request-line version, and the shared pre-body admission pipeline (`read_admitted_request`)
   refuses a request line that is not HTTP/1.1 — HTTP/1.0 included — before
   any gate, body read or framing decision — so `Transfer-Encoding: chunked`,
   an HTTP/1.1-only framing (RFC 9112 §7.1), never reaches an HTTP/1.0 peer
   that need not understand it. The refusal is modelled as a parsed rejection
   — `AdmissionOutcome::Rejected { head, status: 400, message, extra_headers, .. }`
   carrying the real `RequestHead`; a head that never parsed is the same
 seam's `AdmissionError::Malformed` ([ADR-0451](./0451-bound-unauthenticated-network-connections.md)). Because the head is real, the caller's drain
   uses its declared `Content-Length`, the Query API's Origin echo reads the
   real header, and that surface's reply-encoding policy reads the real
   `Accept-Encoding` — so the Query API's 400 is one-shot identity with a
   `Content-Length` and no `Transfer-Encoding`, and carries
   `Vary: Accept-Encoding` on every route except the excluded `GET /`
   discovery probe. The version floor lives in
   the shared gate and applies unconditionally there: both HTTP surfaces —
   the Query API and the MCP transport — refuse HTTP/1.0 through the same
   code path, with no per-call-site parameter and no per-surface switch. The
   refusal (not a one-shot 1.0 answer) is chosen so the surface never speaks
   a negotiated framing to a peer that cannot confirm it, and so the version
   rule has one outcome instead of a second, silently different reply path.
   `docs/api/http/spec.md` and `docs/api/mcp/spec.md` state the refused
   version and the 400; a unit test pins the gate decision and a loopback
   test pins that a 1.0 request carrying an otherwise-streaming body is
   answered one-shot and identity.

9. **Reply envelope model.** The wire shape lives in
   `surfaces/cypherlite-http/src/v2_envelope.rs`, beside the value
   (`v2_json`) and plan (`v2_plan`) encodings; `stream.rs` owns framing only
   (`frame_body`, `CountingWriter`, `ChunkWriter`, `write_chunked`,
   `FRAME_BUFFER_BYTES`). A reply body is one type, `ReplyBody`: one
   insertion-ordered `Vec<(String, Json)>` member list whose `Serialize`
   writes `serialize_map` over the pairs in that order. There is no second
   body shape; the success envelope and every whole-object reply are the same
   list through the same `Serialize` implementation. The success envelope
   serializes `data` first — `data.fields`, then the materialized rows — and
   then its tail in insertion order: the EXPLAIN/PROFILE plan
   (`queryPlan`/`profiledQueryPlan`), `counters`, `notifications`,
   `bookmarks`, and the tx routes' `transaction` last. Whole-object replies
   (the error envelopes, the `GET /` discovery document, a statement-less
   transaction answer) serialize in their constructor's insertion order
   instead of the alphabetical order a `serde_json` map produced:
   `ReplyBody::new` fixes that order, and the discovery document's is
   `query`, `transaction`, `neo4j_version`, `neo4j_edition`, `auth_config`,
   then `bolt_direct` last when a Bolt endpoint exists. The order is the wire
   contract for the one-shot and the chunked path alike, because both
   serialize through the one `Serialize` impl; it is not a JSON map's
   iteration order, so a whole-object reply carries the insertion-order delta
   described above and a below-threshold reply is not byte-identical to the
 pre-[ADR-0386](./0386-http-chunked-streaming.md) sorted-key serialization of one value.
   `ReplyBody::insert_member` is the one mutation path with JSON-object
   semantics — a present key is replaced where it stands, a missing one
   appended after the last member — so inserting a top-level member needs no
   shape check, cannot produce a duplicate key and cannot panic;
   `ReplyBody::new` is the one constructor and `ReplyBody::empty()` the
   member-less body (the 202 rollback answer, `{}` on the wire). The memory
   claim is the **serialized** copy: above the threshold the body-sized
   `serde_json::to_vec` buffer is what the framing removes, while the result
   rows and the envelope's parts stay materialized before the reply seam (§6).

## Alternatives considered

- **whole-body buffer with Content-Length (status quo)** — rejected: `write_reply` serializes the whole envelope into one Vec with `serde_json::to_vec` (surfaces/cypherlite-http/src/server.rs:107) on top of the materialized rows and the `Reply.body` value (surfaces/cypherlite-http/src/query_api.rs:52), so peak memory adds a body-sized copy that grows without bound — hundreds of MB for a multi-million-row answer, exactly the growth this feature must bound. It survives only below the stream threshold, where one bounded write beats chunk framing.
- **engine-level cursor streaming: an iterator entry point on `cypherlite::Connection` that feeds rows straight to the serializer** — rejected: Engine boundary: `cypherlite::Connection::query` returns a materialized `QueryResult` (cypherlite/cypherlite/src/connection/mod.rs:144) whose rows are `Vec<Vec<Value>>` (cypherlite/cypherlite/src/result.rs:90) — there is no cursor or iterator entry point — so chunked streaming here bounds serialization memory, not row materialization. Engine cursors are named out of scope for this feature: they need their own ADR and executor work, and cannot stream an `ORDER BY` or aggregation result anyway.
- **leave the gzip framing rule untouched and stream identity bodies only** — rejected: It leaves every gzip-eligible response at or above the threshold whole-body buffered — the largest payloads the feature exists for — and drops gzip exactly where it saves most, so the memory bound would hold only for clients that did not negotiate gzip. The two framing rules would also still meet at the same reply seam with no stated precedence.
- **keep Content-Length on the streaming path by compressing the whole body into a second buffer** — rejected: The compressed buffer still scales with the body (incompressible data is body-sized), so the memory bound is not achieved for the responses that need it; it is the status quo with an extra copy, and it forces the encoder to finish before the first byte goes out.
- **start chunked streaming at the gzip threshold (1024 bytes)** — rejected: 1024 is where gzip starts paying for itself, not where buffering starts to hurt: chunking every response above 1 KiB strips Content-Length (curl progress, client size hints, simpler parsers) and pays per-frame overhead for bodies the buffered path writes in one call. 65536 is the chosen `stream_threshold_bytes`: 64x the gzip threshold, so small and page-sized answers keep a known compressed length, while the buffered path's extra copy stays at most 64 KiB per connection.
- **close-delimited body: omit Content-Length and Transfer-Encoding and rely on `Connection: close`** — rejected: A close-delimited response cannot distinguish a complete body from a truncated connection, so a cut-off result surfaces as a transport error instead of a clean EOF; chunked gives explicit termination at the same memory cost and keeps the one-request-per-connection close unchanged.
- **add a streaming mode to the shared `write_response`** — rejected: The shared writer (surfaces/cypherlite-http/src/http.rs:502) is called directly by the MCP streamable-HTTP transport (surfaces/cypherlite-mcp/src/http.rs:192), which has no `Accept-Encoding` negotiation and must stay a buffered identity responder: `write_response` keeps its buffered identity behavior and is not extended with a streaming mode, and streaming is a new sibling that only the Query API's `write_reply` selects.

## Consequences

- Serialization memory above the threshold stops scaling with the body: the
  counting pass holds at most `stream_threshold_bytes` and the chunked (and
  gzip) path writes through bounded buffers. Row materialization and
  `Reply.body` are unchanged (engine boundary), so a huge answer still
  materializes in the engine before the reply seam.
- Responses below 65536 bytes keep today's head, `Content-Length` framing and
  default identity path; the body itself is the envelope in the data-first
  wire order of §9, which is not byte-identical to the old whole-value
  serialization's sorted-key order. Every existing framing assertion on a
  small body stays valid.
- Responses at or above 65536 bytes lose `Content-Length` and gain
  `Transfer-Encoding: chunked`; `Connection: close` and the status line are
  unchanged. Clients that read to EOF or decode chunked (ureq, Neo4j
  drivers, `curl`) are unaffected; a raw test that parses a large body as
  JSON must de-chunk the frames first.
- Gzip above the threshold drops [ADR-0385](./0385-http-gzip-response-compression.md)'s "output smaller than input"
  check: the compressed size is unknown before the head, so an
  incompressible body of 64 KiB or more is sent gzip-framed and may be
  marginally larger than identity. Below the threshold that check is
  unchanged.
- The counting pass serializes the envelope twice for bodies above the
  threshold (once to measure, once to write). That is CPU traded for memory,
 the same trade [ADR-0385](./0385-http-gzip-response-compression.md) makes for gzip; the alternative is the unbounded
  copy this decision removes.
- `http.rs:5`'s "no chunked bodies" module doc and the wire-layer wording in
  `docs/api/http/spec.md` must change in the same implementation change, and
 the [ADR-0385](./0385-http-gzip-response-compression.md) Later references note records the scoping. The shared writer
  and the MCP surface do not change.
- No dependency, runtime or lifecycle change: the frame writer runs
  synchronously on the existing connection thread, one request per
 connection ([ADR-0234](./0234-httparse-request-parsing.md)), with no keep-alive, no trailers and no new crate.
- Framing is fixed before the head goes out, so a serialization failure in
  the counting pass never reaches the wire as a partial body: it surfaces as
  an `io::Error` from `frame_body` before any head byte is written, the
  connection is closed with the session error logged (the accept loop's
  handler-error policy), and the client sees a failed exchange rather than a
  truncated one. Once the chunked head is written, a write failure is the
  same truncated-transport failure class every response already has.

## Later references

[ADR-0675](./0675-http-rows-encode-lazily.md) narrows Decision 6: the success envelope no longer rebuilds the rows as a JSON tree; they encode lazily from the engine's values.

[ADR-0682](./0682-engine-batch-consumer.md) builds on this decision: the engine can hand result batches to the surface as they are produced, so the chunked response can start before the statement ends.
