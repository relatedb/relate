# ADR-0502: The typed C ABI v2 — a native value walker, one error shape carried with the call, the JSON paths removed

Status: Accepted
Claim: c4b4fc2e9a81072e0a244df9199688d72b5830d0

## Context

LABS-447. The C ABI (`surfaces/cypherlite-ffi`) is the contract every
language client wraps (AGENTS.md §2: "clients wrap C ABI only"). Below the
top level of a result it is JSON-only: a cell crosses as
`cypherlite_value_json`, an untyped wire encoding, and a failure crosses as
the string in the thread-local `cypherlite_last_error`. Three driver-provider
spikes — the stock Neo4j Java, Python and JavaScript drivers run in-process
against the engine — measured what that costs
(`docs/design/embedded-driver-provider-analysis.md`, the raw reports in
`docs/design/embedded-driver-provider-findings.md`):

- **Values are ambiguous, and the ambiguity is not fixable inside JSON.**
  A user map carrying the entity tag's `kind` key (ADR-0223) is
  indistinguishable from an entity and mis-hydrates as a NODE — the tag
  is user-collidable; a nested PATH
  arrives as a LIST; a temporal nested in a list, map or entity property
  arrives as an ISO STRING; durations survive only at the top level (and
  `PT36H` vs `P1DT12H` is lost wherever they pass through a seconds total);
  VECTOR arrives as a map; NaN/±Inf survive only at the top level. The Java
  and JS legs (both on the JSON ABI) share the same gaps row for row; the
  Python leg, on the typed pyo3 binding, is correct at every depth for all
  of these except the binding-level duration/vector/element-id gaps.
- **Every client repairs it with hacks.** The ADR-0224 first-row column
  stamp is used to re-type values; `kind`/`srid` keys are sniffed;
  element ids are synthesised or re-spelled to match Cypher's
  `elementId()`; bookmarks are minted by the provider. All three spikes
  agree: do not ship a provider with these hacks.
- **Errors carry no classification, and that loses writes.** Without a
  Neo4j status code crossing the boundary, the driver's `execute_write`
  does not retry a write-write conflict and the user's write is silently
  lost (measured on two legs: final value 100 / 10 against the correct
  101 / 11). Each spike carries a second, lossy copy of the Bolt surface's
  `failures::error_class` table — message-sniffing in Java/JS, a kind→code
  map in Python — to get retries back. The repo already holds three drifting
  copies of the classification (Bolt `failures::error_class`,
  `cypherlite::binding_error_metadata`, CLI `errors::classify`); the
  correctness-harness waves pay a three-way edit for every new error
  (#607, #656, #663, #683).
- **The thread-local error slot is not async-safe.** The JS client must not
  block the event loop, so it calls through koffi's `.async` form. A
  `RETURN 1/0` through `cypherlite_query_params.async` returned rc=1 and
  `cypherlite_last_error()` read on the main thread returned `null` — the
  message stayed on the libuv worker thread.
- **Bookmarks are invented.** `try_wal_commit` already returns the commit
  timestamp each committed transaction was assigned; `Connection::commit`
  drops it, so providers mint ADR-0310-shaped bookmarks from a local
  counter.

The ABI also carries a second, older API — the JSON handle set
(`cypherlite_new/open/execute/execute_with_params/reset*/flush*/
begin_transaction/commit_transaction/rollback_transaction/
supports_transactions`) — that duplicates the tabular one (ADR-0220), plus
the `cypherlite_query` / `cypherlite_connect` aliases ADR-0220 kept "for ABI
compat". Their consumer set is the `docs/api/c` examples alone (verified).

Breaking is affordable exactly now. Every C-ABI consumer is first-party and
in-repo: the JS, Java, Go and Swift clients and the C docs examples. The
externally visible surfaces never touch the C ABI — `cypherlite-wasm` is
wasm-bindgen over the Rust API, and the website and cookbook ride the
wasm/CLI surfaces (confirmed by Michael, 2026-11-03). There are no external
callers to protect. After the first public release the ABI freezes and only
additive change applies; a break later would cost a compat story this one
does not.

## Decision

**The C ABI is replaced once, pre-public, by a typed v2: a native value
walker and a typed params builder are the only value paths, one structured
error shape travels with every fallible call, and the JSON value, JSON
params, JSON handle API, thread-local error, column-stamp and alias
surfaces are removed. `cypherlite_abi_version()` returns `2`.** Every
in-repo consumer migrates in the same change.

### 1. A native value walker replaces `cypherlite_value_json`

- **Value handles, owned by the result.** `cypherlite_value` is an opaque
  handle. `cypherlite_value_at(result, row, col)` returns a cell's root
  (NULL out of range); `cypherlite_value_list_len/_list_get`,
  `cypherlite_value_map_len/_map_key/_map_get` descend. Every value handle
  — roots and lazily minted children — is owned by its result and freed by
  `cypherlite_destroy_result`; there is no per-value free, so there is one
  free point and no leak by construction.
- **One exhaustive kind enum mirroring `CypherValue`.**
  `cypherlite_value_kind(v)` returns one of `NULL`, `BOOL`, `INT`, `FLOAT`,
  `STRING`, `NODE`, `RELATIONSHIP`, `PATH`, `LIST`, `MAP`, `POINT`, `DATE`,
  `TIME`, `LOCAL_TIME`, `DATETIME`, `LOCAL_DATETIME`, `DURATION`, `VECTOR`
  (C names `CYPHERLITE_V_*`). The FFI maps `CypherValue` to the enum with
  one exhaustive match — no `_ =>` arm — so a new engine variant fails to
  compile until it is given a kind. Discrimination is structural at every
  depth: a user map is `MAP`, an entity is `NODE`/`RELATIONSHIP`, a nested
  path is `PATH`.
- **Component accessors, by kind.** Out-of-kind access is a defined error
  (the accessor reports it), never undefined behaviour.
  - Scalars: `int64_t` int, `double` float with `is_nan`/`is_inf`,
    `uint8_t` bool, NUL-terminated UTF-8 string (result-owned).
  - Entities: `cypherlite_value_element_id`; relationships add
    `cypherlite_value_start_element_id` / `_end_element_id`; labels (a
    list), type (a string) and properties (a map) through the generic
    accessors. Paths iterate as alternating node/relationship values.
  - Temporals as components, not strings: year … nanosecond, plus
    offset-seconds and the zone id (`""` when offset-only) for `TIME` and
    `DATETIME`. ISO text is a client rendering choice, not an ABI contract.
  - Durations as four lossless integers: months, days, seconds, nanos.
  - Points: srid plus two or three doubles (2D/3D by coordinate count).
  - Vectors: a coordinate dtype tag (the engine's vector coordinate types),
    dimension, and a per-element accessor (float or int by dtype).

### 2. One error shape, carried with the call

- **The shape.** One JSON object, no C structs across the ABI:
  `{"code", "gql_status", "stage", "retryable", "message"}` — the Neo4j
  status code, the GQLSTATUS, the compile/runtime/internal stage
  (ADR-0319's split), the drivers' transient test as a boolean, and the
  engine message.
- **Single-sourced in the engine.** `cypherlite-error` gains `ErrorInfo
  { neo4j_code, gql_status, stage, retryable }` and `error_info(&Error)`,
  one exhaustive match moved from Bolt's `pub(crate)
  failures::error_class`. Bolt, HTTP, the CLI classifier, Python and the C
  ABI derive from it; the three existing tables become thin derivations.
  The move lands first, as its own change, with Bolt's failure tests
  passing byte-identically — a move, not a redesign. The move is decided
  here: it rides ADR-0502 and needs no ADR of its own.
- **Statement failures ride the result.** `cypherlite_query_params`
  returns `CYPHERLITE_OK` with an error result for a statement failure;
  `cypherlite_result_is_error(r)` tests it and
  `cypherlite_result_error_json(r)` returns the shape (NULL on a good
  result; result-owned until destroy). The handle travels back with the
  call, so the error is async-safe by construction.
- **Handle and connection failures use a trailing out-param.** Every
  fallible status call (open, connect, begin/commit/rollback, checkpoint,
  params-builder putters, …) keeps returning `CYPHERLITE_ERROR` and gains
  a trailing `char **error_json`: on failure it receives the same shape,
  caller-owned, freed with `cypherlite_free_string`; passing NULL opts out.
- **`cypherlite_last_error` is removed.** The thread-local slot goes
  entirely — one channel, one shape.

### 3. The JSON handle API, the ADR-0224 column stamp and the ADR-0220 aliases are removed

- The JSON handle API (`cypherlite_new`, `_open`, `_execute`,
  `_execute_with_params`, `_reset*`, `_flush*`, `_begin_transaction`,
  `_commit_transaction`, `_rollback_transaction`,
  `_supports_transactions`) is removed. The tabular API is the one client
  API; the `docs/api/c` examples are rewritten onto it.
- `cypherlite_result_column_type` (the ADR-0224 first-row stamp) is
  removed from the C ABI. Per-value kinds make it redundant for
  correctness, and every client use of it was a re-typing hack deleted
  with the hacks. ADR-0224's engine field, wire `columnTypes` and Python
  `description` type codes are not decided here.
- The `cypherlite_query` / `cypherlite_connect` aliases ADR-0220 kept for
  ABI compatibility are removed; the compat audience does not exist.

Kept unchanged: the tabular handle set (`database_open[_read_only]/close`,
`connect_namespace`, `conn_set_namespace`, `disconnect`,
`conn_begin/commit/rollback`, `checkpoint`) apart from the error
out-param, the ownership rules, the threading contract, panic poisoning,
`column_count/row_count/column_name`, and the result metadata accessors
(`query_type`, `namespace`, `stats`, `notifications` — already code +
gql_status + severity + remedy, ADR-0363 — `plan_json`, and the timing
accessors).

### 4. A typed params builder mirrors the walker

`cypherlite_params` is an opaque builder (`cypherlite_params_new` /
`_free`) with putters mirroring the walker's kinds: null, bool, int,
float, string; date, time, local time, datetime and local datetime from
components, each zoned form taking an offset or a zone id; duration from
(months, days, seconds, nanos); point from srid and two or three
coordinates; vector from dtype, dimension and a `double`/`int64_t` array;
and nested list/map through child builders. `cypherlite_query_params(conn,
cypher, params, &result)` takes the builder. The untyped JSON params path
is removed with the JSON value encoding: one typed encoding inbound, the
walker's mirror. Named-zone datetimes, month durations and sub-µs
temporals become bindable from every client at once.

### 5. The kind enum stays minimal

No `GEOMETRY`, `UUID` or `BINARY` values are reserved. The engine has no
such `CypherValue` kinds on this surface today (bytes cross as
`LIST<INTEGER>`, ADR-0219; UUIDs as `STRING`). Adding an enum
value after the public freeze is additive and safe — a client meeting an
unknown kind treats it as a defined, reportable case — so reserving names
now buys nothing.

### 6. Element ids keep the engine's `elementId()` spelling

`cypherlite_value_element_id` returns exactly what Cypher `elementId()`
returns for the same entity — `4:cypherlite:n0` for node 0,
`5:cypherlite:r0` for relationship 0 — at every depth, and the
relationship endpoint accessors use the same form. No new format is
specified: `WHERE elementId(n) = $id` round-trips by construction, and the
engine format is the one already observable to users.

### 7. Bookmarks are the engine's commit sequence

The commit timestamp `try_wal_commit` already assigns is threaded out of
`cypherlite::Connection` instead of dropped, spelled `cypherlite:<n>`
(ADR-0310's form). Two accessors:
`cypherlite_result_bookmark(r)` (result-owned; NULL when the statement
committed nothing) for auto-commit writes, and the explicit commit
returning the bookmark as a caller-owned string
(`char *cypherlite_conn_commit(conn, char **error_json)`, freed with
`cypherlite_free_string`). `n` is monotonic per store.

### 8. The version is 2, and the break happens once

A new symbol `cypherlite_abi_version()` returns `2` (the JSON-era ABI is
version 1 by definition; it had no version symbol). The v1 symbols listed
above disappear in the same change that adds the v2 ones, and every
in-repo consumer — the JS, Java, Go and Swift clients and the C docs
examples — migrates in that change. After the first public release the
ABI is additive-only.

## Alternatives considered

- **A tagged-JSON dialect beside or instead of `cypherlite_value_json`**
  (the analysis doc's original "opt-in tagged format"). Rejected: any tag
  key is user-collidable — a user map can carry the same key, which is the
  map-vs-entity ambiguity again one level down — and it is a second
  encoding to specify, version and keep in sync with the walker, while
  every client still pays a full serialize + parse per result. A walker
  makes discrimination structural instead of conventional.
- **Keep the JSON value path additively, beside the walker.** Rejected:
  two formats forever, each new value kind landing twice, and every
  client choosing between them; the additive form exists to protect
  external callers, and none exist. This was the spec's first draft,
  superseded by the clean-break revision.
- **A thread-local *JSON* error slot as the single handle-failure
  channel** (structured, but still read via `cypherlite_last_error`-style
  lookup). Rejected: it keeps exactly the defect the JS leg measured —
  koffi's async form runs the call on a worker thread and reads the slot
  on another — and it splits the error from the call that produced it.
  The out-param is uniform and async-safe.
- **Classify errors in the FFI (or per client).** Rejected: it would be a
  fourth copy of the table that already drifts three ways; the engine
  already computes the answer, so the classification moves into
  `cypherlite-error` and every surface derives.
- **Keep the JSON handle API as a "minimal embed" one-call form for the
  C docs.** Rejected: it duplicates the tabular API (two lifecycles, two
  transaction models) for a consumer set of docs examples only, and it
  would need its own JSON value and error encodings that this ADR
  removes. The tabular open/connect/query sequence is short enough to be
  the minimal example.
- **Keep the ADR-0224 column stamp on the C ABI.** Rejected: it describes
  the first row only, and with per-value kinds its only client uses were
  re-typing hacks — keeping it invites them back.
- **Reserve `GEOMETRY`/`UUID`/`BINARY` enum values now.** Rejected:
  values with no engine kind behind them are dead names clients must
  handle anyway; post-freeze enum growth is additive.
- **A denser element-id format, specified while breaking is allowed.**
  Rejected: a new format diverges from Cypher `elementId()`, so every
  client would need a translation for `WHERE elementId(n) = $id` — the
  exact re-spelling hack the spikes want removed. The engine format is
  already the observable one.
- **Provider-minted bookmarks (the ADR-0310 Bolt-listener counter).**
  Rejected for the ABI: a counter local to one provider is not ordered
  with commits made through another connection or surface; the engine's
  commit sequence is, and it is already computed.
- **Defer the break past the first public release.** Rejected: it then
  needs a compat window carrying both ABIs, and every client ships its
  hacks in the meantime. Pre-public, the break costs one coordinated
  in-repo migration.

## Consequences

- **One-time migration cost.** `cypherlite-ffi` removes ~15 symbols and
  adds ~30 (walker, kind enum, params builder, error channel, bookmark,
  version); the hand-maintained C header moves with it. The four clients
  migrate in the same change: Java's `Result` decode becomes a walker
  (the seam a `ValueFactory` visitor), JS likewise, Go and Swift move off
  `value_json` (Swift's `CypherValue.swift` already models typed values,
  so mostly mechanical). `docs/api/c/spec.md` and its examples are
  rewritten onto the tabular API, and each client's spec/description,
  examples and `client-<surface>.md` skill reference move in the same
  change (AGENTS.md §2). The engine-only error single-sourcing lands
  first as its own change; the provider facades (LABS-447) land after.
- **Correctness wins.** Temporals are typed at every depth, including
  inside entity properties; durations are lossless (`PT36H` ≠
  `P1DT12H`; month and sub-µs durations no longer degrade to strings);
  map-vs-entity and path-vs-list discrimination is structural; NaN/±Inf
  survive at every depth; element ids agree with `elementId()`; retries
  are real, because the retryable flag and Neo4j code cross the boundary
  (no more silently lost writes); bookmarks are real commit positions.
  Every spike hack — column-stamp re-typing, `kind`/`srid` sniffing,
  element-id re-spelling, message-sniffing classification,
  provider-minted bookmarks — has nothing left to repair.
- **Error classification has one source.** A new engine error is
  classified once in `cypherlite-error`; Bolt, HTTP, CLI, Python and the
  C ABI follow, ending the three-way edit tax.
- **Performance — expected, not claimed.** For tiny results the gain is
  small: the JS direct client measured 13–16 µs per trivial statement
  *with* JSON, against Python's 12 µs on the typed binding. For wide or
  deep results and parameter-heavy writes it may be large, because both
  serialization passes (engine → JSON, JSON → client objects) and the
  params JSON parse disappear. The figure is measured when the providers
  land, not asserted here.
- **Walker calls cross the FFI per component.** A client decoding a large
  result makes more, cheaper calls than one JSON parse; the per-call cost
  is part of what the provider-time measurement checks.
- **Out of scope.** A streaming/cursor ABI (results stay materialised;
  `fetch_size` pages a buffer — the documented caveat all three spikes
  accepted); `t_last` / consumed-after timing; the JS number codec beyond
  handing over a lossless `int64_t` (string vs BigInt is the client's
  representation choice, LABS-901); bytes and UUID scalars; Bolt adopting
  the engine commit sequence for its own bookmarks (ADR-0310's listener
  counter is unchanged here).
- **ADR-0224** keeps its engine-side decision; its C ABI accessor is
  removed by this ADR (Later references note added there). **ADR-0220**'s
  compat aliases are removed and `cypherlite_abi_version()` = 2 is the
  major-version bump its Consequences require (Later references note
  added there); its single tabular entry points stand.

## Later references

- ADR-0506: the walker gains the legacy numeric entity ids —
  `cypherlite_value_id` / `_start_id` / `_end_id`, additive scalars for
  the driver providers (§1's entity accessors and §6's element-id
  spellings are unchanged).
- ADR-0510: `cypherlite_conn_commit` returns `cypherlite_status` and
  hands the bookmark over through a `char **bookmark_out` out-param —
  §2's status convention now covers the commit call too, and a NULL
  bookmark out-param is a refused, non-committing call; §7's
  caller-owned bookmark and no-fabricated-NULL rule stand.
- ADR-0310 (revised while Proposed): Bolt adopts §7's engine commit
  sequence for its own bookmarks, retiring the listener counter this
  ADR's Consequences left out of scope.
