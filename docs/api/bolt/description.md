# Bolt — description

Bolt is served by the one product binary, `relate serve --host HOST
--bolt PORT --database PATH` (ADR-0368): the standalone `relate-bolt`
binary is retired, and the `relate-bolt` crate stays as the protocol,
transport and TLS library behind it. Bolt accepts Neo4j connections so
official drivers can `RUN`
OpenCypher against an embedded Relate instance. The server speaks
**Bolt 4.4 and Bolt 5.4** (ADR-0484): the handshake's first covering
proposal picks the version, 5.x connections authenticate with LOGON
(LOGOFF returns to it) instead of the 4.4 HELLO, and at 5.4 the graph
structures carry element ids and the fixed DateTime structures. Dialect
is OpenCypher
(ADR-0056), not Neo4j Cypher. `BEGIN` / `COMMIT` / `ROLLBACK` are real
session transactions (ADR-0028); `RUN` without `BEGIN` stays auto-commit.
See the [LOGON example](examples/logon.md) for a driver-visible 5.4
connection.

The HELLO SUCCESS identifies the server honestly — `server:
"Relate/<version>"` — plus a per-connection `connection_id` the Java
driver requires (ADR-0293) and Neo4j's receive-timeout hint, which the
server backs with NOOP keep-alives during long requests (ADR-0312). Tooling that hard-checks the agent string
(cypher-shell refuses non-Neo4j identities) can be admitted with
`--identity Neo4j/5.26.0` (`relate serve --bolt` takes
`--bolt-identity`); the value is sent verbatim and is a compatibility
shim, not a Neo4j feature-parity claim.

Driver temporal and spatial parameters — `date`, `time`, `localtime`,
`datetime` (legacy `0x46`/`0x66` structs, or `0x49`/`0x69` when the driver
negotiates the `patch_bolt` `utc` patch or the connection is Bolt 5.x,
ADR-0484), `localdatetime`, `duration` and
`point` — and byte-array (PackStream Bytes) parameters are accepted and
decoded faithfully, while an unsupported parameter value fails the `RUN`;
see [Parameter value decoding](spec.md#parameter-value-decoding-adr-0177).
Results go the other way with the same encodings, as latest Neo4j sends
them at the negotiated version: temporal and point values are PackStream structures and
byte arrays are Bytes, so drivers hydrate their native types (ADR-0325,
ADR-0326, ADR-0328). Byte arrays are statement-scoped: they cannot be
stored as properties yet (ADR-0327).

Drivers select one of the fixed namespaces (`neo4j`, `system`,
`system_ontologies`) the way they select a Neo4j database:
`session(database=…)` or `execute_query(database_=…)`, sent as `db` in the
`BEGIN` or auto-commit `RUN` extra (ADR-0117). The selection is per request
and never carries over; the final `PULL`/`DISCARD` SUCCESS reports the
namespace the query ran in as `db`. See the
[namespace example](examples/namespace.md).

Result summaries follow Neo4j at Bolt 4.4 (ADR-0309 to ADR-0311, ADR-0319):
RUN SUCCESS carries `t_first`; the SUCCESS that ends a result carries the
query `type`, `t_last`, `db`, Neo4j's non-zero `stats` counters and, after an
auto-commit write, a `bookmark` — the engine's commit sequence, the same
value every other surface reports (COMMIT of a transaction that wrote
carries one too). Each RUN in an
explicit transaction keeps its own result, addressed by `qid`, and a runtime
error (`1 / 0`) is reported at the PULL, as with Neo4j. Requests that do not
fit the session's state — COMMIT without BEGIN, a nested BEGIN, PULL with
nothing to pull — are protocol breaches that close the connection with
`Neo.ClientError.Request.Invalid` (ADR-0314). See the
[query-type example](examples/query_type.md).

Compatibility with the latest Neo4j is probed, not assumed: a boltkit
wire probe and a one-time neo4rs 0.9.0-rc.10 driver probe compared
every driver-visible behavior against **Neo4j 2026.09.0 (Community) at
Bolt 4.4** — 125 findings, 50 compatible, 73 fixed, 1 deferred, the
manifest handshake decided against — and a one-time live probe at
**Bolt 5.4** (2026-11-02) pinned the 5.4 surface (ADR-0484); the
`.bolt` script corpus pins the behaviors in CI
(`cargo nextest run -p relate-bolt --test bolt_compat`). See the
spec's [Compatibility section](spec.md#compatibility) and its
per-finding [compatibility matrix](compatibility.md).

Local CSV import over Bolt is denied by default. An operator can opt the
server into bounded, rooted file reads with `--load-csv-dir PATH` or
`RELATE_LOAD_CSV_DIR` (the flag wins); client queries cannot choose a
new root. Rejected files report generic failures. This capability is
**library-level only** since ADR-0368 — no product binary exposes it; embed
`relate_bolt::serve` to serve a store with a LOAD CSV root. See the
[client example](examples/load_csv.md).

Bolt can be TLS-encrypted on the same bind with `--tls-cert`/`--tls-key`
(and optional mTLS client verification with `--tls-client-ca`), so official
drivers can dial `bolt+s://` or `bolt+ssc://` — **library-level only** since
ADR-0368: no product binary exposes TLS; embed `relate_bolt::serve`, or
front `relate serve` with a TLS-terminating proxy. Plaintext on a non-loopback
bind is refused: the server will not start without TLS unless
`--allow-plaintext` is passed, and with TLS configured it refuses the
plaintext path per connection unless the same flag is set. Encryption is not
authentication — the server authenticates by default (ADR-0126): basic auth
with `--auth user:password` or `RELATE_AUTH`, or a generated token
printed at startup and persisted next to the database when neither is set
(`--auth none` disables, e.g. on loopback behind an authenticating proxy).
A validated principal — HELLO at 4.4, LOGON at 5.4 (ADR-0484) — is
reported engine-ward, so
`dbms.showCurrentUser()` answers the real user under basic auth and the
documented placeholder `'relate'` under `--auth none` (ADR-0273).
See the [TLS example](examples/tls.md).

Bolt is also served over WebSocket (RFC 6455) on the same bind (ADR-0266) —
`ws://` on the plaintext path, `wss://` after the post-TLS sniff — so
browser clients connect with no extra port: the official JavaScript driver's
browser build, and neo4j-browser, whose default `neo4j://` scheme works via
the single-server self routing table (ADR-0269). Path results hydrate as
driver `Path` objects, so the browser and Neo4j Desktop render them as graph
patterns rather than opaque strings (ADR-0272). WebSocket serving is on by
default on serve's Bolt surface (`--no-websocket` is library-level only,
ADR-0368; `--ws-allow-origin` widens the policy) with an Origin policy that
defaults to loopback plus the hosted Neo4j Browser and Console (loopback
only under `--auth none`; ADR-0459). A WebSocket-only concurrency cap is
opt-in via `--ws-max-connections`. See the
[WebSocket example](examples/websocket.md).

A seek-started `MATCH` in the `plan`/`profile` summary nests display-only fused children — the seek step and one `Expand` per hop — marked by `FusedInto` in `args` (ADR-0622). They carry no per-step rows, time or memory; the parent's totals stand.
