/*
 * Relate C FFI — the typed ABI v2 (ADR-0502).
 *
 * Provides a C-compatible API for embedding Relate in the JS, Java, Go
 * and Swift clients via their FFI mechanisms. relate_abi_version()
 * returns 2.
 *
 * Error channel (ADR-0502 §2): one error shape travels with the call that
 * failed — there is no thread-local error slot.
 *
 *   {"code":"<Neo4j status code>","gql_status":"<GQLSTATUS>",
 *    "stage":"compile|runtime|internal","retryable":<bool>,
 *    "message":"<message>"}
 *
 * The classification — code, gql_status, stage, retryable — is the
 * engine's single table (`relate::error_info`); the message is the
 * engine's own for a compile/runtime failure, the boundary's own for a
 * failure the engine never saw (a null or poisoned handle, an argument
 * that failed to decode), and the fixed "internal error" text for
 * engine-internal, IO/storage/locked failures and caught panics
 * (ADR-0260): paths and panic payloads never cross the ABI. The
 * redacted original is logged to the host's stderr only when the host
 * opts in by setting RELATE_FFI_LOG=1 (read once per process).
 *   - A statement failure rides the result: relate_query_params
 *     returns RELATE_OK with an error result —
 *     relate_result_is_error(r) is 1 and
 *     relate_result_error_json(r) returns the shape.
 *   - Every other fallible status call — open, connect,
 *     conn_set_namespace, conn_begin/commit/rollback, checkpoint, the
 *     params builder's putters, and query_params' own connection-level
 *     failures — returns
 *     RELATE_ERROR and fills its trailing `char **error_json`
 *     out-param with the same shape. Passing NULL for the out-param opts
 *     out of the detail; the status code alone then reports the failure.
 *     relate_conn_commit also hands its bookmark over through a
 *     `char **bookmark_out` out-param — see the bookmarks block below.
 *
 * Memory ownership (the return type is the rule: mutable char * is
 * caller-owned, const char * is owned by Relate):
 *   - Caller-owned strings — the `char **error_json` out-params and
 *     relate_conn_commit()'s *bookmark_out — must be freed with
 *     relate_free_string().
 *   - Result-owned strings — relate_column_name(),
 *     relate_result_error_json(), relate_result_bookmark(),
 *     the result metadata accessors and every string a value accessor
 *     returns — are owned by the result and valid until
 *     relate_destroy_result(); they must NOT be freed.
 *   - Value handles (relate_value) are owned by the result as well:
 *     there is no per-value free; relate_destroy_result() frees every
 *     handle minted from that result, after which none may be used.
 *   - A params builder (relate_params) is caller-owned: free the root
 *     with relate_params_free(). Child builders (put_list / put_map)
 *     are owned by their root and freed with it; strings and arrays passed
 *     to a putter are copied, never retained.
 *
 * Threading contract:
 *   A relate_database may be shared across threads; a
 *   relate_connection must NOT be — all calls that share a connection
 *   must be serialized by the caller (one call at a time). Distinct
 *   connections may be used from distinct threads. Because every failure
 *   travels with the call that produced it, the error channel is
 *   async-safe: a call run on a worker thread reports its own error.
 *
 * Panic safety:
 *   Every entry point catches Rust panics at the FFI boundary (an unwind
 *   crossing into C would be undefined behavior). If a call panics, the
 *   handle is POISONED: it is not freed, but every subsequent call on it
 *   returns a defined failure (RELATE_ERROR with the internal-stage
 *   error shape, or a 0/NULL result accessor) instead of touching the
 *   possibly-inconsistent database. Recover by closing the handle and
 *   opening a new one; close/destroy still free a poisoned handle.
 */

#ifndef RELATE_H
#define RELATE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * The ABI major version (ADR-0502 §8): 2 for the typed ABI. After the
 * first public release the ABI is additive-only: a bump means symbols
 * were removed or signatures changed.
 */
int relate_abi_version(void);

/*
 * Free a caller-owned (char *) string returned by this library — today the
 * error_json out-params and relate_conn_commit()'s *bookmark_out.
 * Safe to call with NULL.
 */
void relate_free_string(char *s);

/*
 * Tabular / multi-connection API (language clients). Status:
 * RELATE_OK = 0, RELATE_ERROR = 1.
 *
 * Handle ownership: close the database with relate_database_close(),
 * disconnect the connection with relate_disconnect(), and destroy
 * every query result with relate_destroy_result(). Only
 * relate_destroy_result() accepts NULL as a no-op; passing NULL to
 * relate_database_close() or relate_disconnect() returns
 * RELATE_ERROR (no detail — the error_json channel belongs to the
 * fallible calls below).
 *
 * Values: result cells are read through the typed value walker below
 * (ADR-0502 §1) — relate_value_at() and the relate_value_*
 * accessors.
 *
 * Parameters (relate_query_params): a typed params builder — see
 * relate_params below (ADR-0502 §4). There is no JSON params path.
 *
 * Panic safety: identical to the crate-wide contract above — a panicking
 * database or connection is poisoned; later calls on it return
 * RELATE_ERROR (or 0 / NULL) with the internal-stage error shape;
 * close/destroy still frees a poisoned handle.
 */
typedef struct relate_database_s *relate_database;
typedef struct relate_connection_s *relate_connection;
typedef struct relate_query_result_s *relate_query_result;
typedef struct relate_params_s *relate_params;
typedef enum relate_status {
    RELATE_OK = 0,
    RELATE_ERROR = 1
} relate_status;

/*
 * Open a database: ":memory:", a Native directory, or a file://… / *.cl
 * URI. On RELATE_ERROR, *out is NULL and *error_json (unless the
 * out-param itself is NULL) carries the ADR-0502 §2 shape; free it with
 * relate_free_string().
 */
relate_status relate_database_open(const char *uri, relate_database *out, char **error_json);

/*
 * Open an existing database without writing, checkpointing, or creating a
 * missing store (ADR-0136). Same out-param and error channel rules as
 * relate_database_open. Reads succeed; writes and checkpoints fail
 * with "database is read-only".
 */
relate_status relate_database_open_read_only(const char *uri, relate_database *out, char **error_json);
relate_status relate_database_close(relate_database db);

/*
 * The one connect entry point (ADR-0220), with a session namespace
 * (ADR-0117): "neo4j", "system" or "system_ontologies", matched
 * case-insensitively. The namespace is the session default for statements
 * that carry no USE clause. A NULL name selects the default "neo4j"
 * namespace.
 *
 * On RELATE_ERROR, *out is NULL and *error_json carries the error
 * shape. An unknown name fails with a message ending
 *   Graph not found: <name>
 * (ADR-0146; live Neo4j CE text).
 */
relate_status relate_connect_namespace(relate_database db, const char *name, relate_connection *out, char **error_json);

/*
 * Change the session namespace of an open connection (ADR-0117). Names
 * match case-insensitively; the same unknown-name error as
 * relate_connect_namespace applies.
 *
 * A session namespace changes only between transactions: while an explicit
 * transaction is open (relate_conn_begin without a matching
 * commit/rollback) this fails with RELATE_ERROR and a message ending
 *   Cannot change the namespace while a transaction is open.
 * A NULL name fails with RELATE_ERROR ("null pointer"); it is never
 * dereferenced.
 */
relate_status relate_conn_set_namespace(relate_connection conn, const char *name, char **error_json);

relate_status relate_disconnect(relate_connection conn);

/*
 * The one query entry point (ADR-0220). params is a params builder root
 * (or a put_map child) holding the statement's named parameters, read and
 * not modified — it may be reused and extended for later statements. NULL
 * runs the statement with no parameters. A list builder is refused
 * (RELATE_ERROR; parameters are named).
 *
 * A statement failure rides the result (ADR-0502 §2): the call returns
 * RELATE_OK with an error result — relate_result_is_error() is 1
 * and relate_result_error_json() carries the shape. RELATE_ERROR
 * is reserved for connection-level failures (a null or poisoned
 * connection, a caught panic, an argument that failed to decode), reported
 * through the trailing error_json out-param like every other status call;
 * on RELATE_ERROR, *out is NULL.
 */
relate_status relate_query_params(relate_connection conn, const char *cypher, const relate_params params, relate_query_result *out, char **error_json);

relate_status relate_conn_begin(relate_connection conn, char **error_json);
relate_status relate_conn_rollback(relate_connection conn, char **error_json);
relate_status relate_checkpoint(relate_database db, char **error_json);

/*
 * Bookmarks (ADR-0502 §7) — the engine's commit sequence in the ADR-0310
 * string form "relate:<n>". n is the store's commit sequence,
 * monotonically increasing across every connection and surface, so a
 * later commit carries a greater n. Compare the parsed n, never the
 * strings: "relate:10" sorts before "relate:9" lexicographically.
 *
 * relate_conn_commit commits the explicit transaction (ADR-0510).
 * RELATE_OK:
 * *bookmark_out is the transaction's bookmark as a caller-owned char * —
 * free it with relate_free_string() — or NULL when the transaction
 * committed nothing (no write ran, or every write was a no-op): no
 * bookmark is fabricated for a commit that crossed no WAL boundary. The
 * status alone tells a failure from an empty commit, so opting out of
 * error_json loses nothing but the detail. RELATE_ERROR: the commit
 * failed, *bookmark_out is NULL and *error_json (unless the out-param
 * itself is NULL) carries the ADR-0502 §2 shape. bookmark_out must not be
 * NULL: a NULL bookmark out-param is a refused call (RELATE_ERROR,
 * "null out pointer", the transaction NOT committed) — commit is the
 * durability boundary, and the ABI refuses rather than run it with the
 * bookmark discarded.
 */
relate_status relate_conn_commit(relate_connection conn, char **bookmark_out, char **error_json);

/*
 * The auto-commit statement's bookmark, verbatim from the engine's own
 * commit sequence: "relate:<n>" after an auto-commit write. NULL when
 * the statement committed nothing — a read, a write whose row pipeline
 * was empty, an EXPLAIN (nothing executes), a statement inside an
 * explicit transaction (its commit's bookmark arrives through
 * relate_conn_commit's *bookmark_out), or an error result.
 *
 * The returned pointer is owned by the result and valid until
 * relate_destroy_result(); it must NOT be freed (same convention as
 * relate_result_stats). NULL result -> NULL.
 */
const char *relate_result_bookmark(relate_query_result result);

/*
 * 1 when the result is an error result (a statement failure that rode the
 * result, ADR-0502 §2), 0 on a good result. NULL result -> 0.
 */
uint8_t relate_result_is_error(relate_query_result result);

/*
 * The ADR-0502 §2 error shape when the result is an error result, NULL on
 * a good result. Result-owned: valid until relate_destroy_result()
 * and must NOT be freed. NULL result -> NULL.
 */
const char *relate_result_error_json(relate_query_result result);

long long relate_column_count(relate_query_result result);
long long relate_row_count(relate_query_result result);
const char *relate_column_name(relate_query_result result, long long col);

/*
 * The statement's query type (ADR-0162): exactly "r" (read), "w" (write),
 * "rw" (read-write) or "s" (schema), classified from the plan. EXPLAIN and
 * PROFILE report the wrapped statement's type.
 *
 * The returned pointer is owned by the result and valid until
 * relate_destroy_result(); it must NOT be freed (same convention as
 * relate_column_name). NULL on an error result. NULL result -> NULL.
 */
const char *relate_result_query_type(relate_query_result result);

/*
 * The statement's resolved namespace (ADR-0117): exactly "neo4j", "system"
 * or "system_ontologies". This is the namespace the statement ran in -- the
 * USE target when the statement carries a USE clause, otherwise the session
 * namespace; it never reflects a session-namespace change by itself.
 *
 * The returned pointer is owned by the result and valid until
 * relate_destroy_result(); it must NOT be freed (same convention as
 * relate_column_name). NULL on an error result. NULL result -> NULL.
 */
const char *relate_result_namespace(relate_query_result result);

/*
 * The statement's mutation counters as a JSON object string (ADR-0216):
 * the canonical 11, snake_case, zeros included -- nodes_created,
 * nodes_deleted, relationships_created, relationships_deleted,
 * properties_set, labels_added, labels_removed, indexes_added,
 * indexes_removed, constraints_added, constraints_removed.
 *
 * The returned pointer is owned by the result and valid until
 * relate_destroy_result(); it must NOT be freed (same convention as
 * relate_column_name). NULL on an error result. NULL result -> NULL.
 */
const char *relate_result_stats(relate_query_result result);

/*
 * The statement's query notifications (ADR-0363) as a JSON array string in
 * the engine-native shape. The array is always present on a good result:
 * [] on a plain run, populated by EXPLAIN and PROFILE. Each entry carries
 * code, gql_status, severity, category, title, description and remedy,
 * plus operator_id on operator-scoped kinds:
 *
 *   [{"code":"Neo.ClientNotification.Statement.CartesianProduct",
 *     "gql_status":"03N90","severity":"INFORMATION",
 *     "category":"PERFORMANCE","title":"...","description":"...",
 *     "remedy":"...","operator_id":1}]
 *
 * The returned pointer is owned by the result and valid until
 * relate_destroy_result(); it must NOT be freed (same convention as
 * relate_column_name). NULL on an error result. NULL result -> NULL.
 */
const char *relate_result_notifications(relate_query_result result);

/*
 * The statement's structured plan as a `relate.plan/v1` JSON document
 * (LABS-370, ADR-0381), or NULL when the statement carried no plan
 * (a plain run). The document is
 * {schema, profiled, statement, root} where each node is
 * {operator, details, identifiers, estimated_rows, actual, children,
 * warnings} -- snake_case, real numbers, no nested args map. A node's
 * warnings are the remedy-bearing notifications raised for that operator
 * (code, gql_status, severity, category, title, description, remedy),
 * matched by operator id. This is the
 * schema our own bindings hand out; the Neo4j-facing plan shape the Bolt
 * and HTTP surfaces emit is a different (wire) dialect of the same tree.
 *
 * The returned pointer is owned by the result and valid until
 * relate_destroy_result(); it must NOT be freed (same convention as
 * relate_result_stats). NULL on an error result. NULL result -> NULL.
 */
const char *relate_result_plan_json(relate_query_result result);

/*
 * The engine-reported statement elapsed in wall-clock milliseconds
 * (ADR-0221): parse, plan and execution, excluding any client-side row
 * handling or serialization. 0.0 on an error result. NULL result -> 0.0.
 */
double relate_result_elapsed_ms(relate_query_result result);

/*
 * The statement's phase split (ADR-0491): three disjoint sub-windows of
 * the elapsed total above, measured always-on by the engine.
 * relate_result_parse_ms is the front-end parse and lowering window;
 * relate_result_plan_ms is the binding, logical planning and
 * admission classification window; relate_result_execute_ms is the
 * dispatch window -- or the EXPLAIN plan render for an EXPLAIN statement.
 * Their sum never exceeds relate_result_elapsed_ms (the inter-stage
 * gaps stay in the total alone). 0.0 on an error result. NULL result ->
 * 0.0 for each.
 */
double relate_result_parse_ms(relate_query_result result);
double relate_result_plan_ms(relate_query_result result);
double relate_result_execute_ms(relate_query_result result);

/*
 * The typed value walker (ADR-0502 §1).
 *
 * Handles. A relate_value is an opaque handle to one value of a result.
 * relate_value_at() returns a cell's root; the list and map accessors
 * descend. Every handle — roots and the children minted lazily below them
 * — is OWNED BY THE RESULT: there is no per-value free, and
 * relate_destroy_result() frees every handle minted from that result
 * (a handle must not be used after it). Strings a value accessor returns
 * are result-owned the same way.
 *
 * Kinds. relate_value_kind() returns one enumerator of
 * enum relate_value_kind — one per engine value shape, discriminated
 * structurally at every depth: a user map is MAP even when it is shaped
 * like a node, an entity is NODE / RELATIONSHIP, a nested path is PATH, a
 * Cypher null is a handle of kind NULL (never a NULL handle). TIME and
 * DATETIME carry an offset (and DATETIME possibly a zone id); LOCAL_TIME
 * and LOCAL_DATETIME carry neither. Byte arrays cross as LIST of INT and
 * UUIDs as STRING. A client meeting an enumerator it does not know (a
 * later, additive ABI) treats it as a defined, reportable case.
 *
 * Out-of-kind access is a DEFINED REFUSAL, never undefined behaviour.
 * Each accessor below lists the kinds it serves; called on any other kind,
 * on a NULL handle, or with a NULL out-param, it refuses in the one form
 * its return type allows:
 *   - pointer returns (const char *, relate_value) return NULL. An
 *     in-kind call never returns NULL except where noted (list_get /
 *     map_key past the end, map_get of an absent key, an element id in the
 *     'system' namespace, which has no elements);
 *   - count returns (relate_value_kind, _list_len, _map_len,
 *     _vector_dim) return -1;
 *   - scalar reads return relate_status: RELATE_OK with the
 *     out-params written, or RELATE_ERROR with them untouched. No
 *     error_json is produced — the status is the report.
 * A refusal never poisons anything; the handle stays usable.
 */
typedef struct relate_value_s *relate_value;

enum relate_value_kind {
    RELATE_V_NULL = 0,
    RELATE_V_BOOL = 1,
    RELATE_V_INT = 2,
    RELATE_V_FLOAT = 3,
    RELATE_V_STRING = 4,
    RELATE_V_NODE = 5,
    RELATE_V_RELATIONSHIP = 6,
    RELATE_V_PATH = 7,
    RELATE_V_LIST = 8,
    RELATE_V_MAP = 9,
    RELATE_V_POINT = 10,
    RELATE_V_DATE = 11,
    RELATE_V_TIME = 12,
    RELATE_V_LOCAL_TIME = 13,
    RELATE_V_DATETIME = 14,
    RELATE_V_LOCAL_DATETIME = 15,
    RELATE_V_DURATION = 16,
    RELATE_V_VECTOR = 17
};

/*
 * The root handle of cell (row, col). NULL when either index is negative
 * or out of range (an error result has no cells) and for a NULL result.
 */
relate_value relate_value_at(relate_query_result result, long long row, long long col);

/* The value's kind as an enum relate_value_kind enumerator; -1 for a NULL handle. */
int relate_value_kind(relate_value v);

/* Scalars. BOOL -> 0/1; INT -> int64; FLOAT -> double (NaN and ±Inf included). */
relate_status relate_value_bool(relate_value v, uint8_t *out);
relate_status relate_value_int(relate_value v, int64_t *out);
relate_status relate_value_float(relate_value v, double *out);
/* FLOAT only: *out = 1 when the value is NaN / infinite (either sign), else 0. */
relate_status relate_value_is_nan(relate_value v, uint8_t *out);
relate_status relate_value_is_inf(relate_value v, uint8_t *out);

/*
 * STRING: the payload; RELATIONSHIP: the relationship type. NUL-terminated
 * UTF-8, result-owned. When len is non-NULL it receives the byte length
 * (excluding the terminator) — a string carrying an interior NUL crosses
 * whole.
 */
const char *relate_value_string(relate_value v, size_t *len);

/*
 * Entities (ADR-0502 §6). NODE / RELATIONSHIP: the element id, spelled
 * exactly as Cypher elementId() returns it ("4:relate-neo4j:n0",
 * "5:relate-neo4j:r0") at every depth, so WHERE elementId(n) = $id
 * round-trips. RELATIONSHIP only: the start / end node's element id.
 * Result-owned.
 *
 * The rest of an entity reads through the generic accessors: a NODE's
 * labels are its list (relate_value_list_len / _list_get, each a
 * STRING) and its properties its map (relate_value_map_*); a
 * RELATIONSHIP's type is its string (relate_value_string) and its
 * properties its map.
 */
const char *relate_value_element_id(relate_value v);
const char *relate_value_start_element_id(relate_value v);
const char *relate_value_end_element_id(relate_value v);

/*
 * Entities, numeric ids (ADR-0506). NODE / RELATIONSHIP: the deprecated
 * numeric entity id — exactly the INTEGER Cypher id() returns for the same
 * entity; RELATIONSHIP only: the start / end node's numeric id, as
 * id(startNode(r)) / id(endNode(r)) return them. Driver providers that
 * decide entity equality by numeric id read them here; elementId()-based
 * lookups stay on the accessors above. Wrong kind or NULL handle:
 * RELATE_ERROR with *out untouched (an entity always carries an id, so
 * there is no in-kind refusal).
 */
relate_status relate_value_id(relate_value v, int64_t *out);
relate_status relate_value_start_id(relate_value v, int64_t *out);
relate_status relate_value_end_id(relate_value v, int64_t *out);

/*
 * Lists. LIST: its items; PATH: its elements alternating node,
 * relationship, ..., node (2n+1 handles for n hops); NODE: its labels.
 * list_get returns NULL past the end.
 */
long long relate_value_list_len(relate_value v);
relate_value relate_value_list_get(relate_value v, long long i);

/*
 * Maps. MAP: its entries; NODE / RELATIONSHIP: their properties. Entries
 * keep the engine's order. map_key returns key i (NUL-terminated UTF-8,
 * result-owned; len as for relate_value_string) and NULL past the end;
 * map_get returns the value under a NUL-terminated UTF-8 key and NULL when
 * the key is absent — a present key holding Cypher null returns a handle
 * of kind NULL.
 */
long long relate_value_map_len(relate_value v);
const char *relate_value_map_key(relate_value v, long long i, size_t *len);
relate_value relate_value_map_get(relate_value v, const char *key);

/*
 * Temporals as components (ADR-0502 §1) — ISO text is a client rendering
 * choice. Zoned kinds report their local wall clock.
 *   year, month (1-12), day (1-31): DATE, DATETIME, LOCAL_DATETIME
 *   hour, minute, second, nanosecond (sub-second, 0-999999999):
 *     TIME, LOCAL_TIME, DATETIME, LOCAL_DATETIME
 *   offset_seconds (UTC offset): TIME, DATETIME
 *   zone: TIME, DATETIME — the IANA zone id ("Europe/Stockholm"), or ""
 *     when the value carries only an offset (always "" for TIME).
 *     Result-owned.
 */
relate_status relate_value_year(relate_value v, int64_t *out);
relate_status relate_value_month(relate_value v, int64_t *out);
relate_status relate_value_day(relate_value v, int64_t *out);
relate_status relate_value_hour(relate_value v, int64_t *out);
relate_status relate_value_minute(relate_value v, int64_t *out);
relate_status relate_value_second(relate_value v, int64_t *out);
relate_status relate_value_nanosecond(relate_value v, int64_t *out);
relate_status relate_value_offset_seconds(relate_value v, int64_t *out);
const char *relate_value_zone(relate_value v);

/*
 * DURATION as its four lossless components: months, days, seconds and
 * nanoseconds (duration('P1Y') reads months = 12). Every out-param must be
 * non-NULL.
 */
relate_status relate_value_duration(relate_value v, int64_t *months, int64_t *days, int64_t *seconds, int64_t *nanos);

/*
 * POINT: srid, dimension (2 or 3) and coordinates (x/y = longitude/latitude
 * for a geographic srid). z is written only when *dim is 3. Every
 * out-param must be non-NULL.
 */
relate_status relate_value_point(relate_value v, int32_t *srid, int32_t *dim, double *x, double *y, double *z);

/*
 * VECTOR: the coordinate dtype — the engine's canonical coordinate-type
 * name: INTEGER8, INTEGER16, INTEGER32, INTEGER, FLOAT16, BFLOAT16, FLOAT32
 * or FLOAT (result-owned) — its dimension, and coordinate i. vector_int
 * serves the four INTEGER* dtypes, vector_float the four FLOAT* dtypes;
 * the other one, or an index out of range, refuses.
 */
const char *relate_value_vector_dtype(relate_value v);
long long relate_value_vector_dim(relate_value v);
relate_status relate_value_vector_float(relate_value v, long long i, double *out);
relate_status relate_value_vector_int(relate_value v, long long i, int64_t *out);

/*
 * The typed params builder (ADR-0502 §4) — the inbound mirror of the value
 * walker above: each putter binds one kind from the same components the
 * walker reads back, with no JSON on the way.
 *
 * Builders. relate_params_new() returns a root: the statement's MAP of
 * named parameters. relate_params_put_list() / _put_map() insert a
 * nested LIST / MAP and write its child builder to *child; a child takes
 * the same putters. The container decides the name rule: a MAP takes a
 * non-NULL, unique UTF-8 name; a LIST takes a NULL name and appends.
 *
 * Lifetime. One tree, one owner: the root owns every child. A child
 * handle stays valid — and open for puts — until relate_params_free()
 * frees its root; it is never freed itself (relate_params_free() on a
 * child is a no-op). There is no build or close step: the builder is read
 * as it stands when relate_query_params() runs. A builder must not be
 * used from two threads at once (the connection contract).
 *
 * Refusals. Every putter returns relate_status. A NULL builder, a name
 * that breaks the container's rule, a NULL required pointer, invalid
 * UTF-8, an out-of-range component, an unknown zone, a srid/dimension
 * mismatch, a coordinate the vector dtype cannot hold, or nesting past the
 * engine's value-depth cap is a DEFINED REFUSAL, never undefined
 * behaviour: RELATE_ERROR with the error shape (internal stage) in
 * *error_json (NULL opts out), and the builder unchanged and usable.
 *
 * Fixed caps (OPLANE_REQ-00340363/57): 100000 entries per list/map
 * container, 4096 bytes per parameter name, and a builder-wide byte
 * budget of 16 MiB (the one wire-message bound) covering every entry's
 * name, value bytes and a fixed overhead across all nested containers —
 * put_string refuses an over-budget length before copying its bytes.
 * Each is a defined refusal one past its limit, in the same shape as
 * every other refusal.
 *
 * Trusted-length contract: a `len`/`dim` argument and the buffer behind
 * it are trusted — Rust cannot detect a short or unterminated buffer, so
 * passing one is undefined behaviour. Name buffers must additionally be
 * NUL-terminated (the length-bearing `put_string` value buffer must not
 * contain interior NULs only for UTF-8's sake; names are read as C
 * strings).
 *
 * Components (int64_t, as the walker reads them): month 1-12, day 1-31
 * (a real calendar date), hour 0-23, minute 0-59, second 0-59, nanosecond
 * 0-999999999, offset_seconds within ±18:00 (±64800).
 */
relate_params relate_params_new(void);
/* Free a root and every child below it. NULL and child handles: no-op. */
void relate_params_free(relate_params p);

/* Scalars. bool: 0 is false, any other value true. */
relate_status relate_params_put_null(relate_params p, const char *name, char **error_json);
relate_status relate_params_put_bool(relate_params p, const char *name, uint8_t v, char **error_json);
relate_status relate_params_put_int(relate_params p, const char *name, int64_t v, char **error_json);
relate_status relate_params_put_float(relate_params p, const char *name, double v, char **error_json);
/* STRING: len bytes of UTF-8 at v (an interior NUL crosses whole). A NULL
 * v with no bytes binds the empty string — a marshalling layer may hand a
 * zero-length buffer over as NULL (koffi does); NULL with bytes to read
 * refuses. */
relate_status relate_params_put_string(relate_params p, const char *name, const char *v, size_t len, char **error_json);

/*
 * Temporals from components.
 *   DATE, LOCAL_TIME, LOCAL_DATETIME: the wall clock only.
 *   TIME: the wall clock and offset_seconds. A non-NULL zone (an IANA
 *     region id, "America/New_York") supplies the offset instead, resolved
 *     at the current instant as Cypher's time({timezone: ...}) does; a
 *     TIME keeps no zone id (the walker's zone reads "").
 *   DATETIME: the local wall clock and offset_seconds — or, with a
 *     non-NULL zone (an IANA region id, "Europe/Stockholm"), the zone,
 *     which the value keeps (relate_value_zone reads it back). The
 *     offset is then resolved from the zone at that wall clock and
 *     offset_seconds is ignored: a DST overlap takes the earlier offset
 *     (ADR-0178); a wall clock inside a DST gap refuses. Offset-only zones
 *     ("Z", "+01:00") pass as offset_seconds with a NULL zone.
 */
relate_status relate_params_put_date(relate_params p, const char *name, int64_t year, int64_t month, int64_t day, char **error_json);
relate_status relate_params_put_time(relate_params p, const char *name, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, int64_t offset_seconds, const char *zone, char **error_json);
relate_status relate_params_put_local_time(relate_params p, const char *name, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, char **error_json);
relate_status relate_params_put_datetime(relate_params p, const char *name, int64_t year, int64_t month, int64_t day, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, int64_t offset_seconds, const char *zone, char **error_json);
relate_status relate_params_put_local_datetime(relate_params p, const char *name, int64_t year, int64_t month, int64_t day, int64_t hour, int64_t minute, int64_t second, int64_t nanosecond, char **error_json);

/*
 * DURATION from its four lossless components (duration('P1Y2M') is
 * months = 14). Any int64_t values are accepted; the walker reads back the
 * engine's normalized seconds/nanoseconds split.
 */
relate_status relate_params_put_duration(relate_params p, const char *name, int64_t months, int64_t days, int64_t seconds, int64_t nanos, char **error_json);

/*
 * POINT: one of the four point() srids with its dimension — 7203
 * (cartesian) and 4326 (WGS-84, x/y = longitude/latitude) with z NULL,
 * 9157 (cartesian-3d) and 4979 (WGS-84-3d) with z pointing at the third
 * coordinate. Any other pairing refuses.
 */
relate_status relate_params_put_point(relate_params p, const char *name, int32_t srid, double x, double y, const double *z, char **error_json);

/*
 * VECTOR: dtype is the engine's canonical coordinate-type name, as
 * relate_value_vector_dtype returns it. put_vector_float serves the
 * four FLOAT* dtypes (FLOAT16, BFLOAT16, FLOAT32, FLOAT) from dim doubles,
 * each exactly representable in the dtype (a FLOAT32 vector takes doubles
 * widened from float); put_vector_int serves the four INTEGER* dtypes
 * (INTEGER8, INTEGER16, INTEGER32, INTEGER) from dim int64_t values, each
 * within the dtype's width. The other dtype family, an inexact or
 * out-of-range coordinate, or NULL values with dim > 0 refuses.
 */
relate_status relate_params_put_vector_float(relate_params p, const char *name, const char *dtype, const double *values, size_t dim, char **error_json);
relate_status relate_params_put_vector_int(relate_params p, const char *name, const char *dtype, const int64_t *values, size_t dim, char **error_json);

/*
 * Nested containers: insert an empty LIST / MAP under name and write its
 * child builder to *child (owned by the root — see Lifetime above). A
 * NULL child out-param refuses; on a refusal *child is untouched.
 */
relate_status relate_params_put_list(relate_params p, const char *name, relate_params *child, char **error_json);
relate_status relate_params_put_map(relate_params p, const char *name, relate_params *child, char **error_json);

/*
 * Destroy a result and every value handle and string it owns. Safe to
 * call with NULL.
 */
void relate_destroy_result(relate_query_result result);

#ifdef __cplusplus
}
#endif

#endif /* RELATE_H */
