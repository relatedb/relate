---
type: adr
area: surface
claim: 6a2a00a233e4d12ed7a7136da09d36eb8278399a
name: 0385-http-gzip-response-compression
status: proposed
tags:
  - http
title: gzip response compression on the HTTP Query API
REFERENCES:
  - "[[adr/0158-streaming-single-process-pull]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0234-httparse-request-parsing]]"
  - "[[adr/0386-http-chunked-streaming]]"
---

# ADR-0385: gzip response compression on the HTTP Query API

## Context

[ADR-0232](./0232-network-surfaces-charter.md) charters CypherLite's network surfaces — the HTTP Query API v2
endpoint and the MCP server — on one shared auth story and one reply writer
(`http::write_response` emits a single content-length'd body and closes;
[ADR-0234](./0234-httparse-request-parsing.md) parses the request head). That charter fixes the wire shape but says
nothing about response compression, while the Query API's whole-database
answers and counters-heavy results are the largest payloads the engine puts on
a wire.

Compression is a free, opt-in win on that seam: the in-repo clients already
advertise gzip and decode it with no call-site change, the Neo4j-compatible
JSON envelope is untouched, and the pull path ([ADR-0158](./0158-streaming-single-process-pull.md)) keeps streaming
because gzip is inflated inside its reader. The facts behind those claims:

- In-repo clients already speak gzip: Cargo.toml:124 pins `ureq = "3.4"` workspace-wide (surfaces/cypherlite-load, surfaces/cypherlite-cli, testing/cypherlite-query-latency, testing/cypherlite-graph-examples); ureq 3.4.2's default features are rustls + gzip (ureq-3.4.2/Cargo.toml: `default = ["rustls", "gzip"]`, `gzip = ["dep:flate2"]`), and Cargo.lock:3715-3722 resolves ureq with flate2 1.1.10 (Cargo.lock:1492-1500).
- ureq advertises and decodes gzip with no call-site change: with no explicit Accept-Encoding it sends `Accept-Encoding: gzip` by default (ureq-3.4.2/src/run.rs:305-322), wraps a `Content-Encoding: gzip` response in flate2's MultiGzDecoder (src/body/gzip.rs:3-23, wired at src/body/mod.rs:804-808), and strips the encoding headers after decode (src/body/gzip.rs:86-128). cypherlite-load builds its POST with only Authorization/Content-Type/Accept (surfaces/cypherlite-load/src/http.rs:237-241) and takes `resp.into_body().into_reader()` at :252, so a gzip answer decodes transparently.
- The pull stays streaming: surfaces/cypherlite-load/src/http.rs:288 passes that reader to `response::stream_v2_rows`, and surfaces/cypherlite-load/src/response.rs:7-9 documents the chunked, progressive response the row scanner consumes incrementally (`stream_v2_rows` at :71). gzip is decoded inside the reader, so the scanner, its caps and its one-row-at-a-time memory profile are unchanged ([ADR-0158](./0158-streaming-single-process-pull.md)).
- The shared raw test client cannot carry gzip: testing/cypherlite-test-support/src/loopback.rs:38-52 (`exchange_raw`) writes the request and `read_to_string`s the whole response from a std::net::TcpStream, and :67 (`post`) sends no Accept-Encoding. Identity-by-default therefore keeps every existing query-route body assertion green; a gzip test needs a byte-level exchange plus flate2 inflation, because gzip bytes are not UTF-8.
- Neo4j parity: Query API v2 documents no response compression. https://neo4j.com/docs/query-api/current/headers/ is the complete header list - request Accept, Authorization, Content-Type, neo4j-cluster-affinity; response Content-Type, neo4j-cluster-affinity - with no Accept-Encoding, Content-Encoding or Vary, and https://neo4j.com/docs/query-api/current/streaming/ documents streaming via `Accept: application/jsonl` and `Transfer-Encoding: chunked` under HTTP/1.1, naming no compression. Source: github.com/neo4j/docs-query-api (dev), modules/ROOT/pages/headers.adoc and streaming.adoc; a repo-wide grep for compress/gzip/accept-encoding has zero hits. Gzip must therefore be an additive CypherLite transport extension that clients opt into, never a Neo4j contract promise.
- One writer, one seam: surfaces/cypherlite-http/src/http.rs:502-521 (`write_response`) emits one content-length'd body and closes; surfaces/cypherlite-http/src/server.rs:100-116 (`write_reply`) is the single path every Query API reply takes, while surfaces/cypherlite-mcp/src/http.rs:192 calls `write_response` directly. Compressing in `write_reply` scopes gzip to the Query API surface and leaves MCP untouched.

## Decision

One pure negotiation function on the Query API reply seam, plus a fixed
threshold, scope and encoder. Each item below reproduces the pinned policy
verbatim; the Decision adds no degree of freedom.

1. **Inputs negotiation.** Negotiate from the raw Accept-Encoding request header (RFC 9110 section 12.5.3 list syntax) with one pure function: split on commas, trim OWS, split each element on ';', compare the coding token case-insensitively, and parse an optional q parameter (0, 1, 0.0-0.999 or 1.000; an unparsable or out-of-range q makes that element q=0). Exactly two representations exist: gzip (the legacy x-gzip alias is accepted too) and identity. gzip's effective q is the explicit gzip/x-gzip element when present, else the '*' element when present, else unset. Select gzip only when effective q > 0 AND the serialized uncompressed body is >= threshold_bytes AND the gzip output is smaller than the input; otherwise send identity. A missing, empty or whitespace-only header selects identity. Other codings (br, zstd, deflate, compress) are parsed but never selected. Encoding negotiation can never fail a request: identity is always available, so there is no 406 from Accept-Encoding (the separate Accept media-type negotiation and its 406 are untouched). On gzip: add 'content-encoding: gzip', send the compressed bytes with content-length set to the compressed length, keep the one-shot body (no Transfer-Encoding; Connection: close unchanged), and encode with flate2::Compression::fast (level 1) because the encode runs on the connection thread and latency beats the last few percent of ratio. **Scoped by [ADR-0386](./0386-http-chunked-streaming.md):** the one-shot body clause above (no `Transfer-Encoding`) and the "gzip output is smaller than the input" selection check apply only below `stream_threshold_bytes = 65536` ([ADR-0386](./0386-http-chunked-streaming.md)); at or above 65536 uncompressed serialized bytes the response is chunked and gzip wraps the chunked writer, selected by effective `q` and body size alone.
2. **Vary.** Add 'vary: Accept-Encoding' to every response written by the Query API reply seam, identity responses included, so a shared cache never serves a gzip variant to a client that did not advertise it and keeps the gzip variant keyed. `write_reply` appends it to its local header list beside the CORS echo and the WWW-Authenticate challenge, so http::write_response stays unchanged. Responses outside the seam - the GET / discovery document, the body-less 204 preflight, and the MCP transport - are never gzip and carry no Vary.
3. **q=0.** An explicit 'gzip;q=0' (or 'x-gzip;q=0') disables gzip for that request even when '*' has q>0: the more specific entry wins. q=0 on any other coding only excludes that coding, which we never send anyway. 'identity;q=0' and '*;q=0' do not produce a 406 and do not force gzip: when gzip is not selected the response is identity, because encoding is best-effort and must never fail a query that would otherwise succeed. Any effective q>0 selects gzip; no comparison against identity's q is needed because there is exactly one non-identity coding.
4. **Wildcard.** '*' with q>0 makes gzip acceptable when no explicit gzip/x-gzip element exists ('Accept-Encoding: *' selects gzip when the body clears the threshold and shrinks). An explicit gzip element overrides '*' in both directions: 'gzip;q=0, *;q=1' is identity; 'gzip;q=1, *;q=0' is gzip. '*;q=0' with no explicit gzip entry leaves gzip's effective q at 0, so identity. Empty list elements are ignored, not treated as a wildcard.
5. **Threshold (`threshold_bytes = 1024`).** threshold_bytes = 1024, inclusive: gzip only when the serialized JSON body is at least 1024 bytes. Below it, gzip's ~18-byte framing and encoder CPU cost more than they save, and the small fixed replies (201/202, 404/405/406, 401/403/415) stay identity; at or above 1 KiB a JSON query answer typically shrinks 5-10x. The 'gzip output must be smaller' check runs after the threshold, so an incompressible 1 KiB body still goes identity.
6. **Scope.** Every JSON reply the Query API surface writes through its single reply seam - POST /db/{databaseName}/query/v2 (200 success envelope), POST /db/{databaseName}/query/v2/tx (201), POST .../tx/{id} and .../tx/{id}/commit (200), DELETE .../tx/{id} (202), and all 4xx/5xx error envelopes including the pre-body 400/401/403/415 gate answers - whose serialized body is >= threshold_bytes; GET / discovery, the body-less 204 preflight, and the MCP transport stay identity.
7. **Encoder dependency.** Use flate2 for the gzip encoder; it is not a new crate in the tree. Cargo.toml:124 pins ureq = "3.4" workspace-wide, ureq 3.4.2's default features are rustls + gzip (gzip = ["dep:flate2"]), and Cargo.lock:3715-3722 resolves ureq -> flate2 1.1.10 (Cargo.lock:1492-1500 -> crc32fast, miniz_oxide 0.9.1, zlib-rs). Declare flate2 = { version = "1.1", default-features = false, features = ["rust_backend"] } in the workspace and { workspace = true } in cypherlite-http: the pure-Rust miniz_oxide backend is exactly what ureq already compiles, so there is no C toolchain, no libz-sys, no zlib-rs feature, and no new lockfile entry. Use flate2::write::GzEncoder over the already-serialized body; no async runtime and no streaming writer. R6 does not trigger (no new package, no native build).

## Alternatives considered

- **always-compress: gzip every response body, ignoring Accept-Encoding** — rejected: Violates RFC 9110 content negotiation: a client that did not advertise gzip may not decode it, so bodies arrive as binary. It also breaks every raw loopback test and any non-advertising client (curl without --compressed, older tooling), and pays encoder CPU on tiny error envelopes where gzip expands the body. Opt-in is what keeps the change invisible to the Neo4j-shaped JSON contract.
- **no-compression status quo: ship nothing** — rejected: Leaves the largest payloads - whole-database query answers and counters-heavy results - at full JSON size even though every in-repo HTTP client (ureq 3.4 sends Accept-Encoding: gzip by default) and Neo4j Browser already advertise gzip. The wire saving is free under additive negotiation, the JSON contract does not change, and the [ADR-0158](./0158-streaming-single-process-pull.md) pull path stops paying full bytes on every remote pull.
- **brotli-first: serve br ahead of gzip** — rejected: No brotli encoder is in the tree (ureq's optional brotli feature pulls only brotli-decompressor and is off), so it adds a new dependency with an R6 justification for one-shot bodies where encode latency is higher than gzip's and the ratio gain on JSON is modest; gzip is universal across ureq, curl, drivers and browsers. If brotli is ever wanted it slots into the same parser as another coding - a later decision.
- **zstd-first or deflate-first** — rejected: Same dependency and encode-latency argument as brotli, deflate's ratio is worse with a long tail of buggy client implementations, and no in-repo client advertises either by default. There is no measured need.
- **compress everything and keep the smaller body (no fixed threshold)** — rejected: Runs the encoder on every reply including 4xx/5xx and 201/202 bodies, makes per-request cost unbounded and behavior non-deterministic at the size boundary. A fixed 1 KiB threshold bounds CPU deterministically and leaves the small control replies untouched.
- **streaming gzip writer over chunked transfer: encode rows as they are produced** — rejected: The surface writes one-shot Content-Length bodies and closes the connection ([ADR-0232](./0232-network-surfaces-charter.md), [ADR-0234](./0234-httparse-request-parsing.md)); chunked framing plus a streaming encoder is a separate protocol change with its own tests. Gzip over the already-serialized body needs no framing change and is correct behind Connection: close; revisit when JSONL streaming lands (the current Accept negotiation answers +jsonl with 406).
- **compress every HTTP surface too: GET / discovery and the MCP transport** — rejected: The discovery document is the pre-auth capability probe (under 1 KiB, read before any encoding preference matters), and MCP has its own SSE/streamable-HTTP framing whose compression belongs to that surface's own decision. Keeping the seam on the Query API reply path bounds the change and its tests.
- **server-wide --gzip on/off flag instead of per-request negotiation** — rejected: A server-wide switch cannot know what a given client accepts: off leaves compression unused, on compresses for clients that never advertised it. Accept-Encoding is the per-request control, and a new CLI/config flag would be a documented surface with no consumer.

## Consequences

- Per-response peak body memory doubles: the serialized body and its
  compressed `Vec` are live at once, and the encode runs on the connection
  thread at `flate2::Compression::fast` (level 1) — latency, not the last few
  percent of ratio.
- Clients that send no `Accept-Encoding` get the identity representation:
  same status, content type and one-shot `Content-Length` framing, and no
  `Content-Encoding`. Identity stays the default and cannot regress. The body
 bytes are the reply envelope in [ADR-0386](./0386-http-chunked-streaming.md) §9's insertion order (`data`
 first), not the pre-[ADR-0386](./0386-http-chunked-streaming.md) sorted-key serialization of one JSON value, so
  they are the same JSON document as before but not byte-identical to it.
- `Vary: Accept-Encoding` is new on the query surface. `write_reply` appends
  it to its local header list beside the CORS echo and the
  `WWW-Authenticate` challenge, so `http::write_response` stays unchanged.
- No JSON or typed-JSON envelope, content type or status changes. The Accept
  media-type negotiation and its 406 are untouched, and Accept-Encoding can
  never fail a request because identity is always available.
- The `GET /` discovery document, the body-less 204 preflight and the MCP
  transport never gzip and carry no `Vary`; the change is bounded to the Query
 API reply seam of [ADR-0232](./0232-network-surfaces-charter.md).
- The encoder adds no package: `flate2` 1.1.10 is already in `Cargo.lock`
  through ureq's `gzip` feature, and the `rust_backend` feature selects the
  pure-Rust `miniz_oxide` backend — no C toolchain, no `libz-sys`, no new
  lockfile entry, and no R6 trigger.
- A gzip test must exchange raw bytes and inflate them with
  `flate2::read::GzDecoder`, because `loopback::exchange_raw` is `String`-based
  and gzip bytes are not UTF-8.
- `docs/api/http/spec.md` documents the request `Accept-Encoding` header and
  the response `Content-Encoding` / `Vary` headers in the same change.

## Later references

[ADR-0386](./0386-http-chunked-streaming.md) extends this decision above the streaming threshold. Decision item
1's one-shot `Content-Length` body (no `Transfer-Encoding`) and its "gzip
output is smaller than the input" selection check are scoped to bodies below
`stream_threshold_bytes = 65536`: at or above 65536 uncompressed serialized
bytes the Query API reply seam writes RFC 9112 chunked framing, and gzip —
when `Accept-Encoding` negotiation selects it — wraps the chunked writer
instead of buffering the whole body, so the compressed length is never known
before the head goes out. This is an extension, not a contradiction: the
negotiation rules (the 1024-byte gzip threshold, `Vary: Accept-Encoding`, the
`q=0` and wildcard rules), the `flate2` encoder and the Query-API-only scope
are unchanged. The consequence that per-response peak body memory doubles
holds below the threshold only; above it the body streams. Status stays
Proposed.
