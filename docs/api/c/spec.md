# C ABI — spec

Header (the contract, source of truth):
[`relate.h`](../../../surfaces/relate-ffi/include/relate.h).
Library: `librelate_ffi` (crate `relate-ffi`, builds as `staticlib`
and `cdylib`). Set `RELATE_LIB` to the shared library path for language
clients. The ABI is **version 2** — the typed ABI of ADR-0502 — and
`int relate_abi_version(void)` returns `2`. After the first public
release the ABI is additive-only: a version bump means symbols were
removed or signatures changed.

There is **one API**: the tabular database/connection pair. Result cells
cross through the typed value walker (§ The value walker), parameters
through the typed params builder (§ The params builder), and every
failure travels with the call that failed (§ Error channel). One canonical
entry point per operation (ADR-0220).

Empty URIs honor `RELATE_SNAPSHOT_FORMAT`. Persistent URIs honor
`RELATE_CHECKPOINT_INTERVAL_SECS` and `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`.

## Usage

```c
#include "relate.h"

relate_database db = NULL;
relate_connection conn = NULL;
relate_query_result r = NULL;
char *err = NULL;                       /* out-param errors: start NULL */

if (relate_database_open(":memory:", &db, &err) != RELATE_OK) { /* *db is NULL */ }
if (relate_connect_namespace(db, NULL, &conn, &err) != RELATE_OK) { /* *conn is NULL */ }
if (relate_query_params(conn, "RETURN 1 AS n", NULL, &r, &err) == RELATE_OK
        && !relate_result_is_error(r)) {
    int64_t n = 0;
    relate_value_int(relate_value_at(r, 0, 0), &n);   /* 1 */
}
relate_destroy_result(r);
relate_disconnect(conn);
relate_database_close(db);
```

The lifecycle: open a database, connect one or more connections (each in a
session namespace), run statements through `relate_query_params`, destroy
every result, disconnect every connection, close the database. Every
fallible call reports through the error channel below — there is exactly one
error shape and it travels with the call that failed.

## Error channel

One error shape, two delivery points (ADR-0502 §2). The shape is a JSON
object — no C structs cross the ABI:

```json
{"code":"Neo.ClientError.Statement.ArithmeticError","gql_status":"22G03","stage":"runtime","retryable":false,"message":"type error: / by zero"}
```

| key | type | meaning |
| --- | --- | --- |
| `code` | string | the Neo4j status code (`Neo.<Classification>.<Category>.<Title>`) |
| `gql_status` | string | the GQLSTATUS (five characters) |
| `stage` | `"compile"` \| `"runtime"` \| `"internal"` | parse/plan failure, execution failure, or a failure the engine never saw: a NULL or poisoned handle, a caught panic, an argument that failed to decode, a params-builder refusal |
| `retryable` | bool | the drivers' transient test: `true` exactly when re-running the same work may succeed |
| `message` | string | the engine's own message for a `compile`/`runtime` failure; the boundary's own for a NULL/poisoned handle or a refused argument; the fixed `internal error` for engine-internal, IO/storage/locked failures and caught panics |

The classification — `code`, `gql_status`, `stage`, `retryable` — comes from
the engine's single table (`relate::error_info`); Bolt, HTTP, the CLI,
Python and this ABI all derive from it, so a client never classifies an
error itself. Internal-class messages are genericized (ADR-0260): a
storage failure's file path or a panic payload never crosses the ABI —
`message` is the fixed `internal error` text, the same policy as Bolt's
and HTTP's boundaries. The redacted original is logged to the host's
stderr only when `RELATE_FFI_LOG=1` is set in the environment (read
once per process) — the embedding application's stderr is its channel, so
redaction is the default. Never branch on
`message` text; parse the JSON once into a
typed error and map `retryable` to the client's transient class.

Where a failure comes back:

| failure | status | where the shape is |
| --- | --- | --- |
| a statement fails (syntax, type, arithmetic, constraint, conflict, missing parameter, …) | `relate_query_params` returns **`RELATE_OK`**, `*out` is an **error result** | `relate_result_is_error(r) == 1` and `relate_result_error_json(r)` — result-owned, **do not free**; destroy the result as usual |
| `relate_query_params` itself fails (NULL/poisoned connection, caught panic, an argument that failed to decode such as invalid UTF-8, a list builder passed as params) | `RELATE_ERROR`, `*out` is NULL | the trailing `char **error_json` out-param — caller-owned, free with `relate_free_string` |
| any other fallible status call: `database_open`, `database_open_read_only`, `connect_namespace`, `conn_set_namespace`, `conn_begin`, `conn_commit`, `conn_rollback`, `checkpoint`, every params putter | `RELATE_ERROR` | the trailing `error_json` out-param — caller-owned |
| a walker accessor refuses | NULL / -1 / `RELATE_ERROR` | none — the return value is the whole report (see the walker) |
| `relate_database_close(NULL)` / `relate_disconnect(NULL)` | `RELATE_ERROR` | none — these two calls have no `error_json` out-param |

**The one trap:** a failing statement returns `RELATE_OK`. After every
`RELATE_OK` from `relate_query_params`, test
`relate_result_is_error` before reading cells.

**Opting out.** Pass NULL for an `error_json` out-param and the call reports
through its status alone.

**Initialize the out-param to NULL** before the call. v2 writes it only on
failure; a stale pointer from an earlier call must not be freed twice.

**Async safety.** There is no thread-local slot. The error belongs to the
call (out-param) or to the result handle the call returned, so a call run on
a worker thread reports its own error to whoever reads that call's outputs.

See [`examples/error_channel.c`](examples/error_channel.c).

## Ownership rules

The return type is the rule: **mutable `char *` is caller-owned,
`const char *` is owned by Relate** (result-owned until the owning
handle is destroyed).

| Handle / pointer | Owner | Free with |
| --- | --- | --- |
| `relate_database` | caller | `relate_database_close` (NULL ⇒ `RELATE_ERROR`, no detail) |
| `relate_connection` | caller | `relate_disconnect` (NULL ⇒ `RELATE_ERROR`, no detail) |
| `relate_query_result` | caller | `relate_destroy_result` (NULL is a no-op) |
| every `relate_value` handle (roots and children) | **the result** | nothing — `relate_destroy_result` frees them all; no per-value free exists |
| `char *` from `error_json` out-params | caller | `relate_free_string` |
| `char *` bookmark from `relate_conn_commit`'s `bookmark_out` | caller | `relate_free_string` |
| `const char *` from `relate_column_name`, `relate_result_error_json`, `relate_result_bookmark`, the result metadata accessors, and every string a value accessor returns | the result | nothing — valid until `relate_destroy_result`; do **not** free |
| a `relate_params` root | caller | `relate_params_free` (also frees every child; NULL and child handles are a no-op) |
| strings and arrays passed to a params putter | — | copied at put time, never retained; caller's buffers may be released immediately |

`relate_free_string` and `relate_params_free` accept NULL as a
no-op. A value handle or a result-owned string must never be touched after
the `relate_destroy_result` that owns it — copy anything you keep into
client-owned memory first.

## Symbol table

Every function the header declares. `relate_status` is
`RELATE_OK = 0` / `RELATE_ERROR = 1`.

### Version and free

| Function | Returns | Notes |
| --- | --- | --- |
| `relate_abi_version(void)` | `int` | `2` for the typed ABI |
| `relate_free_string(char *s)` | `void` | frees a caller-owned string; NULL-safe |

### Database and connection

| Function | Returns | Notes |
| --- | --- | --- |
| `relate_database_open(const char *uri, relate_database *out, char **error_json)` | status | `":memory:"`, a Native directory, or a `file://…` / `*.rl` URI; `*out` NULL on error |
| `relate_database_open_read_only(const char *uri, relate_database *out, char **error_json)` | status | opens an existing store without writing, checkpointing or creating (ADR-0136); reads succeed, writes and checkpoints fail "database is read-only" |
| `relate_database_close(relate_database db)` | status | frees the handle (also when poisoned); `RELATE_ERROR` on NULL |
| `relate_connect_namespace(relate_database db, const char *name, relate_connection *out, char **error_json)` | status | the one connect entry point, with a session namespace (ADR-0117, ADR-0220); NULL name selects `neo4j` |
| `relate_conn_set_namespace(relate_connection conn, const char *name, char **error_json)` | status | changes the session namespace between transactions; a NULL name is a refused argument, never dereferenced |
| `relate_disconnect(relate_connection conn)` | status | frees the connection (also when poisoned); `RELATE_ERROR` on NULL |
| `relate_checkpoint(relate_database db, char **error_json)` | status | Parquet snapshot + WAL recycle; not the commit durability boundary (autocommit fsyncs its WAL record before the call returns) |

### Statement and transaction

| Function | Returns | Notes |
| --- | --- | --- |
| `relate_query_params(relate_connection conn, const char *cypher, const relate_params params, relate_query_result *out, char **error_json)` | status | the one query entry point (ADR-0220). `params` is a builder root or a `put_map` child (read, not modified, reusable) or NULL for no parameters; a list builder is refused. A statement failure returns `RELATE_OK` with an error result; `RELATE_ERROR` is connection-level only, with `*out` NULL |
| `relate_conn_begin(relate_connection conn, char **error_json)` | status | explicit transaction (ADR-0028); single-level, one active per connection |
| `relate_conn_commit(relate_connection conn, char **bookmark_out, char **error_json)` | status | commits the explicit transaction. `RELATE_OK`: `*bookmark_out` is the bookmark — caller-owned, free with `relate_free_string` — or NULL when the transaction committed nothing. `RELATE_ERROR`: the commit failed, `*bookmark_out` is NULL and `*error_json` carries the shape. A NULL `bookmark_out` is a refused call (the transaction is **not** committed) — see Bookmarks |
| `relate_conn_rollback(relate_connection conn, char **error_json)` | status | rolls back |
| `relate_destroy_result(relate_query_result result)` | `void` | destroys the result and every value handle and string it owns; NULL-safe |

### Result shape and metadata

Every string in this block is result-owned: valid until
`relate_destroy_result`, never freed by the caller. A NULL result gives
`0` / `0.0` / NULL; an **error result** gives NULL for the string accessors
and `0.0` for the timing accessors (and `relate_row_count` 0 — an error
result has no cells).

| Function | Returns | Notes |
| --- | --- | --- |
| `relate_column_count(result)` | `long long` | `0` on NULL |
| `relate_row_count(result)` | `long long` | `0` on NULL and on an error result |
| `relate_column_name(result, col)` | `const char *` | NULL when out of range |
| `relate_result_is_error(result)` | `uint8_t` | 1 on an error result, 0 on a good result, 0 on NULL |
| `relate_result_error_json(result)` | `const char *` | the error shape on an error result, NULL on a good result |
| `relate_result_bookmark(result)` | `const char *` | the auto-commit bookmark (see Bookmarks) |
| `relate_result_query_type(result)` | `const char *` | the query type (ADR-0162): exactly `"r"`, `"w"`, `"rw"` or `"s"`, classified from the plan; EXPLAIN/PROFILE report the wrapped statement's type |
| `relate_result_namespace(result)` | `const char *` | the resolved namespace (ADR-0117): exactly `"neo4j"`, `"system"` or `"system_ontologies"` — the `USE` target when the statement carries one, otherwise the session namespace |
| `relate_result_stats(result)` | `const char *` | the mutation counters as a JSON object string (ADR-0216): the canonical 11, snake_case, zeros included |
| `relate_result_notifications(result)` | `const char *` | the query notifications as a JSON array string (ADR-0363), always present on a good result, `[]` on a plain run |
| `relate_result_plan_json(result)` | `const char *` | the structured plan as a `relate.plan/v1` document (ADR-0381), NULL when the statement carried no plan |
| `relate_result_elapsed_ms(result)` | `double` | the engine-reported statement elapsed, wall-clock ms (ADR-0221) |
| `relate_result_parse_ms(result)` / `_plan_ms` / `_execute_ms` | `double` | the phase split (ADR-0491): three disjoint sub-windows of the elapsed — parse/lowering, bind+plan, dispatch (or the EXPLAIN render); their sum never exceeds the elapsed |

Mutation counters (the 11): `nodes_created`, `nodes_deleted`,
`relationships_created`, `relationships_deleted`, `properties_set`,
`labels_added`, `labels_removed`, `indexes_added`, `indexes_removed`,
`constraints_added`, `constraints_removed`.

### The value walker

`relate_value` is an opaque handle to one value of a result.
`relate_value_at` returns a cell's root; the list and map accessors
descend. **Every handle — roots and the children minted lazily below them —
is owned by the result**; there is no per-value free, and
`relate_destroy_result` frees them all.

`relate_value_kind` returns one enumerator of
`enum relate_value_kind` — one per engine value shape, discriminated
**structurally at every depth** (a user map shaped like a node is still
`MAP`; a nested path is still `PATH`):

`RELATE_V_NULL=0`, `_BOOL=1`, `_INT=2`, `_FLOAT=3`, `_STRING=4`,
`_NODE=5`, `_RELATIONSHIP=6`, `_PATH=7`, `_LIST=8`, `_MAP=9`, `_POINT=10`,
`_DATE=11`, `_TIME=12`, `_LOCAL_TIME=13`, `_DATETIME=14`,
`_LOCAL_DATETIME=15`, `_DURATION=16`, `_VECTOR=17`. Byte arrays cross as
LIST of INT and UUIDs as STRING. A Cypher `null` is a handle of kind `NULL`,
never a NULL handle — a NULL handle means "no such value" (index out of
range, absent map key, an error result). A client meeting an enumerator it
does not know (a later, additive ABI) treats it as a defined, reportable
case — never a crash, never a silent null.

**Refusal convention.** Out-of-kind access — calling an accessor on the
wrong kind, on a NULL handle, or with a NULL out-param — is a **defined
refusal**, never undefined behaviour, and never poisons anything; the handle
stays usable:

- pointer returns (`const char *`, `relate_value`) return NULL. An
  in-kind call returns NULL only for `_list_get`/`_map_key` past the end,
  `_map_get` of an absent key, and an element id in the `system` namespace
  (which has no elements);
- count returns (`_kind`, `_list_len`, `_map_len`, `_vector_dim`) return -1;
- scalar reads return `relate_status`: `RELATE_OK` with the
  out-params written, or `RELATE_ERROR` with them untouched. No
  `error_json` is produced — the status is the report.

| Function | Serves | Notes |
| --- | --- | --- |
| `relate_value_at(result, row, col)` | — | the cell's root handle; NULL when either index is negative/out of range (an error result has no cells) or the result is NULL |
| `relate_value_kind(v)` | — | the kind enumerator; -1 for a NULL handle |
| `relate_value_bool(v, uint8_t *out)` | BOOL | 0/1 |
| `relate_value_int(v, int64_t *out)` | INT | lossless |
| `relate_value_float(v, double *out)` | FLOAT | NaN and ±Inf included |
| `relate_value_is_nan(v, uint8_t *out)` / `_is_inf` | FLOAT | 1 when NaN / infinite (either sign) |
| `relate_value_string(v, size_t *len)` | STRING | NUL-terminated UTF-8, result-owned; `*len` (when non-NULL) is the byte length without the terminator — a string with an interior NUL crosses whole |
| `relate_value_element_id(v)` | NODE, RELATIONSHIP | spelled exactly as Cypher `elementId()` returns it (`"4:relate:n0"`, `"5:relate:r0"`), so `WHERE elementId(n) = $id` round-trips |
| `relate_value_start_element_id(v)` / `_end_element_id` | RELATIONSHIP | the end nodes' element ids |
| `relate_value_id(v, int64_t *out)` | NODE, RELATIONSHIP | the deprecated numeric entity id — exactly the INTEGER Cypher `id()` returns for the same entity (ADR-0506); driver providers that decide entity equality by numeric id read it here |
| `relate_value_start_id(v, int64_t *out)` / `_end_id` | RELATIONSHIP | the endpoints' numeric ids, as `id(startNode(r))` / `id(endNode(r))` return them; wrong kind (a NODE, a non-entity) or NULL handle → `RELATE_ERROR` with `*out` untouched — an entity always carries an id, so there is no in-kind refusal |
| `relate_value_list_len(v)` | LIST, PATH, NODE | LIST: its items; PATH: 2n+1 elements (node, rel, …, node); NODE: its labels |
| `relate_value_list_get(v, i)` | LIST, PATH, NODE | child handle; NULL past the end |
| `relate_value_map_len(v)` | MAP, NODE, RELATIONSHIP | MAP: entries; entities: their properties; entries keep engine order |
| `relate_value_map_key(v, i, size_t *len)` | MAP, entities | key `i`, NUL-terminated UTF-8 with `*len` as for `_string`; NULL past the end |
| `relate_value_map_get(v, key)` | MAP, entities | the value under a NUL-terminated UTF-8 key; NULL when absent — a present key holding Cypher `null` returns a `RELATE_V_NULL` handle |
| `relate_value_year/_month/_day(v, int64_t *out)` | DATE, DATETIME, LOCAL_DATETIME | month 1-12, day 1-31 |
| `relate_value_hour/_minute/_second/_nanosecond(v, int64_t *out)` | TIME, LOCAL_TIME, DATETIME, LOCAL_DATETIME | nanosecond is sub-second, 0-999999999 |
| `relate_value_offset_seconds(v, int64_t *out)` | TIME, DATETIME | the UTC offset |
| `relate_value_zone(v)` | TIME, DATETIME | the IANA zone id (`"Europe/Stockholm"`), or `""` when the value carries only an offset (always `""` for TIME); result-owned |
| `relate_value_duration(v, int64_t *months, *days, *seconds, *nanos)` | DURATION | the four lossless components; `PT36H` ≠ `P1DT12H` |
| `relate_value_point(v, int32_t *srid, int32_t *dim, double *x, *y, *z)` | POINT | dim 2 or 3; x/y = longitude/latitude for a geographic srid; `z` written only when `*dim` is 3; every out-param must be non-NULL |
| `relate_value_vector_dtype(v)` | VECTOR | the engine's canonical coordinate-type name — `INTEGER8`, `INTEGER16`, `INTEGER32`, `INTEGER`, `FLOAT16`, `BFLOAT16`, `FLOAT32` or `FLOAT`; result-owned |
| `relate_value_vector_dim(v)` | VECTOR | the dimension |
| `relate_value_vector_float(v, i, double *out)` | VECTOR | the four `FLOAT*` dtypes; the int family or an out-of-range index refuses |
| `relate_value_vector_int(v, i, int64_t *out)` | VECTOR | the four `INTEGER*` dtypes; the float family or an out-of-range index refuses |

The rest of an entity reads through the generic accessors: a NODE's labels
are its list (each item a STRING) and its properties its map; a
RELATIONSHIP's type is its string and its properties its map. The numeric
id accessors (`_id`, `_start_id`, `_end_id`, ADR-0506) exist for driver
providers that decide node / relationship equality the way the stock Neo4j
drivers do — by the deprecated numeric `id`; prefer the element ids for
everything user-facing.

Temporals cross as components and the zoned kinds report their **local wall
clock**; ISO text is a client rendering choice. Walk a cell once into client
objects — every accessor is one FFI call.

See [`examples/walker.c`](examples/walker.c) — a complete recursive decoder.

### The params builder

The typed params builder is the inbound mirror of the walker: each putter
binds one kind from the same components the walker reads back. There is no
JSON parameter path.

**Builders.** `relate_params_new()` returns a root: the statement's MAP
of named parameters (it never returns NULL). `relate_params_put_list` /
`_put_map` insert an empty nested LIST / MAP and write its child builder to
`*child`; a child takes the same putters. The container decides the name
rule: a MAP takes a non-NULL, unique UTF-8 name; a LIST takes a NULL name
and appends in call order.

**Lifetime.** One tree, one owner: the root owns every child.
`relate_params_free(root)` frees the tree; on a child or NULL it is a
no-op. A child stays valid — and open for puts — until its root is freed.
There is no build or close step: `relate_query_params` reads the builder
as it stands, without modifying it, so the same root may back many
statements and be extended between them (an existing name cannot be
overwritten — a duplicate is refused). `params` may also be a `put_map`
child; a list builder is refused. Strings and arrays passed to a putter are
copied. A builder must not be used from two threads at once.

**Refusals.** Every putter returns `relate_status`. A NULL builder, a
name that breaks the container's rule, a NULL required pointer, invalid
UTF-8, an out-of-range component or non-existent date, an unknown zone, a
DST-gap wall clock, a srid/dimension mismatch, a coordinate the vector dtype
cannot hold exactly, or nesting past the engine's value-depth cap is a
**defined refusal**: `RELATE_ERROR` with the error shape (internal
stage) in `*error_json` (NULL opts out), the builder unchanged and still
usable. On a container refusal `*child` is untouched.

**Fixed caps** (OPLANE_REQ-00340363/57, LABS-937): 100 000 entries per
list/map container, 4096 bytes per parameter name, and a builder-wide
byte budget of 16 MiB — the one wire-message bound
(`relate_core::MAX_WIRE_MESSAGE_BYTES`, which Bolt's framing also
derives from), covering every entry's name, value bytes and a fixed
overhead across all nested containers, so the aggregate is bounded, not
just each container; `put_string` refuses an over-budget length before
copying its bytes. Each is a defined refusal one past its limit (tested
at the boundary). Fixed constants, no knob — the house rule of
ADR-0257's 4 MiB/500 and ADR-0462's depth cap.

**Trusted lengths.** A `len`/`dim` argument and the buffer behind it are
trusted: Rust cannot detect a short or unterminated buffer, so passing
one is undefined behaviour. Name buffers must be NUL-terminated (they
are read as C strings; an interior NUL in a name is a defined refusal).

| Function | Notes |
| --- | --- |
| `relate_params_new(void)` | the root builder |
| `relate_params_free(p)` | frees the root and every child; NULL/child: no-op |
| `relate_params_put_null(p, name, &err)` | |
| `relate_params_put_bool(p, name, uint8_t v, &err)` | 0 is false, any other value true |
| `relate_params_put_int(p, name, int64_t v, &err)` | full int64 range |
| `relate_params_put_float(p, name, double v, &err)` | NaN/±Inf bind |
| `relate_params_put_string(p, name, const char *v, size_t len, &err)` | `len` bytes of UTF-8; an interior NUL crosses whole; invalid UTF-8 refuses; a NULL `v` with no bytes binds the empty string (a marshalling layer may hand a zero-length buffer over as NULL — koffi does), a NULL `v` that claims bytes refuses ("null string value"); `put_null` binds an explicit null |
| `relate_params_put_date(p, name, year, month, day, &err)` | a real calendar date |
| `relate_params_put_local_time(p, name, hour, minute, second, nanosecond, &err)` | wall clock only |
| `relate_params_put_time(p, name, hour, minute, second, nanosecond, offset_seconds, zone, &err)` | wall clock + offset; a non-NULL zone supplies the offset, resolved at the current instant as Cypher's `time({timezone: …})` does — a TIME keeps no zone id |
| `relate_params_put_local_datetime(p, name, y, mo, d, h, mi, s, ns, &err)` | wall clock only |
| `relate_params_put_datetime(p, name, y, mo, d, h, mi, s, ns, offset_seconds, zone, &err)` | NULL zone: offset-only. Non-NULL IANA zone: the value keeps the zone (the walker reads it back); the offset is resolved from it at that wall clock, `offset_seconds` ignored; a DST overlap takes the earlier offset (ADR-0178), a gap refuses |
| `relate_params_put_duration(p, name, months, days, seconds, nanos, &err)` | any int64 values; the walker reads back the engine's normalized split |
| `relate_params_put_point(p, name, int32_t srid, x, y, const double *z, &err)` | 7203/4326 with `z` NULL; 9157/4979 with `z` pointing at the third coordinate; any other pairing refuses |
| `relate_params_put_vector_float(p, name, const char *dtype, const double *values, size_t dim, &err)` | the four `FLOAT*` dtypes; each coordinate exactly representable |
| `relate_params_put_vector_int(p, name, const char *dtype, const int64_t *values, size_t dim, &err)` | the four `INTEGER*` dtypes; each value within the dtype's width |
| `relate_params_put_list(p, name, relate_params *child, &err)` / `_put_map` | nested container; child owned by the root |

Components are `int64_t`, as the walker reads them: month 1-12, day 1-31, hour
0-23, minute 0-59, second 0-59, nanosecond 0-999999999, `offset_seconds`
within ±18:00 (±64800).

Every parameter a statement references must be supplied (a bound `null` is
fine); a missing one fails the statement before it executes with
`semantic error: ParameterMissing: Expected parameter(s): <names>`
(ADR-0308) — an error result, never a fabricated `null` binding.

See [`examples/params.c`](examples/params.c).

### Bookmarks

A bookmark is the engine's commit sequence in the ADR-0310 string form
`"relate:<n>"`. `n` is the store's commit sequence, monotonically
increasing across every connection and surface, so a later commit carries a
greater `n`. Compare the parsed `n`, never the strings: `relate:10` sorts
before `relate:9` lexicographically. Clients never mint bookmarks
from a local counter.

- **Auto-commit:** `const char *relate_result_bookmark(r)` is
  result-owned — copy it before `relate_destroy_result`. It returns NULL
  when the statement committed nothing: a read, a write whose row pipeline
  was empty, an EXPLAIN, a statement inside an explicit transaction, or an
  error result.
- **Explicit transaction:** `relate_conn_commit(conn, &bookmark, &err)`
  commits and reports through its status (ADR-0510). `RELATE_OK` with
  `*bookmark` set ⇒ committed: the bookmark is a caller-owned string (free
  with `relate_free_string`). `RELATE_OK` with `*bookmark` NULL ⇒
  committed nothing (no bookmark is fabricated for a commit that crossed
  no WAL boundary; the caller's last bookmark stays current).
  `RELATE_ERROR` ⇒ the commit failed (`*bookmark` is NULL, `*err`
  carries the shape) — a `retryable: true` conflict belongs to the
  driver's retry loop. The status alone distinguishes the three, so
  opting out of `err` loses nothing but the detail. `bookmark_out` must
  not be NULL: a NULL bookmark out-param is a refused call
  (`RELATE_ERROR`, "null out pointer") and the transaction is **not**
  committed — commit is the durability boundary, and the ABI refuses
  rather than run it with the bookmark discarded.

See [`examples/bookmarks.c`](examples/bookmarks.c).

## Namespaces (ADR-0117)

Every store has the fixed namespaces `neo4j`, `system` and
`system_ontologies` (ADR-0116). A connection's session namespace — `neo4j`
by default — is where its statements run when they carry no `USE` clause; a
`USE` clause routes one statement only. Names match case-insensitively.
`system_ontologies` is listed but not queryable (ADR-0229): every statement
routed there — reads included — fails as an error result with the 42N17
text; only the ADR-0116 §2 administration commands run from there.

- `relate_connect_namespace(db, name, &conn, &err)` opens a connection
  whose session namespace is `name`; a NULL name selects `neo4j`.
- `relate_conn_set_namespace(conn, name, &err)` changes the session
  namespace, only **between** transactions: while an explicit transaction is
  open it fails with a message ending
  `Cannot change the namespace while a transaction is open.`

An unknown name on either call fails with a message ending
`Graph not found: <name>` (ADR-0146; live CE wire 42001 with a 42N00 cause).

See [`examples/namespace.c`](examples/namespace.c) and
[`examples/result_metadata.c`](examples/result_metadata.c).

## Threading

A `relate_database` may be shared across threads; a
`relate_connection` must **not** be — all calls that share a connection
must be serialized by the caller (one call at a time). Distinct connections
may be used from distinct threads. Because every failure travels with the
call that produced it, the error channel is async-safe: a call run on a
worker thread reports its own error.

## Panic safety

Every entry point catches Rust panics at the FFI boundary (an unwind
crossing into C would be undefined behavior). If a call panics, the handle
is **POISONED**: it is not freed, but every subsequent call on it returns a
defined failure — `RELATE_ERROR` with the internal-stage error shape, or
a 0/NULL result accessor — instead of touching the possibly-inconsistent
database. Recover by closing the handle and opening a new one; close and
destroy still free a poisoned handle.

## Invocation

```sh
cargo build -p relate-ffi
cc -I surfaces/relate-ffi/include docs/api/c/examples/return_one.c \
   -L target/debug -lrelate_ffi -o /tmp/return_one
DYLD_LIBRARY_PATH=target/debug /tmp/return_one   # prints: n / 1
```

CI compiles and runs every `.c` file in
[`examples/`](examples/) via `scripts/ffi-compile-example.sh`:

- [`return_one.c`](examples/return_one.c) — open, connect, one query, one
  INT cell through the walker.
- [`error_channel.c`](examples/error_channel.c) — the two error channels
  (an out-param failure and an error result) side by side.
- [`walker.c`](examples/walker.c) — a complete recursive decoder over every
  kind, plus the value kinds' refusal behaviour.
- [`params.c`](examples/params.c) — the typed params builder: scalars,
  nested list/map children, temporals, point, vector; reuse of one root.
- [`transaction.c`](examples/transaction.c) — an explicit multi-statement
  transaction.
- [`bookmarks.c`](examples/bookmarks.c) — the auto-commit and
  explicit-commit bookmarks, and the commit that committed nothing.
- [`namespace.c`](examples/namespace.c) — namespaces and the
  not-queryable refusal.
- [`result_metadata.c`](examples/result_metadata.c) — query type, resolved
  namespace, stats, timings.
- [`notifications.c`](examples/notifications.c) — the engine-native
  notification list.
- [`plan.c`](examples/plan.c) — the `relate.plan/v1` plan document.

## The agent-native plan document (`relate.plan/v1`)

`relate_result_plan_json` returns the statement's structured plan as a
`relate.plan/v1` document (ADR-0381) — the schema our own bindings hand
out, deliberately distinct from the Neo4j-facing plan shape the Bolt and
HTTP surfaces emit (ADR-0357, ADR-0365). NULL means the statement carried no
plan.

```json
{
  "schema": "relate.plan/v1",
  "profiled": true,
  "statement": "MATCH (n:Person) RETURN n.name AS name",
  "root": {
    "operator": "ProduceResults",
    "details": "name",
    "identifiers": ["name"],
    "estimated_rows": null,
    "actual": {
      "rows": 3, "time_ms": 0.004, "total_time_ms": 0.046, "db_hits": 3,
      "memory_bytes": 66, "expression_built_bytes": 0,
      "hops": 0, "neighbor_walks": 0, "node_reads": 3
    },
    "children": [ /* the same shape, one object per operator */ ],
    "warnings": [ {
      "code": "Neo.ClientNotification.Statement.CartesianProduct",
      "gql_status": "03N90", "severity": "INFORMATION", "category": "PERFORMANCE",
      "title": "…", "description": "…", "remedy": "…"
    } ]
  }
}
```

- `profiled` — `true` on a `PROFILE` tree (the nodes carry `actual`),
  `false` on `EXPLAIN` (`actual` is `null`).
- `statement` — the planned statement as echoed, or `null`.
- Each node: `operator`, `details`, `identifiers`, `estimated_rows`
  (or `null`), `actual` (or `null`), `children`, and `warnings` — the
  remedy-bearing notifications raised for that operator, matched by
  operator id, each `{code, gql_status, severity, category, title,
  description, remedy}`; `[]` when the operator raised none. `actual` carries `rows`, `time_ms` (a float), `total_time_ms` (a
  float, the operator's own time plus every descendant's — the cumulative
  form of `time_ms`), `seek_ms`/`walk_ms` (floats, the phase times: the
  candidate resolution and the relationship traversal each spent,
  inclusive of nested phases — zero when the clause does no pattern work),
  `loops` (the input clause's rows — the operator ran once per input row;
  `null` at the root, the seed row), `frontier` (`{edge_visits, endpoints,
  levels, cells, memo_hits, memo_misses, representative_proofs,
  search_proven, search_disproven, declined_exhausted, budget_fraction}`
  — the traversal frontier's counters with the NFA rung's own facts (the
  (node, state) cells, the re-arrival rule's memo asks, the admission
  asks by verdict, and the rule's spent-work fraction over its budget
  constant), zeros when the clause walks no frontier), `frontier_decline`
  (the frontier rung's run-time decline as its stable name — `null` when
  the clause never declined; `rearrival-budget`, ADR-0516, or
  `table-budget`, ADR-0521),
  `db_hits`, `memory_bytes`, `expression_built_bytes`, the read-kind mix (`hops`,
  `neighbor_walks`, `node_reads`), `batched` (whether the operator ran the
  ADR-0468 batch scan/filter/project chain over `Chunk` — `false` on every
  operator the chain declined), `zone_skips` (that chain's filter's zone
  skips — conjunct-batch evaluations the chunk's derived per-column min/max
  provably replaced with the correct empty survivor set, `0` when it
  skipped nothing), and `start` (`{element, backward, reason}`) when the
  operator walked a pattern.
- Field names are `snake_case` and numbers are numbers; there is no nested
  `args` map and no rendered text to parse. A breaking change bumps the
  `schema` version and ships its own ADR.
