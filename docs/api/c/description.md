# C ABI — description

`librelate_ffi` is the only published C library and the contract every
language client wraps (JS, Java, Go, Swift — via JNA, koffi, cgo and
XCFramework). The ABI is **version 2**, the typed ABI of ADR-0502:
`relate_abi_version()` returns `2`. There is no separate
`relate_*` crate and no second API — the tabular
database/connection/result model is the whole surface.

The v2 shape (ADR-0502):

- **A typed value walker reads result cells.** `relate_value_at` hands
  back an opaque handle; `relate_value_kind` discriminates one of 18
  kinds structurally at every depth, and per-kind accessors read lossless
  components — int64 integers, temporals as year/month/…/nanosecond plus
  offset and zone, durations as four integers, points as srid and
  coordinates, vectors by coordinate dtype. No JSON below the top level, no
  `kind`-key sniffing, no first-row column stamp to re-type against.
- **A typed params builder binds parameters.** One putter per kind from the
  same components — including named-zone datetimes, month durations,
  sub-microsecond times, NaN/±Inf and full-range int64 — values that
  otherwise have to be constructed inside the statement itself.
- **One error shape travels with the call that failed.** A statement
  failure rides its result (`relate_result_is_error` /
  `relate_result_error_json`); every other failure fills the call's own
  `char **error_json` out-param. The shape — Neo4j status code, GQLSTATUS,
  stage, `retryable` — comes from the engine's one classification table, so
  a driver's retry loop reads a boolean instead of sniffing message text.
  With no thread-local slot, the channel is async-safe: a call on a worker
  thread reports its own error.
- **Bookmarks are the engine's commit sequence** (`"relate:<n>"`,
  ADR-0310): `relate_result_bookmark` after an auto-commit write,
  `relate_conn_commit`'s `bookmark_out` for an explicit transaction.

Every store has the fixed namespaces `neo4j`, `system` and
`system_ontologies` (ADR-0116). A connection's session namespace — `neo4j`
by default — is where statements without a `USE` clause run.
`relate_connect_namespace` opens a connection in a named namespace and
`relate_conn_set_namespace` switches it between transactions
(ADR-0117); the JS, Java and Go clients expose `connect(namespace)` /
`setNamespace(name)` over these two functions. `system_ontologies` is
visible only under `RELATE_PREVIEW=ontology` (ADR-0593): without the
opt-in both functions refuse it with the ordinary `Graph not found`
rejection, and `SHOW DATABASES` lists the two CE rows. See the
[compiled namespace example](examples/namespace.c).

Every good result carries metadata (ADR-0285), readable while the result
lives (result-owned; freed by `relate_destroy_result`):
`relate_result_query_type` is the statement's plan-time classification —
`"r"`, `"w"`, `"rw"` or `"s"` (ADR-0162) — so a client can tell reads from
writes without parsing Cypher, and can classify a statement without
executing it via EXPLAIN. `relate_result_namespace` is the statement's
*resolved* namespace: the `USE` target when the statement carries one,
otherwise the session namespace. `relate_result_elapsed_ms` reports the
engine-stamped statement elapsed in milliseconds (ADR-0221), and the phase
accessors `relate_result_parse_ms` / `_plan_ms` / `_execute_ms`
(ADR-0491) break that total into its PARSE / PLAN / EXECUTE windows.
Query notifications (ADR-0363) ride the same result: `EXPLAIN` (and later
`PROFILE`) derives a deterministic diagnostics list while a plain run
carries `[]` — `relate_result_notifications` returns it. See the
[compiled result-metadata example](examples/result_metadata.c), the
[notifications example](examples/notifications.c) and the
[plan example](examples/plan.c).

Compiled, runnable samples for every part of the contract — the two error
channels, a complete recursive walker, the params builder, bookmarks,
transactions, namespaces, metadata, notifications, plans — live in
[`examples/`](examples/); CI compiles and runs each one. The precise
contract is [`spec.md`](spec.md).
