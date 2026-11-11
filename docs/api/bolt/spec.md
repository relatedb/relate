# Bolt — spec

Protocol: Bolt 4.4 and Bolt 5.4 (ADR-0484; handshake magic `0x6060B017`).
PackStream v1 messages.
Version negotiation follows the client's four proposals (ADR-0303,
generalized by ADR-0484). The server offers Bolt 4.4 and Bolt 5.4 only —
no intermediate 5.x minor, nothing newer — and reads the proposals in
client order: the first proposal covering a spoken version decides. A
proposal `00 RR mm MM` covers spoken version `M.m` when `MM = M` and
`mm - RR <= m <= mm` (`RR` is the Bolt 4.3+ range of lower minors; the
subtraction saturates at 0, and a range never spans a major), and the
reply is that version's `00 00 mm MM` — `00 00 04 04` or `00 00 04 05`.
When no proposal covers a spoken version, the reply is `00 00 00 00` and
the server closes the connection without reading further or starting a
session, inside the same ADR-0141 deadline. Zero-filled slots and
manifest proposals (`00 00 01 FF`, Bolt 5.7+) never match — the manifest
handshake is not implemented (ADR-0484).

> **One product binary (ADR-0368).** The standalone `relate-bolt` binary
> is **retired** (LABS-381). The crate stays (protocol, transport, TLS,
> PackStream), and Bolt is served by the CLI's one product binary:
> `relate serve --host HOST --bolt PORT --database PATH
> [--auth USER:PASSWORD|none] [--bolt-identity STRING]
> [--read-only[=FRESHNESS]] [--max-connections N]
> [--ws-allow-origin ORIGIN] [--ws-max-connections N]
> [--concurrent-logical-workspace-limit SIZE]
> [--max-traversal-depth N] [--safe|--no-safe]`
> (canonical: `cargo run -p relate-cli -- serve --host 127.0.0.1
> --bolt 7687 --database :memory:`). Capabilities the retired binary
> uniquely exposed — TLS (`--tls-cert`/`--tls-key`/`--tls-client-ca`),
> `--load-csv-dir`, `--no-websocket`,
> `--allow-plaintext`, `--handshake-timeout-secs`/`--idle-timeout-secs`/
> `--query-timeout-secs`, `--max-result-bytes`,
> `--checkpoint-interval-secs`, `--wal-dir` — are
> **library-level only** (embed `relate_bolt::serve`); a follow-up ADR
> may give serve flags. `--logical-graph-size-limit` is on `relate serve`
> (ADR-0390), and so is `--durability-mode` (ADR-0470). For TLS, front `relate serve` with a
> TLS-terminating proxy.

Embedded: `relate serve --bolt PORT` (LABS-298) hosts this server on
`127.0.0.1:PORT` inside the CLI's `serve` command, sharing the open store and
the resolved auth credential with the HTTP/MCP surfaces (loopback plaintext,
no TLS, WebSocket on with the default Origin policy — loopback plus the
hosted Neo4j Browser/Console with auth, loopback only with `--auth none` —
and default limits). The embedding path is `serve_listener` (the CLI binds the socket and
shares one `Arc<Database>`), not `serve`, which opens a store itself.

| Message | Support |
| --- | --- |
| HELLO | The extra map must carry a string `user_agent`; a HELLO without one is rejected before any auth check, at either version (see [HELLO](#hello-adr-0293-adr-0312-adr-0313)). At 4.4 the auth token rides in the HELLO map, per [Authentication](#authentication-adr-0126). SUCCESS metadata: `server` (the agent string — default `"Relate/<version>"` single-sourced from the crate's `CARGO_PKG_VERSION`, overridable with `--identity`/`RELATE_IDENTITY`, ADR-0293), `connection_id` (`conn-N`, unique per listener — required by the Java driver's HelloResponseHandler) and `hints: {"connection.recv_timeout_seconds": 120}` (ADR-0312), plus `patch_bolt: ["utc"]` when the extra's `patch_bolt` asks for the utc patch (see [patch_bolt utc](#patch_bolt-utc-adr-0329)). At 5.4 (ADR-0484) the HELLO does not authenticate: a `bolt_agent` map of string keys and string values is required beside `user_agent`, the auth fields (`scheme`/`principal`/`credentials`) and `patch_bolt` are silently ignored — no acknowledgment key — the hints add `telemetry.enabled: false`, and the session stays in the AUTHENTICATION state until LOGON opens it; the notifications filter keys are accepted in the extra there (see [Notifications](#notifications-adr-0363)). |
| RUN | `RUN cypher [params] [extra]` then `PULL`/`DISCARD`. The statement field is required: a RUN whose first field is absent or not a string answers FAILURE `Neo.ClientError.Request.Invalid` (`missing RUN statement …` / `unsupported request value for statement: <kind> …`) and enters the failed state (ADR-0342). The extra's `db` selects the namespace for that auto-commit query (ADR-0117 §7); see [Namespace selection](#namespace-selection-adr-0117). The extra's `bookmarks` are accepted and ignored (ADR-0310). Parameter values are decoded per [Parameter value decoding](#parameter-value-decoding-adr-0177). The RUN SUCCESS carries `fields` (the result column names), `columnTypes` (the per-column type names, a Relate extension key; see [Column types](#column-types-adr-0224)), `t_first` (whole milliseconds until the buffered result was available) and, inside an explicit transaction, `qid` (ADR-0311). An `EXPLAIN`/`PROFILE` statement's structured plan rides the **final PULL SUCCESS** instead — Neo4j's placement, so strict clients (cypher-shell) read it (see [Structured plan](#structured-plan-adr-0344-0345-adr-0357)) — and so do the statement's `notifications` when it has any (see [Notifications](#notifications-adr-0363)). See [Results and summaries](#results-and-summaries-adr-0309-adr-0311). |
| PULL | `PULL {n, qid}` returns at most `n` buffered RECORD(s) of the result `qid` addresses (`n = -1` drains; `qid` absent or `-1` is the most recent RUN's result). A `n` or `qid` that is present but malformed — not an Int, or an `n` outside `-1` and the non-negative Ints — is a FAILURE `Neo.ClientError.Request.Invalid` and the failed state (ADR-0342); the `-1` defaults apply only when the key is absent, so a present out-of-range `n` (e.g. `-5`) fails instead of draining. A page that leaves records answers exactly `{has_more: true}`; the PULL that takes the last record answers the [ending summary](#results-and-summaries-adr-0309-adr-0311), which carries the `EXPLAIN`/`PROFILE` plan under `plan`/`profile` (ADR-0357, Neo4j's placement) and the statement's `notifications` when it has any (ADR-0363). |
| DISCARD | `DISCARD {n, qid}` drops at most `n` rows of the addressed result (`n = -1` drops the rest) and answers like PULL: `{has_more: true}` while rows remain, else the ending summary — with the same `plan`/`profile` pair (ADR-0357) and `notifications` list (ADR-0363). A malformed `n` or `qid` is the same FAILURE as PULL (ADR-0342). |
| RESET | Clears failed state (rolling back an open transaction first) and drops every open result. A failed rollback is a FAILURE — the failed state stays set, so a retrying RESET gets another attempt; it is never reported as a clean reset. A RESET pipelined behind other messages interrupts them: see [RESET priority](#reset-priority-adr-0317). |
| GOODBYE | Close |
| BEGIN | Starts an explicit transaction on the session connection (ADR-0028). The extra's `db` selects the transaction's namespace (ADR-0117 §7); its `bookmarks` are accepted and ignored (ADR-0310). BEGIN inside an open transaction is a [protocol breach](#protocol-breaches-adr-0314). |
| COMMIT | One WAL commit of that transaction; SUCCESS is `{bookmark}`, the engine's commit sequence, or `{}` when the transaction wrote nothing (ADR-0310, ADR-0502 §7). Results still open are dropped. COMMIT without a transaction is a [protocol breach](#protocol-breaches-adr-0314). |
| ROLLBACK | Discard uncommitted writes and every open result; SUCCESS is `{}`. ROLLBACK without a transaction is a [protocol breach](#protocol-breaches-adr-0314). |
| ROUTE | Single-server self routing table (ADR-0269, ADR-0318): SUCCESS with `rt: {servers: [{addresses: [<self>], role: "WRITE"}, {…, role: "READ"}, {…, role: "ROUTE"}], ttl: 300, db: <db>}`, the roles in that order. `db` is the extra's `db` when it names one, else the home database `"neo4j"`. `<self>` is the routing-context `address` the client dialed when present and non-empty, else the connection's own local socket address (ADR-0140); an operator-set `--advertised-address` is the recorded future step, not a flag today. Answered regardless of the failed state; the auth gate still applies. |
| LOGON | 5.4 only (ADR-0484) — one field, the auth token map `{scheme, principal, credentials}`. `basic` authenticates against the resolved credential ([Authentication](#authentication-adr-0126), ADR-0126's resolution order unchanged) and forwards the verified principal engine-ward (ADR-0273); `none` authenticates only under `--auth none` (under which every LOGON succeeds). Any other scheme, or a mismatched `basic` token, fails `Neo.ClientError.Security.Unauthorized` (`The client is unauthorized due to authentication failure.`) after the ADR-0139 delay, and the connection is DEFUNCT — closed with nothing after it answered. Success is SUCCESS `{}`. Answered only while the session awaits LOGON — after HELLO, or after LOGOFF; every other message but GOODBYE there is an AUTHENTICATION-state [protocol breach](#protocol-breaches-adr-0314). From an open session LOGON itself is a breach; the failed state IGNOREs it. |
| LOGOFF | 5.4 only (ADR-0484) — no fields. From an open session, SUCCESS `{}` and back to the AUTHENTICATION state, where a fresh LOGON reopens the session; engine state (an open transaction, buffered results) is left as it stands. The failed state IGNOREs it. |
| TELEMETRY | 5.4 only (ADR-0484) — one field, `api: Int`. The server collects nothing — the HELLO hint already said `telemetry.enabled: false` — and every in-range `api` (0-3) answers SUCCESS `{}` with nothing recorded. An out-of-range Int fails `Neo.DatabaseError.General.UnknownError`, a non-Int `api` `Neo.ClientError.Request.Invalid`; both leave the failed state, so RESET recovers. |

A message whose structure signature is not in the negotiated version's
vocabulary — the ten shared signatures at 4.4, thirteen at 5.4, where
LOGON, LOGOFF and TELEMETRY join (their signatures stay unknown at 4.4,
like the Bolt 1-2 ACK_FAILURE `0x0E` at either) — or a message that is
not a structure, is a fatal decode error: the server sends nothing, logs
one stderr line and closes the connection (ADR-0315). Before a validated
HELLO with auth on, the ADR-0126 gate answers it first, with the
Unauthorized FAILURE.

Not supported: reactive, the Bolt 5.7+ manifest handshake (decided
against, ADR-0484 — see [Compatibility](#compatibility)), byte arrays as
graph properties (statement-scoped only, ADR-0327), UTF-8 identifier quirks.

### Namespace selection (ADR-0117)

Every store has the fixed namespaces `neo4j`, `system` and
`system_ontologies` (ADR-0116). Neo4j drivers select one through session or
query configuration (`session(database=…)`, `execute_query(database_=…)`),
which Bolt 4.x sends as `db` in the `BEGIN` extra and in the auto-commit
`RUN` extra:

- For each auto-commit `RUN` and each `BEGIN`, the session namespace is the
  extra's `db`, or `neo4j` when `db` is absent, `null` or `""`. It does not
  carry over from an earlier request: the next `RUN` without `db` is back in
  `neo4j`.
- A `RUN` inside an explicit transaction carries no `db`, per the Bolt
  specification, and runs in the namespace chosen at `BEGIN`.
- An unknown `db` is a FAILURE with code
  `Neo.ClientError.Database.DatabaseNotFound` (ADR-0307) and the live CE
  message `Graph not found: <name>` (ADR-0146; live CE wires GQLSTATUS
  42001 with a 42N00 cause).
- The SUCCESS that ends a result (the last `PULL`, or `DISCARD`) includes
  `db`, the namespace the query ran in. `HELLO` carries no `db`. A `ROUTE`
  that names no database answers the home database `neo4j` (ADR-0318), so
  `neo4j://` drivers resolve `neo4j` as home.

See the [namespace example](examples/namespace.md).

### Query type (ADR-0162)

The SUCCESS that ends a result (the last PULL, or DISCARD) carries a `type`
key with the statement's Neo4j Query API v2 `queryType` code — `"r"`
(read), `"w"` (a write with no returned columns, e.g. `MATCH … SET` without
RETURN), `"rw"` (a write with returned columns, e.g. `CREATE (n) RETURN n`)
or `"s"` (schema DDL), the split per ADR-0332 — the key
Neo4j drivers read for the result summary's query type. RUN SUCCESS does not
carry it (ADR-0309). The engine derives it from the logical
plan before execution, so `EXPLAIN` and `PROFILE` report the **wrapped**
statement's type: `EXPLAIN CREATE (n)` answers `"w"` without executing
anything. The value set is exactly those four codes. Bolt stays a transport
(ADR-0057): this is one summary key beside the ADR-0117 §7 `db` echo — a
separate summary key, not the plan tree (see
[Structured plan](#structured-plan-adr-0344-0345-adr-0357)).

See the [query-type example](examples/query_type.md).

### Column types (ADR-0224)

The RUN SUCCESS carries a `columnTypes` key beside `fields`: the
engine-stamped per-column type names, one string per `fields` entry, in
column order — the same 18-name set, under the same key, the wire envelope
carries as `columnTypes` and the FFI exposes per column (`STRING`,
`INTEGER`, `FLOAT`, `BOOLEAN`, `NODE`, `RELATIONSHIP`, `PATH`, `LIST`,
`MAP`, `POINT`, `DATETIME`, `DATE`, `TIME`, `DURATION`, `VECTOR`, `UUID`,
`BINARY`, `ANY`). This is a **Relate extension key**, sent beside the
standard Bolt `t_first` (ADR-0309); drivers ignore keys they do not know. A
column whose first-row value is `Null`, or a result with no rows (including
a statement whose execution failure is reported at PULL, ADR-0319), stamps
`ANY`; the type describes the first row only,
because Cypher columns are heterogeneous. A statement with no result
columns carries an empty `columnTypes` list. The names are
pinned to the engine stamp by the protocol test
`run_success_carries_the_engine_column_types`
(`surfaces/relate-bolt/src/protocol_tests.rs`), which runs the same
statement through the engine and the server and asserts the two agree —
the client-integration-test example for this key.

### Structured plan (ADR-0344/0345, ADR-0357)

The **final PULL SUCCESS** summary of an `EXPLAIN` statement carries its
structured plan under `plan`, a `PROFILE` statement under `profile` — the
Neo4j protocol's two summary keys, in Neo4j's placement. An `EXPLAIN`
statement carries **no result rows** — no columns, no records — so the
plan rides the summary alone; a `PROFILE` statement's records are the
wrapped statement's own, with the plan beside them. The plan rides the
**drained result's** summary (a `DISCARD` SUCCESS carries it too), not the
RUN SUCCESS, so clients that read only the pull summary (cypher-shell)
render it. The summary is a dict in the Neo4j structured-plan field
shape: `operatorType` (string), `args` (map: `Id` the pre-order operator
number, `Details` the one-line details string, `EstimatedRows` when the
estimate phase filled it — a float, Neo4j's form — and on PROFILE trees the
metrics set `Rows` + `TimeNs` + `DbHits` + `MemoryBytes` +
`ExpressionBuiltBytes`, always together;
when admission produced a receipt the same args also carry `PeakLogicalBytes`
(ADR-0501); on a `MATCH` operator `PatternEngine` names the store's
`RELATE_PATTERN_ENGINE` mode, and a PROFILE tree's served `MATCH` clause
also carries `KernelRung` — `chain` (with `KernelHops`), `hub-star`,
`frontier-nfa`, or `walker` — the ADR-0538 traversal-rung fact),
`identifiers` (list of strings, possibly empty), `children` (list of child
dicts, absent when the operator has none), and `warnings` (list of warning
names — `EstimationMiss`, `CartesianProduct`, `AllNodesScan`,
`LabelDisjunctionScan` — on warned
nodes, absent otherwise).

The plan root's `args` additionally carry the Neo4j-facing args
(ADR-0357): `string-representation` — the plan text rendered at this
boundary through `relate-formatter` (ADR-0359: the engine emits the
tree only, never rendered text), headed by the
`Cypher`/`Planner`/`Runtime` lines tooling parses (the planner and runtime
named honestly as `relate`, never Neo4j's `COST`/`SLOTTED`) and
showing the same boxed operator table (`+---+` grid) the CLI's table mode
prints — plus
`version` (the Cypher language version, `5`), `planner`/`planner-impl`/
`planner-version`, `runtime`/`runtime-impl`/`runtime-version`, and `Memory`
(the root's row-materialization bytes, mirrored under Neo4j's arg name).
The engine's own wire form stays underneath; the one decoration
(`PlanNode::to_neo4j_value`) lives on the engine, and every surface calls
it at its boundary, so the field shape the HTTP Query API envelope
(`queryPlan`/`profiledQueryPlan`), the CLI's machine modes (`{"plan": …}`)
and the FFI carry cannot drift. The operator vocabulary is the engine's
one set of names (`ProduceResults`, `Expand`, `NodeByLabelScan`,
`AllNodesScan`, …) carried bare — `operatorType` adds no suffix here —
including the index-seek starts (ADR-0486): `NodeIndexSeek` for an
anchored-equality point read on a range-indexed `(label, property)` pair,
`NodeIndexSeekByRange` for a comparison window, and `NodeByLabelSeek`
for a per-row keyed lookup (LABS-1083: an equality predicate whose bound
is a row expression, or a label disjunction's per-arm union — the
`Details` string reading `RANGE INDEX <var>:<Label>(<prop>)`, one target
per arm, with no count of its own), the `Details` string of the counted
seeks reading `RANGE INDEX <var>:<Label>(<prop>) WHERE <prop> <op> <bound>` and
`EstimatedRows` carrying the seek's own count. A plain run (no
`EXPLAIN`/`PROFILE` preamble) carries neither key. A statement whose plan
tree nests past
packstream's container depth limit (128) cannot pack: the summary then
carries `planTruncated: true` instead of the key (ADR-0353) — the depth
limit is this boundary's, so the marker is a Bolt-transport fact no other
surface carries; the statement's own rows are unaffected. The
field shape is
pinned by the protocol test `pull_success_carries_the_structured_plan`
(`surfaces/relate-bolt/src/protocol_result_metadata_tests.rs`), the
client-integration-test example for this key; the truncation marker by
`pull_success_marks_a_depth_truncated_plan` beside it, and the seek
operators by `an_index_backed_start_narrates_the_seek_on_the_bolt_wire`
beside those.

### Notifications (ADR-0363)

The SUCCESS that ends a result — the final PULL, or a DISCARD that drains it
— carries a `notifications` list when the statement has any. Like the plan
pair, the list rides the **ended result's** summary, never RUN SUCCESS, so a
client that reads only the drained summary sees it. The key is **omitted
when the list is empty**; a notification-free statement's ending summary is
unchanged. Each entry is a map with exactly five keys, in this order:

| key | value |
| --- | --- |
| `code` | the notification code, e.g. `Neo.ClientNotification.Statement.CartesianProduct` |
| `title` | the canonical human title |
| `description` | the diagnosis with the remedy appended as ` (remedy: <remedy>)` |
| `severity` | `INFORMATION` or `WARNING` |
| `category` | `PERFORMANCE`, `UNRECOGNIZED`, `DEPRECATION`, `SCHEMA` or `HINT` |

There is no `position` key — the front-end retains no source spans — and no
separate `remedy` key: the action is folded into `description`, the same
shape the HTTP Query API v2 carries and the drivers' summary model renders.
The entries and their text come from the engine's one notification
vocabulary (`relate_model::wire`) through its Neo4j-shaped serializer;
Bolt adds only the wire key order. A plain run derives no notifications
(ADR-0363 decision 3), and `EXPLAIN`/`PROFILE` carry the notifications of
the wrapped statement beside its plan. The shape is pinned by the protocol
tests `pull_success_carries_the_neo4j_shaped_notifications`,
`discard_success_carries_the_notifications` and
`statements_without_notifications_omit_the_key`
(`surfaces/relate-bolt/src/protocol_result_metadata_tests.rs`); the
official driver's `summary.notifications` is exercised by
`tests/python_driver_smoke.py`.

At 5.4 the Bolt 5.2 filter keys configure that list (ADR-0484 decision 8):
`notifications_minimum_severity` (String — `INFORMATION`, `WARNING` or
`OFF`) and `notifications_disabled_categories` (List<String>) are accepted
in the HELLO, BEGIN and auto-commit RUN extra. A statement's keys override
the HELLO's, and a `null` value falls back to it: entries below the named
minimum severity and entries in a listed category drop from the ending
summary, and `OFF` drops the whole list. Category entries are free
strings — a filter naming one of Neo4j's wider set filters nothing rather
than failing a legal driver value. A wrong-typed value or an unknown
severity fails `Neo.ClientError.Request.Invalid`. Pinned by
`surfaces/relate-bolt/tests/compat/scripts/protocol/notifications-filter.bolt`.

### Parameter value decoding (ADR-0177)

A RUN's second field is a PackStream map of parameters. Scalars — `Null`,
`Bool`, `Int`, `Float` and `String` — decode to themselves, `Bytes` decodes
to the engine's statement-scoped byte array (ADR-0328, see [Byte
arrays](#byte-arrays-adr-0327-adr-0328)), and `List` and `Map` decode
recursively; every map key, at any depth, must be a string. Nine struct
tags decode to engine values on every connection, so Bolt 4.4 drivers can
send temporal, duration and point parameters instead of having them
silently become `null`. The two DateTime rows depend on the
[`utc` patch](#patch_bolt-utc-adr-0329) — without it the legacy `0x46`/`0x66`
decode, with it the UTC `0x49`/`0x69` do — and on the version: at 5.x the
UTC pair decodes unconditionally and the legacy pair never does (ADR-0484
decision 7):

| Tag | Bolt struct | Cypher type |
| --- | --- | --- |
| `0x44` | Date `[days: Int]` | `date` |
| `0x54` | Time `[nanoseconds: Int, tz_offset_seconds: Int]` | `time` |
| `0x74` | LocalTime `[nanoseconds: Int]` | `localtime` |
| `0x46` | DateTime (legacy, 4.4 without the utc patch) `[seconds: Int, nanoseconds: Int, tz_offset_seconds: Int]` | `datetime` (offset form) |
| `0x66` | DateTimeZoneId (legacy, 4.4 without the utc patch) `[seconds: Int, nanoseconds: Int, tz_id: String]` | `datetime` (zone form) |
| `0x49` | DateTimeUTC (utc patch at 4.4, every 5.x connection) `[seconds: Int, nanoseconds: Int, tz_offset_seconds: Int]` | `datetime` (offset form) |
| `0x69` | DateTimeZoneIdUTC (utc patch at 4.4, every 5.x connection) `[seconds: Int, nanoseconds: Int, tz_id: String]` | `datetime` (zone form) |
| `0x64` | LocalDateTime `[seconds: Int, nanoseconds: Int]` | `localdatetime` |
| `0x45` | Duration `[months: Int, days: Int, seconds: Int, nanoseconds: Int]` | `duration` |
| `0x58` | Point2D `[srid: Int, x: Float, y: Float]` | `point` (2D) |
| `0x59` | Point3D `[srid: Int, x: Float, y: Float, z: Float]` | `point` (3D) |
| `CC`/`CD`/`CE` (marker, not a struct) | Bytes `[8/16/32-bit length, bytes]` | byte array (statement-scoped; ADR-0328) |

`0x46` carries local epoch seconds plus an offset, so it decodes with
`zone: None` — the same representation as `datetime('...+01:00')` — and a
zero offset renders as `Z`. `0x66` carries the wall clock in `tz_id`: a
unique wall time resolves to its offset, a DST overlap to the earliest
occurrence, and a DST gap is rejected. `0x49` carries UTC epoch seconds and
decodes with `zone: None`; `0x69` carries the UTC instant, so `tz_id`'s
offset at that instant is never ambiguous and only an unknown `tz_id` is
rejected. Point coordinates are stored verbatim; the struct names the SRID,
so `point()`'s map-construction rules do not re-run.

A value the engine cannot represent fails the RUN — it is never silently
dropped. The parameter conversion returns `Err` for every struct tag
outside the table (a DateTime tag the connection does not use counts as
outside: `0x49`/`0x69` without the utc patch at 4.4, `0x46`/`0x66` with
it, and the legacy pair on every 5.x connection, ADR-0484 decision 7); a
wrong field count or field type; an
out-of-range component (a temporal `nanoseconds` beyond its layout bound,
an epoch/`days` outside chrono's range, or a `tz_offset_seconds` that does
not fit `i32`); a point SRID outside `4326`/`4979`/`7203`/`9157` or one
that does not match the tag's dimension; an unknown `tz_id`; a `0x66` wall
time in a DST gap; and a non-string map key at any depth. The RUN envelope
rejects, one level above, a params field that is present but not a map (an
absent one stays the empty map), as is a non-string key of the top-level
params map.

A rejected RUN sends FAILURE with code `Neo.ClientError.Request.Invalid`
(ADR-0307), sets the failed state, does not execute the query, and PULL is IGNORED until
RESET. The same policy rejects the RUN envelope itself: a statement field
that is absent or not a string, and a params field that is present but not
a map (an absent one stays the empty map), as is a non-string key of the
top-level params map (ADR-0342). Each message names the offending value by
parameter path: `$` is the params field itself, `$name` a top-level
parameter, `.key` a map entry and `[i]` a list element (for example `$p`,
`$p.x`, `$p.xs[0]`). `{kind}` is the Pack variant name — `Null`, `Bool`,
`Int`, `Float`, `String`, `List`, `Map`, `Bytes` or `Struct(0xNN)` (two
uppercase hex digits). The templates:

| Case | Message template |
| --- | --- |
| unsupported struct tag | `unsupported parameter value for {path}: Struct(0x{tag:02X})` |
| wrong field count | `invalid parameter structure for {path}: Struct(0x{tag:02X}) has {count} fields, expected {expected}` |
| wrong field type | `invalid parameter structure for {path}: field {index} of Struct(0x{tag:02X}) must be {expected}, got {kind}` |
| field out of range | `invalid parameter structure for {path}: field {index} of Struct(0x{tag:02X}) is out of range: {value}` |
| unknown point SRID | `invalid parameter structure for {path}: unknown srid {srid}: expected one of 4326, 4979, 7203, 9157` |
| point SRID/dimension mismatch | `invalid parameter structure for {path}: srid {srid} does not match the coordinate dimension of Struct(0x{tag:02X})` |
| unknown `tz_id` | `invalid parameter structure for {path}: unknown timezone '{tz_id}'` |
| DST gap | `invalid parameter structure for {path}: ambiguous local time {wall} in timezone '{tz_id}'` |
| non-string map key | `unsupported parameter map key for {path}: {kind} (map keys must be strings)` |
| params not a map | `unsupported parameter value for {path}: {kind} (RUN params must be a map)` |

Results use the inverse encodings: a temporal, point or byte-array
parameter comes back as the same PackStream value (see [Result value struct
encoding](#result-value-struct-encoding-adr-0272-adr-0325-adr-0326)).

See the [temporal parameters example](examples/temporal_params.md).

Cypher: same engine as the C ABI. Neo4j-specific clauses fail as syntax or
semantic errors. Write-write conflict is a FAILURE with message
`write-write conflict` under the transient code
`Neo.TransientError.Transaction.Outdated`, so drivers' managed transactions
retry it. It is reported at the PULL or DISCARD of the conflicting
auto-commit statement ([Execution failures](#execution-failures-adr-0319));
the server itself does not retry (ADR-0017, ADR-0307). A statement that
references a parameter missing from the RUN parameters fails the RUN with
`Neo.ClientError.Statement.ParameterMissing` and the message
`semantic error: ParameterMissing: Expected parameter(s): <names>`
(ADR-0308): nothing executes, no RECORD with `null` is sent, PULL is IGNORED
and RESET recovers. A parameter supplied as `null` stays `null`.

`RUN` queries are subject to the ADR-0257 input limits: query text of at
most 4 MiB (4 194 304 bytes) and nesting depth of at most 500. Violations
fail as syntax errors naming the exceeded limit.

Those limits bound static shape; nesting multiplies dynamically. Per
ADR-0395, every top-level statement also carries a **nested evaluation
budget of 100 000 nested clause-engine part passes**. The counting rule: per
top-level statement, the engine counts one nested clause-engine part pass
for every nested body run — each `CALL { … }` body once per imported row
**per UNION part of the body** (every `CALL … IN TRANSACTIONS` row
included), each taken conditional branch body once, each
`EXISTS { … }` / `COUNT { … }` / `COLLECT { … }` subquery once per outer
row, each `FOREACH` clause list once per list item per row, and each
chained NEXT-stage statement once — and a statement whose next pass would
take the count past 100 000 fails instead of running that pass. A wide body
costs its part count, so the rows × parts product lands on the budget, not
just the row axis. There is no knob: a statement whose legitimate nested
fan would exceed the budget must split into separate top-level statements,
each of which gets its own. Exceeding it is an execution failure reported at the
first PULL or DISCARD of that result ([Execution
failures](#execution-failures-adr-0319)) with code
`Neo.ClientError.Statement.SemanticError` — the `Execution` mapping in
[Failure codes](#failure-codes-adr-0307) — and the engine's message
verbatim:

```text
NestedEvaluationBudgetExceeded: nested evaluation exceeds 100000 passes per statement; nested subqueries compound per level (security budget).
```

Runtime values are capped independently at **500 container levels**
(ADR-0462): a dynamically built list/map nested past that — a `reduce`
accumulator that wraps itself each iteration is the canonical shape — fails
with a typed execution error whose message begins
`ValueDepthExceeded: value nesting exceeds 500 levels`. It maps to
`Neo.ClientError.Statement.SemanticError` through the same `Execution`
mapping and is reported at PULL/DISCARD like the budget error. Exactly 500
levels are accepted; the limit is fixed. Separately, PackStream's own
128-container wire limit still refuses a deeper **result encoding** with a
typed `DepthExceeded` failure (the protocol's 128-level container limit,
ADR-0110), so an engine value between 129 and 500 levels is a typed protocol
refusal, not an abort.

Incoming chunked Bolt messages have a **16 MiB (16,777,216-byte) total
payload cap**, accumulated across all chunks, not a per-chunk allowance.
A chunk that would exceed it is rejected before that chunk is allocated or
read; the server closes only that connection. The per-connection framing
buffer is bounded at **16 MiB plus one 64 KiB read**: a client that keeps
a message incomplete while streaming past that bound — before or after
HELLO — has its connection closed the same way. A partial message resumes
at its last parsed chunk, so a trickled message is parsed once per new
byte, not re-parsed from its start on every read. An empty chunk (`00 00`)
where a message would start is a Bolt 4.1+ NOOP: the server skips it, any
number may precede or separate messages, and it counts as activity for the
idle timeout (ADR-0316). The server likewise sends NOOPs as keep-alives
during long requests ([HELLO](#hello-adr-0293-adr-0312-adr-0313)), so a raw
client must skip them between messages. Malformed PackStream input
also terminates its connection without aborting the server; other clients
can keep connecting. PackStream decoding is bounded to 128 nested levels and
262,144 total decoded values per incoming message (including containers,
map keys, and map values; ADR-0082). A dense message beyond the decoded-value
limit similarly terminates only its connection.

Per ADR-0259 and ADR-0504, every `RUN` query executes on a 64 MiB stack:
the served connection's own thread carries that budget explicitly
(ADR-0233), and the session asserts it to the engine, so a statement whose
plan is bounded READ work (point and joined reads, catalog commands)
runs directly on that thread — no handoff to the engine's per-connection
query worker. Every write clause and every data-proportional or
unbounded-row shape (variable-length and quantified paths, cartesian
multi-pattern MATCH, `UNWIND`, `CALL {}`, procedures, `LOAD CSV`, pattern
expressions) escalates to that same 64 MiB worker,
whose wait keeps the NOOP keep-alive (ADR-0312) ticking for the requests
that can outlive its interval. Either way, oversized or over-nested
queries fail as typed `FAILURE` messages, never as a server process abort;
the sole assumption is that the process can spawn 64 MiB-stack threads for
its connections. A thread spawn failure or an engine-internal panic
likewise surfaces as a typed `FAILURE`, never as an abort. A panic
leaves that connection usable for the next statement.

### Failure codes (ADR-0307)

Every FAILURE carries a Neo4j status code, so drivers classify it as Neo4j
would: the classification segment decides whether a managed transaction
retries, and the category picks the exception type. The message is the
engine's own text, verbatim. An engine error maps by its typed
`relate::Error` variant:

| Engine variant | Code |
| --- | --- |
| `Syntax` (parse error, ADR-0257 input limit; an undefined variable, ADR-0335; the traversal-depth admission limit, LABS-610/ADR-0466; every rejection the statement text decides — a literal argument of the wrong type or outside its domain, a literal that cannot parse, call arity, a statically non-boolean predicate, ADR-0566) | `Neo.ClientError.Statement.SyntaxError` |
| `ProcedureNotFound` (a call to an unregistered procedure, ADR-0138) | `Neo.ClientError.Procedure.ProcedureNotFound` |
| `ParameterMissing` (ADR-0308) | `Neo.ClientError.Statement.ParameterMissing` |
| `Type`, `TypeError`, `InvalidArgumentType` (a type fault only a per-row value reveals, ADR-0566) | `Neo.ClientError.Statement.TypeError` |
| `ArgumentError` (a DDL `OPTIONS`/`indexConfig` value rejection, ADR-0489) | `Neo.ClientError.General.InvalidArguments` |
| `ArgumentValue` (a run-time function-argument value rejection — `range()`'s zero step, the `substring()`/`left()`/`right()` range checks, list-slice bounds, `point()`'s in-map values, `bar()`'s width, the percentile aggregates' argument, the vector similarity/distance and `vector()` dimension faults, `round()`'s precision/mode checks, `replace()`'s limit, the `trim()` single-character rule and a zero `CALL … IN TRANSACTIONS` batch size; ADR-0559, ADR-0565) | `Neo.ClientError.Statement.ArgumentError` |
| `Arithmetic` (`/ by zero`, `% by zero`, `ArithmeticError: rounding necessary`) | `Neo.ClientError.Statement.ArithmeticError` |
| `DatabaseNotFound` (unknown `db` extra or `USE` target) | `Neo.ClientError.Database.DatabaseNotFound` |
| `UniquenessViolation` | `Neo.ClientError.Schema.ConstraintValidationFailed` |
| `Cancelled` (`--query-timeout-secs` expired) | `Neo.ClientError.Transaction.TransactionTimedOut` |
| `Conflict` (`write-write conflict`) | `Neo.TransientError.Transaction.Outdated` (retryable) |
| `Capacity` — the `--logical-graph-size-limit` cap (`capacity exceeded: used=… limit=…`), the per-allocation 256 MiB expression cap (ADR-0463, plain-language detail line), or the per-record 64 MiB WAL payload cap (ADR-0494, plain-language detail line naming the record and the cap) | `Neo.ClientError.General.TransactionOutOfMemoryError` |
| every other `Semantic` and `Execution` | `Neo.ClientError.Statement.SemanticError` |
| `Internal`, `Storage`, `Io`, `Locked` | `Neo.DatabaseError.General.UnknownError`, message `internal error` |

The server's own failures:

| Condition | Code |
| --- | --- |
| A parameter the server cannot convert (ADR-0177) | `Neo.ClientError.Request.Invalid` |
| A RUN whose statement field is absent or not a string (ADR-0342) | `Neo.ClientError.Request.Invalid` |
| A PULL or DISCARD whose `n` or `qid` is present but not an Int (ADR-0342) | `Neo.ClientError.Request.Invalid` |
| A PULL or DISCARD whose `n` is present and outside `-1` and the non-negative Ints (ADR-0342) | `Neo.ClientError.Request.Invalid` |
| A HELLO without a non-empty string `user_agent` (ADR-0313, ADR-0342), or — at 5.4 — without a valid `bolt_agent` map (ADR-0484) | `Neo.ClientError.Request.Invalid` |
| A protocol breach ([Protocol breaches](#protocol-breaches-adr-0314)) | `Neo.ClientError.Request.Invalid` |
| A PULL or DISCARD whose `qid` names no open result (ADR-0311) | `Neo.ClientError.Request.Invalid` |
| A result over `--max-result-bytes` (`query result exceeds max-result-bytes limit of <N> bytes`) | `Neo.ClientError.General.TransactionOutOfMemoryError` |
| A failed or missing auth token — in the HELLO at 4.4, in LOGON at 5.4 (ADR-0126, ADR-0484) | `Neo.ClientError.Security.Unauthorized` |

A message that is not a request the negotiated version speaks (not a
structure, or an unknown signature) gets no FAILURE: the server closes the
connection (ADR-0315).
Where an engine error's FAILURE arrives — at the RUN, or at the first PULL
or DISCARD of its result — is decided separately
([Execution failures](#execution-failures-adr-0319)); the code is the same.

Both memory conditions use `TransactionOutOfMemoryError`, the one Neo4j
memory code in the non-retryable `ClientError` class: a retry cannot free
memory in an embedded store. They differ by message. The per-record WAL
payload cap refusal (ADR-0494) joins them: a too-large single commit is a
memory condition the caller fixes by committing smaller transactions, not
an internal failure, so it stopped being genericized `internal error`.

Engine errors are partitioned at this trust boundary (ADR-0260). The
internal-class variants `Internal`, `Storage`, `Io`, and `Locked` are
genericized: the client receives the fixed message `internal error` under
`Neo.DatabaseError.General.UnknownError`, and the server writes the full
engine error to its own stderr (`eprintln!`) so operators keep the detail.
No path, errno text, or panic payload reaches the client. Every other
variant passes its message through verbatim; the pinned
`write-write conflict` and `capacity exceeded: …` messages are unchanged.

Listen — through the one product binary, `relate serve` (ADR-0368):

```
cargo run -p relate-cli -- serve --host 127.0.0.1 --bolt 7687 --database :memory:
cargo run -p relate-cli -- serve --bolt 7688 --database ./graph --auth none
cargo run -p relate-cli -- serve --bolt 7688 --database :memory: --bolt-identity Neo4j/5.26.0  # cypher-shell-compatible HELLO agent (ADR-0293)
cargo run -p relate-cli -- serve --help
```

The standalone `relate-bolt` binary is retired (ADR-0368). Its old flag
spellings map onto `serve`: `--uri PATH` → `--database PATH`;
`--bind HOST:PORT` → `--host HOST --bolt PORT`; `--identity` →
`--bolt-identity`; `--auth`, `--read-only`, `--max-connections`, and
`--ws-allow-origin` keep their names, and `--ws-max-connections` is the
WebSocket-only session cap (ADR-0459).
`--max-active-queries` is removed. Token admission is on when
`--concurrent-logical-workspace-limit` is set to a positive size
(ADR-0452); there is no `--token-admission` switch.
`--logical-graph-size-limit`,
`--ws-allow-origin`, and `--ws-max-connections` are `relate serve` flags
(ADR-0390, ADR-0459). Every other standalone flag
(`--load-csv-dir`, `--tls-cert`, …) has no serve spelling
yet — those capabilities are library-level only (embed `relate_bolt::serve`).

The flag table below documents the **crate-level** serving vocabulary
(`relate_bolt::serve`'s `Args`, which the e2e/compat harness drives
in-process, ADR-0368 §3) — the reference for every limit serve inherits
its defaults from. Serve's own surface is the reduced set in the CLI
spec's serve flag table.

The server opens `--uri` **before** binding or accepting connections. If a
persistent Native root, its WAL directory (including `--wal-dir`), or a `.rl`
file is already owned, it writes `storage locked: <path>` to stderr and exits
1 (ADR-0097). This is a startup error, not a Bolt `FAILURE` message; no
client connection has been established. Invalid storage and I/O failures
retain their own error messages.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--bind <ADDR>` | `127.0.0.1:7687` | Listen address |
| `--uri <URI>` | `:memory:` | Database URI (`:memory:`, path, `file://`) |
| `--read-only` | off | Open an existing database without writing, checkpointing, or creating a missing store (ADR-0136). Reads succeed; writes fail with `database is read-only`. |
| `--wal-dir <PATH>` | unset | Local WAL directory (ignored for `:memory:`) |
| `--checkpoint-interval-secs <N>` | `300` (`RELATE_CHECKPOINT_INTERVAL_SECS`) | Seconds between timed checkpoints when the WAL has work (ADR-0041). `0` disables the timer. Persistent URIs only. |
| `--logical-graph-size-limit <SIZE>` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Live graph + index cap in logical bytes (ADR-0042). `SIZE` is an integer (bytes) or an integer plus `b`/`bytes`/`mb`/`mib`/`gb`/`gib` (case-insensitive; optional space). `mb`/`gb` are SI; `mib`/`gib` are IEC. A `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` value that does not parse is rejected at startup the same way as a bad `--logical-graph-size-limit`. Growing writes are FAILURE `capacity exceeded: used=… limit=…` under `Neo.ClientError.General.TransactionOutOfMemoryError` (ADR-0307). |
| `--load-csv-dir <PATH>` | denied (`RELATE_LOAD_CSV_DIR` fallback) | Allow local `LOAD CSV` only under this canonical directory; flag wins over environment. Regular files only; default per-file read cap is 8 MiB (ADR-0081). |
| `--tls-cert <PATH>` | unset | PEM certificate chain to present to TLS clients (server cert + intermediates; requires `--tls-key`). |
| `--tls-key <PATH>` | unset | PEM private key for `--tls-cert` (requires `--tls-cert`). |
| `--tls-client-ca <PATH>` | unset | PEM CA bundle; when set, TLS clients must present a CA-signed certificate (mTLS; requires `--tls-cert` and `--tls-key`). |
| `--allow-plaintext` | off | Allow plaintext Bolt on a non-loopback bind (insecure; prints a warning). |
| `--no-websocket` | off (WebSocket served) | Disable Bolt-over-WebSocket serving (default on; ADR-0266); a WebSocket upgrade request then closes like any other unrecognized first byte. |
| `--ws-allow-origin <ORIGIN>` | auth-dependent (loopback plus `https://browser.neo4j.io` and `https://console.neo4j.io` with auth on; loopback Origins only with `--auth none`) | Allow this Origin on WebSocket upgrades (repeatable; an `http(s)://` origin URL or `*` for any Origin). The value is validated by the shared origin grammar and stored canonically (scheme and host lowercased, RFC 3986 §3.1/§3.2.2); an out-of-shape value is a usage error. Overrides the auth-dependent default: loopback plus the hosted Neo4j Browser and Console with auth on (ADR-0459; ADR-0266's any-Origin default is replaced), loopback Origins only with `--auth none`. |
| `--max-connections <N>` | unlimited (`0`; `RELATE_MAX_CONNECTIONS`) | Maximum concurrent connections in this process (ADR-0110, ADR-0333). Each accepted connection holds one permit for its lifetime; an over-cap connection is accepted then immediately closed (accept-then-close) and never queued. `0`, omit, or an unset env is unlimited. On `relate serve`, Bolt, HTTP, and MCP share this one cap. |
| `--ws-max-connections <N>` | unlimited (`0`; `RELATE_WS_MAX_CONNECTIONS`) | Opt-in cap on concurrent **WebSocket** sessions (ADR-0459), applied on top of `--max-connections`: the permit is taken before the upgrade read and held for the session, and an over-cap upgrade is accepted then immediately closed with no HTTP response. `0`, omit, or an unset env is unlimited — the default, because bulk data-transfer workloads legitimately run many parallel sessions. Available on `relate serve` as the same flag. |
| `--max-traversal-depth <N>` | unset (`RELATE_MAX_TRAVERSAL_DEPTH`, else 500) | Largest explicit traversal hop bound a statement may ask for (LABS-610, ADR-0466). Values 1 through 2000. An `*1..N` / `{1,N}` bound above it returns FAILURE `TraversalDepthExceeded` under `Neo.ClientError.Statement.SyntaxError` at RUN, before the statement plans or walks. A safety limit protecting against a stack-overflow abort, not a database limit. |
| `--safe` / `--no-safe` | off (`RELATE_SAFE_MODE=1/0`) | Opt-in safe mode for the served store (LABS-680, ADR-0477), passed to the open: a refused statement shape (a new named node without a label, or an unbounded `MATCH` read) returns FAILURE `SafeModeViolation` under `Neo.ClientError.Statement.SyntaxError` at RUN. The standalone `relate-bolt` crate's `Args` carries no flag; an embedding host opts in through `OpenOptions::safe_mode`. |
| `--concurrent-logical-workspace-limit <SIZE>` | unset (`RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`; omit leaves admission off) | Pool capacity in logical bytes (ADR-0452). A positive value turns admission on; the engine does not default a pool. `SIZE` uses the `--logical-graph-size-limit` grammar. `0` is rejected. Off, overlap is not refused and responses carry no workspace receipt fields. On, charges round up to 4096-byte quanta (`CHARGE_QUANTUM_BYTES`); a new admission reserves at least 4096 logical bytes. A new reservation must leave one eighth of the pool free (`reserved + cost + tail <= capacity`, `tail = capacity / 8`). The next reservation that does not fit returns retryable `resource limit exceeded: concurrent-logical-workspace-limit limit=<N>`. A running query may grow into that tail. A set `RELATE_TOKEN_ADMISSION` fails open. |
| `--handshake-timeout-secs <N>` | `5` | Seconds for the whole connection negotiation: one wall-clock deadline computed at accept, spanning the first-byte read, TLS handshake, post-TLS first-byte peek, WebSocket upgrade read, and Bolt magic/version exchange (ADR-0110, ADR-0141); every read in those loops re-arms the remaining budget, so a client trickling a byte per read window still dies at the deadline. After the version exchange it discharges into the idle timeout. `0` disables the bound. |
| `--idle-timeout-secs <N>` | `300` | Seconds a connection may stall waiting for the next chunk of a Bolt message before it is closed (ADR-0110). The same budget also bounds the whole window one message may take: the deadline is armed when the message read starts and re-armed per chunk, so a byte-at-a-time trickle cannot hold a message open indefinitely. `0` disables the idle timeout. |
| `--query-timeout-secs <N>` | `0` (off) | Seconds a single `RUN` query may execute before it is cancelled through the engine's `CancelToken` (ADR-0110/0111). On expiry the server sends `FAILURE` with message `query cancelled` under `Neo.ClientError.Transaction.TransactionTimedOut` (ADR-0307) and the session stays usable; `0` disables the bound. |
| `--expression-memory-limit <SIZE>` | `256 MiB` (`RELATE_EXPRESSION_MEMORY_LIMIT`; `none` or `0` disables) | Per-allocation expression cap (ADR-0463). `SIZE` uses the `--logical-graph-size-limit` grammar, plus `none` for the explicit opt-out. One value an expression builds may not exceed the cap, and a retaining container's running total is checked too; a long streaming load of many small values is not charged a statement total. Omitted or unset keeps the engine default (256 MiB). |
| `--max-result-bytes <SIZE>` | `256 MiB` (`268435456`) | Maximum retained bytes per open result before RUN returns a FAILURE naming the limit under `Neo.ClientError.General.TransactionOutOfMemoryError` (ADR-0307): the rows' `Value::logical_bytes()` plus the retained RUN statement text (ADR-0357); the result is discarded (ADR-0110). `SIZE` uses the `--logical-graph-size-limit` grammar (`mb`/`mib`/`gb`/`gib`). `0` disables the cap. |
| `--identity <STRING>` | `Relate/<version>` (`RELATE_IDENTITY`) | The HELLO SUCCESS `server` agent string (ADR-0293), sent verbatim. The honest default reports Relate; set a Neo4j-shaped value (e.g. `Neo4j/5.26.0`) for tooling like cypher-shell that hard-checks the identity. A spoofed identity may unlock tooling behaviors the server does not implement — it is a compatibility shim, not a Neo4j feature-parity claim. |
| `-h, --help` | | Show clap-generated usage and exit. |

### Result value struct encoding (ADR-0272, ADR-0325, ADR-0326)

Graph, temporal and point values are PackStream structures, both as
top-level results and at any nesting depth inside a List or Map (the
encoder recurses through the same depth accounting, so a value nested in a
collection encodes identically to one returned directly; a structure counts
as one nesting level, like a list). A byte array is PackStream Bytes
(`CC`/`CD`/`CE`, the shortest length marker; ADR-0328). Vector and UUID
results keep their string encodings (ADR-0272, CIP-246).

| Type | Tag | Fields |
| --- | --- | --- |
| Node | `0x4E` | `[id: Int, labels: List of String, properties: Map]`; at 5.x a fourth field follows, `element_id: String` |
| Relationship | `0x52` | `[id: Int, start: Int, end: Int, rel_type: String, properties: Map]`; at 5.x three more follow — `element_id: String, start_node_element_id: String, end_node_element_id: String` |
| UnboundRelationship | `0x72` | `[id: Int, rel_type: String, properties: Map]`; at 5.x a fourth field follows, `element_id: String` |
| Path | `0x50` | `[nodes: List of Node, relationships: List of UnboundRelationship, sequence: List of Int]` |
| Date | `0x44` | `[days: Int]` — days since 1970-01-01 in the proleptic Gregorian calendar (out-of-range civil dates included) |
| Time | `0x54` | `[nanoseconds: Int, tz_offset_seconds: Int]` — nanoseconds since midnight of the local time |
| LocalTime | `0x74` | `[nanoseconds: Int]` — nanoseconds since midnight |
| LocalDateTime | `0x64` | `[seconds: Int, nanoseconds: Int]` — epoch seconds of the wall clock read as UTC (floored), sub-second nanoseconds `0..=999_999_999` |
| DateTime (legacy, 4.4 without the utc patch) | `0x46` | `[seconds: Int, nanoseconds: Int, tz_offset_seconds: Int]` — UTC epoch seconds plus the offset (the local wall clock), for a datetime whose zone is an offset (no zone, `Z`, `+01:00`, …) |
| DateTimeZoneId (legacy, 4.4 without the utc patch) | `0x66` | `[seconds: Int, nanoseconds: Int, tz_id: String]` — UTC epoch seconds plus the offset, for a datetime whose zone is an IANA region |
| DateTime (utc patch at 4.4, every 5.x connection) | `0x49` | `[seconds: Int, nanoseconds: Int, tz_offset_seconds: Int]` — UTC epoch seconds, offset zones |
| DateTimeZoneId (utc patch at 4.4, every 5.x connection) | `0x69` | `[seconds: Int, nanoseconds: Int, tz_id: String]` — UTC epoch seconds, IANA regions |
| Duration | `0x45` | `[months: Int, days: Int, seconds: Int, nanoseconds: Int]` — `seconds` = total nanoseconds `div_euclid` 10^9, `nanoseconds` = `rem_euclid` 10^9 |
| Point2D | `0x58` | `[srid: Int, x: Float, y: Float]` — coordinates always Float (`13.0`, never `13`) |
| Point3D | `0x59` | `[srid: Int, x: Float, y: Float, z: Float]` |
| Bytes (byte array) | marker `CC`/`CD`/`CE` (not a struct) | 8/16/32-bit length then the bytes, the shortest marker that fits (ADR-0328) |

The 5.x `element_id` strings are the engine `elementId()` helpers' own
output (ADR-0116 §7) — `4:rl-id:n0` for the first node,
`5:rl-id:r0` for the first relationship — sourced per result
namespace; the integer `id` fields stay. The `system` namespace has no
element-id segment, so a 5.x result carrying a graph value from there
fails with the typed error `elements of this namespace have no element
ids` (the `elementId()` precedent) — never a fabricated id. Pinned by
`surfaces/relate-bolt/tests/compat/scripts/values/element-id-structs.bolt`.

Which DateTime pair a connection uses is one derivation, shared by this
encoder and the [parameter
decoders](#parameter-value-decoding-adr-0177) (ADR-0329, ADR-0484
decision 7): the `utc` patch at 4.4, or any 5.x connection — so at 5.x
the UTC pair is unconditional in both directions and the legacy
`0x46`/`0x66` never appear. Every other temporal structure is unchanged
by version. Pinned by
`surfaces/relate-bolt/tests/compat/scripts/values/fixed-datetime-structs.bolt`
beside the 4.4
`surfaces/relate-bolt/tests/compat/scripts/values/patch-bolt-utc.bolt`.

Examples (ADR-0325, ADR-0326): `date('2024-01-02')` is `0x44(19724)`;
`time('03:04:05.123456789+01:00')` is `0x54(11045123456789, 3600)`;
`datetime('2024-01-02T03:04:05.123456789+01:00')` is
`0x46(1704164645, 123456789, 3600)` (and `0x49(1704161045, 123456789, 3600)`
under the utc patch at 4.4 or on a 5.x connection);
`datetime('2024-01-02T03:04:05[Europe/Stockholm]')` is
`0x66(1704164645, 0, "Europe/Stockholm")`; `duration('P1Y2M3DT4H5M6.5S')` is
`0x45(14, 3, 14706, 500000000)`; `point({longitude: 13, latitude: 55.6})` is
`0x58(4326, 13.0, 55.6)`. Each temporal and point structure is the exact
inverse of its [parameter decoder](#parameter-value-decoding-adr-0177), so a
parameter echoes back as the same structure. A temporal component that does
not fit a PackStream Int (i64) fails the PULL instead of falling back to a
string; the engine produces such values only at its range edges.

A **Path** carries its traversal in `sequence`, with **two entries per hop**:
first the **1-based index of the relationship** the hop uses, made **negative
when the hop traverses that relationship against its start-to-end
direction**; then the **0-based index of the node arrived at**. A hop is
forward exactly when the relationship's `start` equals the node it departs
from, so the index is positive; a reverse hop is negative. Because the
sequence re-derives the traversal, the Path's relationships are the
**UnboundRelationship** (`0x72`) form — three fields at 4.4, four at 5.x
(its `element_id` appended) — and the bound `start`/`end`
endpoints are deliberately dropped.

### patch_bolt utc (ADR-0329)

A HELLO whose extra carries `patch_bolt` as a list containing the string
`"utc"` turns on the Bolt 4.4 utc patch for that connection: HELLO SUCCESS
adds `patch_bolt: ["utc"]`, and from then on DateTime values use the UTC
structures in both directions — results encode as `0x49`/`0x69` and
parameters decode from `0x49`/`0x69`, while the legacy `0x46`/`0x66`
parameters fail with the unsupported-struct message. At 5.x the same
structures apply with no patch to negotiate, and a 5.x HELLO's
`patch_bolt` is silently ignored (ADR-0484 decision 7). Only patches the
server applies are echoed: unknown names, non-string entries and a non-list
`patch_bolt` apply nothing, and with no patch applied the SUCCESS has no
`patch_bolt` key. The official Python driver sends `patch_bolt: ["utc"]` on
Bolt 4.4, so its 4.4 connections use the UTC structures; against this
server it negotiates 5.4, where the same structures apply with no patch
(ADR-0484). Every other temporal
structure is unchanged by the patch.

### Byte arrays (ADR-0327, ADR-0328)

A PackStream Bytes parameter, at any depth, becomes the engine's
statement-scoped byte array and comes back as the same PackStream Bytes:
`RETURN $b AS b` with `b = bytes 00 01 FF` answers `RECORD [bytes 00 01 FF]`,
`CC 03 00 01 FF` on the wire, and the column type is `BINARY`. Byte arrays
compare byte-wise and type as `LIST<INTEGER NOT NULL>` in Cypher. They live
only inside a statement: storing one as a node or relationship property
(CREATE, MERGE, SET), alone or in a list, fails the statement with the
typed engine `TypeError` (`type error: TypeError: byte array values cannot
be stored as node or relationship properties; they live only inside a
statement (parameter to result)`), which Bolt reports as
`Neo.ClientError.Statement.TypeError`, and nothing is written. Persistent
byte-array properties are the Linear follow-up **"Engine: persist bytes
(byte-array) values as node and relationship properties (WAL, snapshot and
Parquet/JSON checkpoint codecs)"**.

### Authentication (ADR-0126)

The server validates the auth token (`scheme`/`principal`/`credentials`
in the HELLO map at 4.4; in LOGON at 5.4, ADR-0484) when authentication is
configured:

- `--auth user:password` / `RELATE_AUTH=user:password` — a single preset
  credential; a HELLO without it, or with a mismatch, is
  `Neo.ClientError.Security.Unauthorized` and the connection closes.
- `--auth none` / `RELATE_AUTH=none` — no validation (credentials are
  ignored; the pre-ADR behavior).
- Unset — a token is generated at startup: user `relate`, a 128-bit hex
  token. It is printed in the startup banner, and for a persistent database
  persisted next to it (`<dir>.auth-token`,
  `<bundle>.rl.auth-token` (siblings, never inside the storage directory — the engine
  rejects a storage directory holding foreign files), mode 0600) and reused on restart; `:memory:`
  generates a fresh token every start. The container shows it in
  `docker logs`. Malformed `--auth`/`RELATE_AUTH` values (no colon, an
  empty side) are startup errors, never a silent fallback to no-auth.

`--auth user:password` on the command line is visible in the process
list (CWE-214) — on shared machines prefer `RELATE_AUTH` (an
environment variable) or the generated-token default, which never puts
the credential in argv.

The startup banner reports the effective mode: `auth=none | basic` plus the
source (flag/env/generated + path).

A HELLO that validates under `--auth user:password` (or the generated-token
default) also forwards the validated principal engine-ward: the server calls
[`Connection::set_principal`](../rust/spec.md#connection) on the session's
engine connection (ADR-0273), so `dbms.showCurrentUser()` reports the real
user — see the [neo4j-browser](#neo4j-browser) section below. Under
`--auth none` the HELLO principal is unverified and is never forwarded.

A failed HELLO — bad scheme, bad principal, or bad credentials — waits a
fixed **1000 ms** before the server sends the
`Neo.ClientError.Security.Unauthorized` FAILURE and closes (ADR-0139). The
delay is a compiled-in constant with no flag, applies identically on every
transport (raw TCP, TLS, `ws://`, `wss://`), and never applies to a
successful HELLO; it is a blocking sleep on the offending connection's own
thread (ADR-0233), so other
connections proceed concurrently. It bounds online guessing of an
operator-chosen preset credential to at most `--max-connections` guesses
per second (128/s at the default); the generated-token default is
unguessable at any rate this delay permits.

## Docker

The server ships as a container image (ADR-0128), whose runtime since
ADR-0249 is the unified **`relate serve`**: one process serving Bolt
(7687), the HTTP Query API v2 (7474, [HTTP spec](../http/spec.md)), and
MCP over streamable HTTP (7475, [MCP spec](../mcp/spec.md)) on one open
store with **one shared credential** (ADR-0232 §1) — the token in
`docker logs` unlocks all three surfaces. `make docker` builds it,
`make docker-run` runs it (all three ports published, data volume at
`/data`). The engine env vars above pass straight through the
container. Configuration resolves most-to-least canonically:
container args pass through to `relate serve` (`CMD` defaults
`--host 0.0.0.0 --database /data/graph --bolt 7687`; the entrypoint
appends the unset ports — http 7474, mcp 7475, or the derived fleet
bases under `INSTANCES=N`); a mounted `/config/seed.json`
(`RELATE_CONFIG` overrides the path) drives seeding with ordered steps
(`query`, `load` + `query`, `pull` + `query`; `force: true` reseeds); and
the env fallback covers one-liners (`RELATE_QUERY`,
`RELATE_LOAD` + `RELATE_LOAD_QUERY`, `RELATE_PULL_FROM` +
`RELATE_PULL_QUERY`, `RELATE_FORCE_SEED=1|true|yes`).
Auth (ADR-0232 §1) passes straight through the container:
`RELATE_AUTH` reaches the server unchanged, and container args after
the image name reach it too (`docker run image --auth user:password`).
Unset, the server generates a token and prints it in the startup banner —
in a container, `docker logs` is where to read it; it is also persisted
as a `<uri>.auth-token` sibling (on the `/data` volume for the default
URI) and reused across restarts.

`INSTANCES=N` runs a test fleet — N serve processes, every surface's
port stepping from its base (instance *i*: base+*i*−1). Unset
`--http`/`--mcp` bases derive from the bolt base so the three stepped
ranges never overlap (N=2, defaults: bolt 7687-7688, http 7689-7690, mcp
7691-7692); explicitly-set bases must be pairwise N apart, a loud
startup error otherwise. Per-instance suffixed URIs. See the Dockerfile
header and the README Docker section for examples.

The image has a container-level smoke suite:
`make docker && python3 surfaces/relate-bolt/tests/docker_smoke.py`
(boot/seed/restart with token reuse, all three surfaces answering on the
one credential, preset auth + negatives, loud bad inputs, loopback
warning, fleet isolation, `docker stop` exit 143). It
skips cleanly without a docker daemon or image;
`RELATE_DOCKER_IMAGE` overrides the tag (default
`relate-bolt:<workspace version>`). CI runs it in the `relate-bolt`
job.

The default `CMD` binds `--host 0.0.0.0`: a container must bind all
interfaces to be reachable at all, and `serve` is plaintext-only — the
loud `WARNING` it prints on a non-loopback host (ADR-0248) is the
expected posture in `docker logs` for this dev/test image, and auth
stays on by default. The ADR-0128 bolt spellings still work as aliases
(`--uri` ≡ `--database`, `--bind HOST:PORT` ≡ `--host HOST --bolt
PORT`; `--allow-plaintext` is accepted and dropped with a note — the
`--host` flag subsumes it). Bolt-only flags the unified runtime does not
take (`--tls-cert`, `--max-connections`, …) fail `serve`'s parser. The
image carries `relate` and `entrypoint` only (ADR-0368) — the
`--entrypoint /bin/relate-bolt` TLS alternative is gone; for
encrypted Bolt, front the container with a TLS-terminating proxy (serve
has no TLS).

### `entrypoint` binary

The image's `ENTRYPOINT` is a dedicated binary (scratch has no shell);
it consumes no flags of its own — every argument after the image name
replaces the `CMD` and goes to `relate serve` (it reads the store
and the bind plan to plan seeding and fleet ports). When argv omits
them, the entrypoint appends the image defaults (`--database
/data/graph`, `--host 0.0.0.0`, `--bolt 7687`, `--http 7474`, `--mcp
7475`) so the seeded database is the one served and the bind stays
reachable; serve's own loopback/no-defaults posture never leaks into the
container. The legacy bolt spellings translate (`--uri`,
`--bind HOST:PORT`, `--allow-plaintext` dropped with a note); mixing a
spelling with its alias (`--database` + `--uri`, `--host`/`--bolt` +
`--bind`) is a loud startup error. `--read-only` (ADR-0306, the ADR-0480
spelling) passes through to `serve` valueless — the bare form takes no
value and never consumes the next argument, and the attached
`--read-only=live` splits and forwards as one token — so
`--read-only=live` alone starts a following server on the default
database; the removed `--follow` spelling still forwards as one bare
token for the server's unknown-flag error to name. Usage: `docker run
relate-bolt[:tag] [serve flags]` (default `--host 0.0.0.0
--database /data/graph --bolt 7687 --http 7474 --mcp 7475`). It is
configured entirely by environment:

| Env var | Default | Meaning |
| --- | --- | --- |
| `RELATE_CONFIG` | `/config/seed.json` | Seed config path; an explicitly-set but unreadable/invalid path is a startup error (unknown fields rejected), a missing one falls back to env seeding |
| `RELATE_QUERY` | unset | Env fallback: one Cypher seed statement |
| `RELATE_LOAD` | unset | Env fallback: `NAME=SOURCE` for the CLI `--load` flag (needs a build with that flag; without it the container exits with an actionable error). Requires `RELATE_LOAD_QUERY` |
| `RELATE_LOAD_QUERY` | unset | Query consuming `$NAME` from `RELATE_LOAD` |
| `RELATE_PULL_FROM` | unset | Env fallback: pull env file for `relate pull --from` (ADR-0080). Requires `RELATE_PULL_QUERY` |
| `RELATE_PULL_QUERY` | unset | Pull shape query for `RELATE_PULL_FROM` |
| `RELATE_FORCE_SEED` | unset | `1`/`true`/`yes` reseeds an existing database; `0`/`false`/`no` accepted; anything else is a startup error. `:memory:` always seeds |
| `INSTANCES` | `1` | Fleet size `1..=65535`; all three surface ports step from their bases (`--bolt`/`--http`/`--mcp`; unset `--http`/`--mcp` derive from the bolt base as B+N and B+2N so the stepped ranges never overlap), persistent URIs get `-N` suffixes. PID 1 handles SIGINT/SIGTERM and kills every child |

Seeding runs once per database URI, before the server starts (never
against a live server); a failing seed step stops the container. The
startup banner echoes `uri`, the pass-through args, the config source,
and the seed plan. Engine env vars (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`,
`RELATE_DURABILITY_MODE`, `RELATE_CHECKPOINT_INTERVAL_SECS`,
`RELATE_LOAD_CSV_DIR`) and `RELATE_AUTH` pass through untouched.

### TLS and plaintext policy

TLS is **library-level only** since ADR-0368: no product binary exposes
`--tls-cert`/`--tls-key`/`--tls-client-ca`. The TLS transport itself is
unchanged (embed `relate_bolt::serve` with the cert/key pair to use
it); for a product deployment, front `relate serve` with a
TLS-terminating proxy and let the proxy speak plaintext Bolt to serve's
loopback bind.

TLS and plaintext share one `--bind` address (ADR-0109). On each accepted
connection the server peeks the first byte before choosing a path:

- `0x16` (TLS `ClientHello` record) → run the TLS handshake, then sniff the
  first decrypted byte: `0x47` → WebSocket upgrade over TLS (`wss://`),
  `0x60` → Bolt over TLS, anything else → close.
- `0x60` (Bolt magic preamble) → plaintext Bolt.
- `0x47` (`G` of `GET`) → WebSocket upgrade (`ws://`), under the same
  plaintext policy as raw Bolt.
- anything else → close that connection.

TLS is enabled by `--tls-cert` together with `--tls-key` (the pair is
required; `--tls-client-ca` additionally requires both). Certificates and keys
are read once at startup and reload only on **process restart**; cipher suites
and TLS versions are rustls defaults, with no knobs. Drivers connect with
`bolt+s://` to verify the server certificate against the system trust store,
or `bolt+ssc://` to accept a self-signed server certificate. Both schemes use
the same port, so no second listener is needed.

TLS and mTLS servers are started through the library entry point
(`relate_bolt::serve` with `--tls-cert server.pem --tls-key
server.key`, plus `--tls-client-ca client-ca.pem` for mTLS) — there is no
product-binary command for them (ADR-0368; see the
[TLS example](examples/tls.md)).

Plaintext policy:

- A loopback bind — every resolved IP is loopback — serves plaintext silently;
  an empty or non-loopback resolution is treated as non-loopback.
- A non-loopback bind with TLS unconfigured and no `--allow-plaintext` refuses
  to start, writing `refusing to serve plaintext Bolt on non-loopback bind …`
  to stderr and exiting 1.
- `--allow-plaintext` on a non-loopback bind starts and writes
  `WARNING: serving plaintext Bolt on non-loopback bind …; traffic is
  unencrypted (--allow-plaintext)` to stderr — whether or not TLS is
  configured.
- With TLS configured, a non-loopback connection that takes the plaintext path
  is refused per connection unless `--allow-plaintext` is set, writing
  `refusing plaintext Bolt on a non-loopback bind without --allow-plaintext`
  and closing only that connection; the startup line reports `tls=on`.

See the [Bolt TLS example](examples/tls.md).

### WebSocket (ADR-0266)

Bolt over WebSocket (RFC 6455) is served on the same bind by default —
serve's Bolt surface keeps WebSocket serving on (ADR-0368). The opt-out
flag `--no-websocket` is **library-level only** (no product-binary
spelling; embed `relate_bolt::serve`); `--ws-allow-origin` and
`--ws-max-connections` are `relate serve` flags (ADR-0459), so
browser clients (the official JavaScript driver's browser build,
neo4j-browser) can connect: `ws://` on the plaintext path, `wss://` after the
post-TLS sniff. Inbound binary message payloads concatenate into the Bolt
byte stream; each outbound Bolt write maps to one binary WebSocket message.
HELLO auth (ADR-0126) runs unchanged over the WebSocket transport, and
WebSocket connections count against `--max-connections` and the
`--handshake-timeout-secs` bound like every other connection.

- `--no-websocket` opts out; a WebSocket upgrade request then closes like
  any other unrecognized first byte.
- Origin policy: a missing `Origin` header (a non-browser client) is always
  allowed; a present `Origin` that is not visible ASCII fails closed with
  `HTTP/1.1 403 Forbidden`. The policy is resolved once from
  `--ws-allow-origin` and the auth mode: a `*` anywhere in the list allows
  any Origin; any other non-empty list allows the Origins that
  canonicalize to one of its entries (each entry is parsed by the same
  origin grammar as `serve --origin` and stored canonically, so scheme and
  host case is insignificant); an empty list falls back to the
  auth-dependent default — with auth on, loopback Origins **plus the hosted
  Neo4j Browser (`https://browser.neo4j.io`) and Neo4j Console
  (`https://console.neo4j.io`)**, the same hosted set the HTTP surface's
  default allows; with `--auth none`, loopback Origins only (the
  cross-site WebSocket hijacking guard). An arbitrary Origin needs an
  explicit `--ws-allow-origin` even with auth on (ADR-0459; this replaces
  ADR-0266's any-Origin default, since a weak preset password would
  otherwise let a malicious page attempt authentication from the victim's
  browser).
  Loopback-only accepts an `http`/`https` Origin (the scheme is
  case-insensitive, RFC 3986 §3.1) with no userinfo, no query, no
  fragment, no path beyond `/`, an empty or numeric port, and a host of
  exactly `localhost`, `127.0.0.1`, or `[::1]` — bracketed non-IPv6
  hosts are rejected and `127/8` is not widened. Rejection logs carry
  only an `escape_debug`-escaped excerpt of the Origin capped at 128
  characters, never the raw value.
- A request on the `0x47` path that is not a valid WebSocket upgrade gets
  `HTTP/1.1 400 Bad Request`; a valid upgrade with a rejected Origin gets
  `HTTP/1.1 403 Forbidden`; both close the connection.
- A text frame closes the connection with close code 1003 (unsupported
  data); pings are answered with pongs automatically, with at most one
  pong ever outstanding — a ping flood from a client that never reads
  stalls on backpressure instead of growing an unbounded write buffer.
  An inbound Close frame is answered and flushed before EOF, and after a
  Bolt session ends (e.g. GOODBYE) the server drives a WebSocket close
  handshake under its own fresh handshake-timeout budget before closing.
- Caps: the HTTP upgrade request (request line plus headers) is bounded at
  **8192 bytes**; a WebSocket frame declaring more than **1048576 bytes**
  (1 MiB) is rejected before its payload is buffered; a reassembled
  WebSocket message is bounded at **16777216 bytes** (16 MiB) — the same
  value as the Bolt message cap — and the WebSocket path can hold that
  reassembled message plus the Bolt payload it decodes into.
- Concurrency: `--ws-max-connections N` (ADR-0459) is an opt-in cap on
  concurrent WebSocket sessions, default unlimited. The permit is taken
  before the upgrade read, so an over-cap upgrade is closed without a
  response, and it is held until the session ends; the accept-time
  `--max-connections` cap still bounds every connection independently.
  Time is bounded without this knob: the ADR-0141 handshake deadline
  bounds the upgrade and negotiation, and the ADR-0110 idle deadline —
  re-armed per message — bounds a stalled or trickled session.

The startup banner line reports the effective mode as `ws=on|off`:

```
Relate Bolt listening on 127.0.0.1:7687 (uri=..., tls=..., auth=..., ws=on)
```

See the [Bolt over WebSocket example](examples/websocket.md).

### neo4j-browser

neo4j-browser and the official JavaScript driver's browser build speak Bolt
over WebSocket, so they connect to the same `--bind` with no extra port:

- An **http-hosted** Browser (self-hosted, or Neo4j Desktop) dials
  `bolt://host:port`, which the driver browser channel maps to `ws://`, and
  works out of the box against the default plaintext loopback posture.
- An **https-hosted** Browser (e.g. `https://browser.graphapp.io`) dials
  `wss://` — the browser channel picks `wss://` when the page is served over
  HTTPS — so the server needs `--tls-cert`/`--tls-key` with a
  **browser-trusted certificate**; a self-signed one is refused by the
  browser's WebSocket stack.
- The browser's default `neo4j://` scheme also works: after HELLO the driver
  sends ROUTE, and the server answers with the ADR-0269 single-server self
  routing table, so no scheme change is needed.

Two Bolt-layer behaviors were fixed for browser compatibility:

- **ROUTE self routing table (ADR-0269, ADR-0318):** ROUTE (`0x66`)
  returns SUCCESS with `rt: {servers: [{addresses: [<self>], role}` for
  WRITE, READ and ROUTE, in that order`], ttl: 300, db: <requested db or
  "neo4j">}`. The browser's connect message names `db: "system"` and gets
  `system` back.
- **ProcedureNotFound FAILURE code (ADR-0138):** a call to an unregistered
  procedure is sent under `Neo.ClientError.Procedure.ProcedureNotFound`, so
  the browser's `dbms.clientConfig` → `dbms.listConfig` fallback fires.

Engine-side, `dbms.components` returns three rows with the `edition` column
(ADR-0142): `{name: 'Neo4j Kernel', versions: ['2026.10-relate'],
edition: 'community'}` first (row order is load-bearing — Neo4j
Desktop finds the version row *by name* while the public web Browser's
version gate reads only the first record's `versions[0]`), then `{name:
'Cypher', versions: ['5', '25'], edition: 'community'}`, then `{name:
'Relate', versions: [<engine version>], edition: 'community'}`. The
`Neo4j Kernel` version is the ADR-0152 **policy constant**
`2026.10-relate` — the Neo4j calver month the Browser compat surface
was verified against, plus the `-relate` product suffix (Aura's
`5.27-aura` convention), bumped deliberately rather than derived from the
workspace version. browser.neo4j.io's sdk minimum-version gate (`>=
4.4.0`, loose-coerced semver) reads only this row and throws on anything
lower, blocking the Database information sidebar
([LABS-150](https://linear.app/neo4j/issue/LABS-150)); the suffixed calver
passes it while staying distinguishable from any real Neo4j release. The
Bolt HELLO server agent is unchanged — it stays `Relate/<engine
version>`, since the gate never reads it (ADR-0152). Neo4j Desktop's own
`4.4.0` gate reads the same row, so its former **non-fatal** "not
officially supported" warning no longer fires. `dbms.info()` (ADR-0142) answers Desktop's
`updateDBMSInfo` call with one row `{id: 'relate', name: 'system',
creationDate: '1970-01-01T00:00:00Z'}` (`creationDate` is a documented
placeholder; the engine tracks no creation time). This resolves
[LABS-139](https://linear.app/neo4j/issue/LABS-139). SHOW DATABASES rows
carry concrete values instead of nulls (ADR-0142), in both the `neo4j` and
`system` namespaces (ADR-0116): `access` `'read-write'`, `address`
`'localhost:7687'` (ADR-0142; Desktop builds the instance id `name@address`
and rejects an empty address), `role` `'primary'`,
`requestedStatus`/`currentStatus` `'online'`, `statusMessage` `''`, `writer`
`true`, and `default`/`home` `true` for `neo4j` and `false` for `system`.
These are fixed constants, not live status — Relate is a single-node
embedded engine — and they exist so Neo4j Desktop's record codec accepts the
rows and lets its embedded Browser connect.

The four remaining `dbms.*` procedures the browser calls during or after
connect are implemented engine-side (ADR-0143); all but
`dbms.showCurrentUser` return fixed rows per the ADR-0142
documented-placeholder precedent — the engine carries no session identity
beyond the ADR-0273 principal, so these are documented constants, not live
state:

- `dbms.showCurrentUser()` returns one row `{username: <principal or
  'relate'>, roles: ['admin'], flags: []}` (ADR-0273). After a HELLO
  validates under `--auth user:pw` (or `RELATE_AUTH=user:pw`), the
  server sets the validated principal on the engine connection and
  `username` is that user; under the generated-token default the same path
  reports `relate` — now an honest read, not a placeholder. Under
  `--auth none` the HELLO principal is unverified and is **never**
  forwarded, so `username` stays the documented placeholder `'relate'`
  (byte-identical to ADR-0143). `roles: ['admin']` is a documented
  placeholder that matches the Browser's own missing-`roles` fallback and
  conveys the engine's actual authorization model (there is none);
  `flags: []` is honest — Relate has no user flags.
- `dbms.clientConfig()` returns the 8-column `ConfigResult` shape (`name`,
  `description`, `value`, `dynamic`, `defaultValue`, `startupValue`,
  `explicitlySet`, `validValues`) with one row per key of the Neo4j 5.x+
  default allowlist: `browser.allow_outgoing_connections: 'true'`,
  `browser.credential_timeout: '0'`, `browser.post_connect_cmd: ''`,
  `browser.remote_content_hostname_whitelist: '*'`,
  `browser.retain_connection_credentials: 'false'`,
  `browser.retain_editor_history: 'true'`, `client.allow_telemetry:
  'false'`, `dbms.security.auth_enabled: 'true'`, `server.metrics.prefix:
  'relate'`. On every row `dynamic: false`, `defaultValue` and
  `startupValue` equal `value`, `explicitlySet: false`, and `validValues:
  ''`. All rows are documented placeholders except
  `server.metrics.prefix: 'relate'` and `client.allow_telemetry:
  'false'`, which are honest (no Neo4j-style metrics namespace; telemetry
  off unless opted in). `dbms.security.auth_enabled: 'true'` is accurate for
  the ADR-0126 default and cosmetic-only wrong under `--auth none`. The
  Browser reads only `name`/`value`; the remaining six columns keep the
  Neo4j 5.x+ documented shape for other consumers.
- `dbms.listConfig()` returns the same 8-column shape and the same nine rows
  as `dbms.clientConfig`, with no input arguments — Relate has no
  general settings registry, so the documented scope is the Browser-relevant
  subset. The Browser only ever calls it as the
  `ProcedureNotFound`-keyed fallback (ADR-0138) and post-processes it
  identically.
- `dbms.licenseAgreementDetails()` returns one row `{status: 'yes',
  daysLeftOnTrial: null, totalTrialDays: null}` — an honest constant:
  Relate is Apache-2.0 (ADR-0055), so there is no license agreement to
  accept and no trial. No current client ever calls it — the Browser guards
  on server-reported `edition === 'enterprise'` and ADR-0142 reports
  `community` on every `dbms.components` row — so the procedure is defensive
  completeness for future or non-Browser clients, not a Browser requirement.

This resolves
[LABS-148](https://linear.app/neo4j/issue/LABS-148) (consolidating
[LABS-140](https://linear.app/neo4j/issue/LABS-140),
[LABS-141](https://linear.app/neo4j/issue/LABS-141),
[LABS-142](https://linear.app/neo4j/issue/LABS-142), and
[LABS-143](https://linear.app/neo4j/issue/LABS-143)); together with
ADR-0142 this closes the Browser/Desktop compatibility work stream.

### Connection cap and rejection (ADR-0110)

The accept loop tracks `--max-connections` (unlimited unless a positive
value or `RELATE_MAX_CONNECTIONS` is set; ADR-0333) with an atomic
counter (ADR-0233). Every accepted connection takes one slot for its
entire lifetime and releases it when the connection closes. `relate
serve` uses one cap for Bolt, HTTP, and MCP together.

Over-cap connections are handled **accept-then-close**: the server still
`accept()`s the socket, sees the cap reached, logs
`bolt: --max-connections <N> reached; closing connection` to stderr, and closes the
socket immediately. Over-cap clients are rejected, not queued, so a client can
never wait in the TCP backlog. As soon as an existing connection disconnects
and releases its slot, the next connection is served. There is no Bolt
FAILURE: the cap is enforced before a session exists.

`--ws-max-connections N` (ADR-0459) is the opt-in WebSocket-only sibling:
unlimited by default, it bounds concurrent WebSocket sessions in addition to
the process cap. Its permit is taken when a connection classifies as a
WebSocket upgrade, before the upgrade read; over cap logs
`bolt: --ws-max-connections <N> reached; closing connection` and closes with
no HTTP response, and the slot is released when the session ends.

`accept()` errors — including resource exhaustion such as `EMFILE` — are
logged to stderr (`accept error: …; retrying`) and retried after a short
backoff. They never exit the server process, and other connections keep being
served.

Run a server capped at one connection at a time:

```
cargo run -p relate-cli -- serve --host 127.0.0.1 --bolt 7687 --database :memory: --max-connections 1
```

### Timeouts (ADR-0110, ADR-0141)

The three flags below are **library-level only** since ADR-0368: serve
carries none of them (embed `relate_bolt::serve` to set them); the
defaults apply on every serve-run Bolt surface.

- **Handshake timeout** (default 5s, `--handshake-timeout-secs`): one deadline
  is computed when a connection is accepted and spans every negotiation phase
  — the first-byte read, the TLS handshake, the post-TLS first-byte peek, the
  WebSocket upgrade read, and the Bolt magic/version exchange (ADR-0141).
  A client that never sends the handshake magic, stalls partway through any
  phase, or trickles one byte per read window (every loop read re-arms the
  remaining budget) is disconnected when the deadline elapses: on expiry a phase-specific
  line is written to stderr and only that connection closes. After the version
  exchange the deadline discharges into the idle timeout, and the post-session
  WebSocket close handshake runs under its own fresh handshake-timeout budget.
  `0` disables the bound.
- **Idle timeout** (default 300s, `--idle-timeout-secs`, `0` disables): bounds
  each blocking read, armed as the socket read deadline (ADR-0233), AND the
  whole window one message may take: the deadline is armed when a message's
  read starts and re-armed before every chunk read, so a client trickling one
  byte per read window is disconnected at the deadline rather than holding the
  message open indefinitely. A connection that stalls mid-message — for
  example, declaring a chunk length and then sending fewer bytes — is
  disconnected instead of pinning its thread indefinitely.
- **Query timeout** (default `0`, off, `--query-timeout-secs`): bounds one
  `RUN` query. When set, the query runs on the connection's own thread
  against the engine's `CancelToken` (ADR-0111) while a watchdog thread
  sleeps the budget (ADR-0233). On
  expiry the watchdog calls `CancelToken::cancel`; the engine observes the
  cancellation, returns its typed error (`Cancelled`) promptly, and the
  server sends `FAILURE` with message `query cancelled` under
  `Neo.ClientError.Transaction.TransactionTimedOut` (ADR-0307) — at the
  first PULL or DISCARD of that statement, since cancellation happens while
  the planned statement executes (ADR-0319). The connection — and its engine
  `Connection` — survive, so the session stays usable after a `RESET`.
  `BEGIN`/`COMMIT`/`ROLLBACK` are not bounded; the timeout applies to `RUN`
  only.
- **Keep-alive** (60 s, not configurable from the command line): HELLO
  SUCCESS advertises `hints: {"connection.recv_timeout_seconds": 120}`,
  twice the interval, and while a RUN's execution or a COMMIT has gone one
  interval without its reply the server writes a NOOP chunk (`00 00`) and
  repeats it every interval until the reply is written (ADR-0312). NOOPs go
  only between messages. Embedders set the interval with
  `ConnLimits::keep_alive_interval` (the hint follows it); a zero interval
  is rejected before any engine call (`Heartbeat::every`, ADR-0334) and
  closes the connection instead of ticking without waiting.

### Results and summaries (ADR-0309, ADR-0311)

`RUN` executes the statement and buffers its result (ADR-0110); true
executor streaming remains deferred per ADR-0082. In auto-commit there is
one open result: a new `RUN` replaces it. Inside an explicit transaction
every `RUN` opens its own result under a `qid` — 0 for the first RUN after
BEGIN, one more per RUN — and a later RUN never discards an earlier one.
`PULL {n, qid}` and `DISCARD {n, qid}` address the result with that `qid`;
`qid` absent or `-1` means the most recent RUN's result, and a `qid` or `n`
that is present but malformed — not an Int, or an `n` outside `-1` and the
non-negative Ints — is a FAILURE `Neo.ClientError.Request.Invalid`
(ADR-0342). A `qid` that names
no open result, while others are open, is a FAILURE
`Neo.ClientError.Request.Invalid` (`no open result with qid <qid>`) and the
failed state. COMMIT, ROLLBACK and RESET drop every result still open.

Each PULL sends **at most `n` RECORDs** of its result (`n = -1` drains);
DISCARD drops at most `n` rows the same way. The `-1` defaults apply only
when the key is absent: a present `n` outside `-1` and the non-negative
Ints (e.g. `-5`) fails before anything is consumed. The SUCCESS that
follows:

- **A page that leaves rows** is exactly `{has_more: true}`.
- **The SUCCESS that ends the result** carries `type` (`"r"`, `"w"`, `"rw"`
  or `"s"`, see [Query type](#query-type-adr-0162)), `t_first` (whole
  milliseconds until the buffered result was available — Neo4j sends it on
  the drained summary too, so a client that reads only that one,
  cypher-shell, renders its `Time` from here), `t_last` (whole milliseconds
  spent streaming or discarding the result, over all its PULL and DISCARD
  messages), the phase split `parsing_time`, `planning_time` and
  `plan_execution_time` (each a PackStream **Float** holding **seconds**,
  beside `t_last` — the statement's always-on PARSE / PLAN / EXECUTE split
  of the time `t_first` reports, so the three sum to at most that total;
  ADR-0491), the fine sub-split of the same family (LABS-1120) —
  `lex_parse_time` / `rewrite_time` / `lower_time` (sub-windows of
  `parsing_time`; zero on a plan-cache hit, which `plan_cache` explains)
  and `result_generation_time` (a sub-window of `plan_execution_time`;
  zero on `EXPLAIN`, whose render is itself its result generation), each
  Float seconds — the plan-cache outcome `plan_cache` (a String:
  `miss` / `hit-raw-fastpath` / `hit-template` /
  `replan-generation-mismatch` / `bypassed`) and `db` (the namespace it
  ran in). It adds `stats` when any
  counter is non-zero, `bookmark` after an auto-commit statement that
  committed a write ([Bookmarks](#bookmarks-adr-0310)), the query `notifications` when the
  statement has any ([Notifications](#notifications-adr-0363)), and the
  `EXPLAIN`/`PROFILE` plan under
  `plan`/`profile` ([Structured plan](#structured-plan-adr-0344-0345-adr-0357),
  ADR-0357). RUN SUCCESS never carries the phase keys — its ADR-0309
  contract (`fields`, `columnTypes`, `t_first`, `qid`) is unchanged. A read
  ended by DISCARD answers `{type: "r", t_first: <ms>, t_last: 0,
  parsing_time: <secs>, planning_time: <secs>, plan_execution_time: <secs>,
  lex_parse_time: <secs>, rewrite_time: <secs>, lower_time: <secs>,
  result_generation_time: <secs>, plan_cache: "<outcome>",
  db: "neo4j"}` — the `t_*` keys stay integer milliseconds and the phase
  keys Float seconds, never mixed units.
- **`stats`** holds Neo4j's kebab-case counters, only the non-zero ones:
  `nodes-created`, `nodes-deleted`, `relationships-created`,
  `relationships-deleted`, `properties-set`, `labels-added`,
  `labels-removed`, `indexes-added`, `indexes-removed`, `constraints-added`,
  `constraints-removed`, plus `contains-updates: true`. A statement with no
  changes (every read) sends no `stats`. The counts are Neo4j's: labels per
  node, one `properties-set` per property a `CREATE`, `SET` or `REMOVE`
  writes (a delete counts none), index and constraint DDL in the schema
  counters (an `IF [NOT] EXISTS` no-op counts 0; a `UNIQUE` constraint
  counts no index). Example: `CREATE (a:P {k: 1})-[:R]->(b:P) SET b.k = 2`
  ends with `{stats: {nodes-created: 2, relationships-created: 1,
  properties-set: 2, labels-added: 2, contains-updates: true}, type: "w",
  …}`; `CREATE INDEX` ends with `{stats: {indexes-added: 1,
  contains-updates: true}, type: "s", …}`.

### Bookmarks (ADR-0310)

The SUCCESS that ends an auto-commit result whose statement committed a
write, and the COMMIT SUCCESS of a transaction that wrote, carry
`bookmark: "relate:<n>"`. `n` is the engine's commit sequence
(ADR-0502 §7) — the same value `Connection::commit` /
`QueryResult::bookmark` and the C ABI's `relate_conn_commit` /
`relate_result_bookmark` report for that commit — monotonically
increasing per store across every connection and surface, so a Bolt
bookmark sorts with commits made through any other writer. Nothing
committed, nothing fabricated: a read, a write that changed nothing, and
an `EXPLAIN` end without a bookmark, and the COMMIT of a transaction that
wrote nothing answers `{}`. Inside an explicit transaction no PULL or
DISCARD SUCCESS carries a bookmark; ROLLBACK SUCCESS is `{}`. Bookmarks a
client sends in the BEGIN or RUN `bookmarks` list are accepted and ignored:
this is one embedded store, and every committed transaction is visible to
the next statement on any connection, so every bookmark is already
satisfied.

### Execution failures (ADR-0319)

A failure raised while executing an already planned statement — a runtime
error such as `10 / 0`, a constraint violation, an auto-commit write-write
conflict, `capacity exceeded`, query cancellation — is reported at the first
PULL or DISCARD of that result, as Neo4j does. The RUN answers SUCCESS with
the planned `fields`, `columnTypes` `"ANY"` per column and `t_first` (plus
`qid` in a transaction); the PULL or DISCARD answers the FAILURE with no
RECORDs, and the session enters the failed state (IGNORED until RESET).
Failures before execution still fail the RUN: parse, semantic validation,
planning, an unknown procedure, a missing parameter, an unknown `db`, the
[parameter conversion](#parameter-value-decoding-adr-0177) and the
[result byte cap](#result-byte-cap-adr-0110). Inside an explicit transaction
the statement's failure also fails the transaction, so a RUN or COMMIT sent
before that PULL fails with `transaction failed; rollback required`.

### Protocol breaches (ADR-0314)

These requests are invalid in the session's state and are detected before
any engine call: COMMIT or ROLLBACK with no open transaction, BEGIN inside
one, and PULL or DISCARD with no open result. Each answers exactly one
FAILURE `{code: "Neo.ClientError.Request.Invalid", message: "Message of
type <Name> cannot be handled by a session in the <STATE> state."}` and the
server closes the connection without reading further. `<Name>` is Neo4j's
message class (`CommitMessage`, `RollbackMessage`, `BeginMessage`,
`PullMessage`, `DiscardMessage`); `<STATE>` is `READY` (no transaction, no
open result), `STREAMING` (an auto-commit result open), `IN_TRANSACTION` or
`TX_STREAMING` (a transaction with open results). In the failed state these
messages answer IGNORED instead — the failed-state gate runs first and
covers RUN, PULL, DISCARD, BEGIN, COMMIT and ROLLBACK.

### RESET priority (ADR-0317)

The server frames every complete message it has already received before it
handles the next one. When those messages include a RESET, each message
queued ahead of it that has not started answers IGNORED (no RECORDs), the
RESET answers SUCCESS (rolling back an open transaction, clearing the
failed state and the open results), and the messages after it run
normally. One write of `RUN 'UNWIND range(1, 3) AS x RETURN x'`,
`PULL {n: -1}`, `RESET`, `RUN 'RETURN 2 AS y'`, `PULL {n: -1}` answers
IGNORED, IGNORED, SUCCESS `{}`, then SUCCESS `{fields: [y]}`, RECORD `[2]`
and SUCCESS. HELLO and GOODBYE are never interrupted, and a request already
executing when RESET arrives runs to completion.

### HELLO (ADR-0293, ADR-0312, ADR-0313)

A HELLO whose extra map has no non-empty string `user_agent` (absent,
`null`, `""` or not a string) is rejected before the auth check with a
FAILURE whose code is
`Neo.ClientError.Request.Invalid` and whose message is
`Illegal value for field "user_agent": Expected value to be non-null`, and
no session opens. At 5.4 a HELLO whose `bolt_agent` is not a map of string
keys and string values (absent included) is rejected the same way, with
`Illegal value for field "bolt_agent": Must be a map with string keys and
string values.` (ADR-0484); the rejected-HELLO sequence below is the same.
Every following message except HELLO and GOODBYE answers IGNORED; a second
HELLO answers FAILURE `Neo.ClientError.Request.Invalid` (`Message of type
HelloMessage cannot be handled by a session in the AUTHENTICATION state.`)
and the server closes; GOODBYE closes. Official drivers always send
`user_agent`.

HELLO SUCCESS carries `hints: {"connection.recv_timeout_seconds": 120}`:
drivers treat a connection silent for 120 s as dead, and the server keeps
every request it is working on alive with NOOP chunks every 60 s (see
[Timeouts](#timeouts-adr-0110-adr-0141)). At 5.4 the hints map adds
`telemetry.enabled: false` (ADR-0484 decision 5): the server collects
nothing, and the hint tells drivers not to send `TELEMETRY`.

### Result byte cap (ADR-0110)

`--max-result-bytes` is **library-level only** since ADR-0368 (no serve
flag; embed `relate_bolt::serve` to change the 256 MiB default).

When a `RUN` query finishes, the server checks its result against
`--max-result-bytes` (default 256 MiB) on the connection's own thread, in
the same thread as the query. The cap bounds the sum of every result open on
the connection (ADR-0311): inside an explicit transaction a RUN whose result
would push the open results past the cap fails. The check adds
up `Value::logical_bytes()` row by row and stops as soon as the running total
exceeds the limit; it does not walk the rest of the result. An over-cap
result is dropped on that thread, and the server sends a Bolt `FAILURE` whose
message names the limit
(`query result exceeds max-result-bytes limit of <N> bytes`), under
`Neo.ClientError.General.TransactionOutOfMemoryError` (ADR-0307); no rows are
buffered for `PULL`. `0` disables the cap. The cap is a logical-byte measure,
not a wire-byte measure.

Each open result also reserves the RUN statement text it keeps for the
ending summary's plan pair (ADR-0357): the statement's byte length is
counted beside the rows, in the buffered and the execution-failed arm
alike, so a transaction full of huge statements cannot park unbounded text
outside the cap.

Stopping early matters because rows can share one payload (ADR-0170): 20,000
rows that each return the same 100,000-element vector use little memory but
count about 16 GB of logical bytes, and they fail without that total being
computed.

Environment: `RELATE_LOAD_CSV_DIR` opts in when the flag is absent;
without either setting local `LOAD CSV` is denied. The flag overrides the
environment. Server-side LOAD CSV (`--load-csv-dir` and the env var) is
**library-level only** since ADR-0368 — no product binary exposes it; embed
`relate_bolt::serve` to serve a store with a LOAD CSV root
(see the [Bolt client example](examples/load_csv.md)).
`RELATE_CHECKPOINT_INTERVAL_SECS` (default 300; `0` off)
and `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` (`0` or absent means unlimited) are the other
database-open fallbacks. With the root configured, relative paths and local
`file:///` paths must resolve to regular files within it; traversal, symlink
escapes, other URLs and unavailable files return a generic Bolt FAILURE
`LOAD CSV file unavailable`, and a file over 8,388,608 bytes returns
`LOAD CSV file exceeds byte limit`. No path or OS error is included.
Sources are memoized per statement.

## Compatibility

Compatibility is probed, not assumed: the wire probe
`scripts/bolt_compat_probe.py` compared every driver-visible behavior
with the latest Neo4j — **Neo4j 2026.09.0 (Community), at Bolt 4.4** —
and a one-time neo4rs 0.9.0-rc.10 driver probe recorded the same
behaviors through the neo4rs public API. Together they produced 125
findings (`F-01`…`F-68` wire, `D-01`…`D-57` driver): 50 compatible, 73
fixed (ADR-0307 to ADR-0319, ADR-0325 to ADR-0329, ADR-0332), 1
deferred (F-12), and F-06 — the Bolt 5.7+ manifest handshake — decided
against by ADR-0484. A third probe, a live Docker probe against the same
Neo4j at **Bolt 5.4** (2026-11-02), pinned the 5.4 surface ADR-0484
decides; its findings are that ADR's numbered decisions, each with its
covering script. Every finding's behavior, status and covering test are
tabulated in [`compatibility.md`](compatibility.md), this
section's second page — which also records the
[shared-backend interop rules](compatibility.md#shared-backend-interop-rules-labs-595-to-598)
(LABS-595 to 598) for one client backend serving both Relate and a
Neo4j 5.x; the official Python driver smoke
(`tests/python_driver_smoke.py`) asserts the driver-visible fixes, at
both negotiated versions, and the Go driver smoke
(`tests/go_driver_smoke`) pins the unmodified neo4j-go-driver config
against the 4.4 path (LABS-598).
Re-run the wire probe and the `.bolt` corpus:

```text
docker run -d --rm --name bolt-probe-neo4j -p 127.0.0.1:17687:7687 -e NEO4J_AUTH=none neo4j:latest
git clone https://github.com/neo4j-drivers/boltkit /tmp/boltkit && git -C /tmp/boltkit checkout 687a47973c0cfb14f7f2789a4b9b494cbe938a46
cargo run -p relate-cli -- serve --host 127.0.0.1 --bolt 17688 --database :memory: --auth none &
python3 scripts/bolt_compat_probe.py --boltkit /tmp/boltkit --neo4j 127.0.0.1:17687 --relate 127.0.0.1:17688 --json findings.json
cargo nextest run -p relate-bolt --test bolt_compat
```
