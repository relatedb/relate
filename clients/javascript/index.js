"use strict";

const koffi = require("koffi");
const fs = require("fs");
const { ResultGraph, collectGraphEntities } = require("./graph");
const createValueCodec = require("./values");
const {
  RelateError,
  abiViolation,
  clientRefusalError,
  errorFromJson,
} = require("./errors");
const { decodeQueryType, decodeNamespace } = require("./metadata");
const path = require("path");

// Platform support, keyed by `${process.platform}-${process.arch}`; a key
// absent from the table is a combination release.yml does not build and
// resolves through UNKNOWN_PLATFORM below. `lib` is the single library
// filename release.yml ships into the platform package's lib/ directory;
// `repoExts` is the wider extension list the repo target/ fallback probes
// (a darwin repo build can produce .so from cross-builds). `repoBase`
// overrides the `librelate_ffi` filename stem the repo fallback probes
// — cargo names the Windows DLL relate_ffi.dll (no lib prefix). The npm
// platform package name is derived from the key:
// `@relate/relate-${key}` (ADR-0562).
const PLATFORMS = {
  "linux-x64": { lib: "librelate_ffi.so", repoExts: ["so"] },
  "linux-arm64": { lib: "librelate_ffi.so", repoExts: ["so"] },
  "darwin-x64": { lib: "librelate_ffi.dylib", repoExts: ["dylib", "so"] },
  "darwin-arm64": { lib: "librelate_ffi.dylib", repoExts: ["dylib", "so"] },
  "win32-x64": { lib: "relate_ffi.dll", repoBase: "relate_ffi", repoExts: ["dll"] },
};

// Fallback entry for a platform/arch release.yml does not build: no
// platform package exists to probe (lib: null skips that stage), and the
// repo fallback probes every extension under the default filename stem.
const UNKNOWN_PLATFORM = { lib: null, repoBase: "librelate_ffi", repoExts: ["so", "dylib", "dll"] };

function findLib() {
  // (a) Explicit override.
  if (process.env.RELATE_LIB) return process.env.RELATE_LIB;
  const key = `${process.platform}-${process.arch}`;
  const { lib, repoBase = "librelate_ffi", repoExts } = PLATFORMS[key] ?? UNKNOWN_PLATFORM;
  // The platform package name; the placeholder form names the shape when
  // this platform/arch has no published package.
  const pkg = lib ? `@relate/relate-${key}` : "@relate/relate-<platform>-<arch>";
  // (b) The platform optionalDependency (published npm installs). A missing
  // package is not an error; keep looking. Only the shipped lib filename is
  // probed; the wider repo-fallback list lives in repoExts.
  if (lib) {
    try {
      const pkgDir = path.dirname(require.resolve(`${pkg}/package.json`));
      const p = path.join(pkgDir, "lib", lib);
      if (fs.existsSync(p)) return p;
    } catch {
      // Platform package not installed; fall through to the repo build.
    }
  }
  // (c) Repo build output (development fallback).
  const root = path.resolve(__dirname, "../..");
  for (const dir of ["debug", "release"]) {
    for (const ext of repoExts) {
      const p = path.join(root, "target", dir, `${repoBase}.${ext}`);
      if (fs.existsSync(p)) return p;
    }
  }
  throw new Error(
    "librelate_ffi not found; looked in " +
      "(1) the RELATE_LIB environment variable, " +
      `(2) the platform package ${pkg} (lib/ directory), ` +
      "(3) the repo target/debug and target/release directories. " +
      "Set RELATE_LIB to the library path."
  );
}

const lib = koffi.load(findLib());

// ABI v2 gate (ADR-0502): resolve the version symbol before anything else.
// A library without it predates the version symbol; a library with a
// different version is one this client was never built against. Failing
// here names the problem instead of a missing walker symbol surfacing
// later at the first query.
let abi_version;
try {
  abi_version = lib.func("int relate_abi_version()");
} catch {
  throw new Error(
    "incompatible librelate_ffi: no relate_abi_version symbol " +
      "(the library predates the ABI version symbol); this client requires ABI v2"
  );
}
if (abi_version() !== 2) {
  throw new Error(
    `incompatible librelate_ffi: ABI version ${abi_version()}; this client requires 2`
  );
}

const free_string = lib.func("void relate_free_string(void *s)");
const database_open = lib.func(
  "int relate_database_open(str uri, _Out_ void **out, _Out_ void **err)"
);
const database_open_read_only = lib.func(
  "int relate_database_open_read_only(str uri, _Out_ void **out, _Out_ void **err)"
);
const database_close = lib.func("int relate_database_close(void *db)");
// Canonical entry points only (ADR-0220): relate_connect_namespace with a
// NULL name is the one connect; relate_query_params is the one query.
// The v1 aliases stay in no build this client loads.
const connect_namespace = lib.func(
  "int relate_connect_namespace(void *db, str name, _Out_ void **out, _Out_ void **err)"
);
const conn_set_namespace = lib.func(
  "int relate_conn_set_namespace(void *conn, str name, _Out_ void **err)"
);
const disconnect = lib.func("int relate_disconnect(void *conn)");
const query_params = lib.func(
  "int relate_query_params(void *conn, str cypher, void *params, _Out_ void **out, _Out_ void **err)"
);
const result_is_error = lib.func("uint8_t relate_result_is_error(void *r)");
// Result-owned `str` returns (koffi copies them during marshalling): the
// error shape, the bookmark, column names, metadata.
const result_error_json = lib.func("str relate_result_error_json(void *r)");
const result_bookmark = lib.func("str relate_result_bookmark(void *r)");
const column_count = lib.func("int64 relate_column_count(void *r)");
const row_count = lib.func("int64 relate_row_count(void *r)");
const column_name = lib.func("str relate_column_name(void *r, int64 col)");
const result_query_type = lib.func("str relate_result_query_type(void *r)");
const result_namespace = lib.func("str relate_result_namespace(void *r)");
const result_stats = lib.func("str relate_result_stats(void *r)");
// The statement's query notifications (ADR-0363) as a JSON array string in
// the engine-native shape (code, gql_status, severity, category, title,
// description, remedy, plus operator_id on operator-scoped kinds). The array
// is always present — `[]` on a plain run.
const result_notifications = lib.func("str relate_result_notifications(void *r)");
// The statement's structured plan (LABS-418, ADR-0381) as a
// `relate.plan/v1` JSON document, or NULL when the statement carried no
// plan (a plain run).
const result_plan_json = lib.func("str relate_result_plan_json(void *r)");
const result_elapsed_ms = lib.func("double relate_result_elapsed_ms(void *r)");
// The statement's phase split (ADR-0491): three disjoint sub-windows of the
// elapsed total above, measured always-on by the engine.
const result_parse_ms = lib.func("double relate_result_parse_ms(void *r)");
const result_plan_ms = lib.func("double relate_result_plan_ms(void *r)");
const result_execute_ms = lib.func("double relate_result_execute_ms(void *r)");
const destroy_result = lib.func("void relate_destroy_result(void *r)");
const checkpoint = lib.func("int relate_checkpoint(void *db, _Out_ void **err)");
const conn_begin = lib.func("int relate_conn_begin(void *conn, _Out_ void **err)");
// Commit rides the status convention (ADR-0510): the status first, the
// caller-owned bookmark (ADR-0502 §7) through a bookmark_out out-array —
// decode then free, never a `str` return, which koffi would copy while the
// native string leaks — and the error shape through the trailing error
// out-param, the same two-out-array shape as connect_namespace.
const conn_commit = lib.func(
  "int relate_conn_commit(void *conn, _Out_ void **bm, _Out_ void **err)"
);
const conn_rollback = lib.func("int relate_conn_rollback(void *conn, _Out_ void **err)");
const value_at = lib.func("void *relate_value_at(void *r, int64 row, int64 col)");

// The two directions of the typed value channel (values.js): the walker
// decode for cells, the builder for parameters, one class vocabulary.
const {
  decodeValue, buildParams, paramsFree, isPlainParamsObject,
  CypherFloat, CypherDate, CypherTime, CypherLocalTime, CypherDateTime,
  CypherLocalDateTime, CypherDuration, CypherPoint, CypherVector, float,
} = createValueCodec(lib);

// --- The error channel (ADR-0502 §2) --------------------------------------
//
// One error shape travels with the call that failed: a statement failure
// rides the result handle, every other failure fills the call's trailing
// error_json out-param. No thread-local slot remains, so an async call on
// koffi's worker thread reports its own error to whoever reads its outputs.
// The shape's parsing and typed exception live in errors.js; this module
// owns the FFI half — reading and freeing the out-array element.

/// Copy and free a caller-owned error_json out-array element: koffi wrote
/// element 0 on failure. The error JSON is NUL-free JSON text, so the
/// NUL-terminated decode is exact here (cell strings are not — values.js
/// reads those by length). A NULL element after a failed status breaks the
/// ABI's contract (the failure was to fill it) — a violation naming the
/// call, never a fabricated error without classification (the Go client's
/// same rule).
function takeError(what, errOut) {
  const p = errOut[0];
  errOut[0] = null;
  if (p === null) {
    throw abiViolation(`${what} failed without an error detail`);
  }
  const json = koffi.decode(p, "char", -1);
  free_string(p);
  return errorFromJson(json);
}

/// A putter refusal during parameter binding: the same error shape through
/// the same takeError. The binder's client-side rejections (values.js
/// putValue's TypeErrors — an unsupported leaf, a cycle, a non-plain
/// object) throw before any putter runs; only the ABI's own refusals ride
/// this path.
function putterRefusal(errOut) {
  throw takeError("binding a parameter", errOut);
}

// --- Result materialization (ADR-0216 / ADR-0285) -------------------------

/// Read every result-owned value off the live C result handle — metadata
/// strings, stats and notifications JSON, the bookmark, columns, and every
/// cell through the typed value walker — into plain JS values. The ADR-0285
/// lifetime rule (all reads happen before destroy_result; koffi's `str`
/// return copies during marshalling) lives in the caller (readResult), so
/// no call site can reorder it away.
function materialize(result) {
  const queryType = decodeQueryType(result_query_type(result));
  const namespace = decodeNamespace(result_namespace(result));
  const stats = JSON.parse(result_stats(result));
  // The always-present notification list (ADR-0363): `[]` on a plain run,
  // the engine-native objects on EXPLAIN and PROFILE.
  const notifications = JSON.parse(result_notifications(result));
  // The structured plan (LABS-418, ADR-0381): the relate.plan/v1
  // document, or null on a plain run (the accessor returns NULL then —
  // koffi marshals a NULL `str` as null).
  const planRaw = result_plan_json(result);
  const plan = planRaw === null ? null : JSON.parse(planRaw);
  // Engine-reported statement elapsed in wall-clock milliseconds
  // (ADR-0221) and its phase split (ADR-0491), read while the result
  // handle is alive.
  const elapsedMs = result_elapsed_ms(result);
  const parseMs = result_parse_ms(result);
  const planMs = result_plan_ms(result);
  const executeMs = result_execute_ms(result);
  // The auto-commit bookmark (ADR-0502 §7): result-owned, copied by koffi;
  // null when the statement committed nothing (a read, an EXPLAIN, a
  // statement inside an explicit transaction, an error result).
  const bookmark = result_bookmark(result);
  const cols = Number(column_count(result));
  const rowsN = Number(row_count(result));
  const columns = [];
  for (let i = 0; i < cols; i++) columns.push(column_name(result, i));
  const rows = [];
  for (let r = 0; r < rowsN; r++) {
    // Rows are objects keyed by column name (ADR-0216) — the JS/TS idiom.
    // Object.fromEntries defines properties instead of assigning them, so a
    // `__proto__` column alias cannot swap the row's prototype.
    const entries = [];
    for (let c = 0; c < cols; c++) {
      // The typed value walker (ADR-0502 §1): one decode per cell, no JSON
      // payload in between.
      entries.push([columns[c], decodeValue(value_at(result, r, c))]);
    }
    rows.push(Object.fromEntries(entries));
  }
  return {
    columns,
    rows,
    bookmark,
    stats,
    notifications,
    plan,
    queryType,
    namespace,
    elapsedMs,
    parseMs,
    planMs,
    executeMs,
  };
}

/// One query result, start to finish: the error-result check (a statement
/// failure rides the result — RELATE_OK, not an error status), the
/// materialization, and the destroy — in that order, with the destroy in a
/// finally so a decode throw cannot leak the native handle.
function readResult(result) {
  try {
    if (result_is_error(result) === 1) {
      throw errorFromJson(result_error_json(result));
    }
    return materialize(result);
  } finally {
    destroy_result(result);
  }
}

/// Thrown by Result#single() on zero or more than one row, like the Neo4j
/// drivers' ResultNotSingleError.
class ResultNotSingleError extends Error {
  constructor(rowCount) {
    super(`expected exactly one row, got ${rowCount}`);
    this.name = "ResultNotSingleError";
    this.rowCount = rowCount;
  }
}

/// The metadata of one executed statement, as the Neo4j driver's
/// ResultSummary: the 11 mutation `counters` (the bundle's `stats`), the
/// resolved `namespace` and `queryType` (ADR-0285), `elapsedMs` — the
/// engine-reported statement elapsed (ADR-0221) — its phase split
/// `parseMs`/`planMs`/`executeMs` (ADR-0491), the `notifications` list
/// (ADR-0363), the `plan` document (ADR-0381), and the auto-commit
/// `bookmark` (ADR-0502 §7). The constructor takes the same materialized
/// bundle Result receives, so statement metadata has one shape.
class ResultSummary {
  constructor({
    stats,
    namespace,
    queryType,
    elapsedMs,
    parseMs,
    planMs,
    executeMs,
    notifications,
    plan,
    bookmark,
  }) {
    this.counters = stats;
    this.namespace = namespace;
    this.queryType = queryType;
    this.elapsedMs = elapsedMs;
    this.parseMs = parseMs;
    this.planMs = planMs;
    this.executeMs = executeMs;
    this.notifications = notifications;
    this.plan = plan;
    this.bookmark = bookmark;
  }
}

/// One executed statement's result (ADR-0216): `columns`, `rows` (objects
/// keyed by column name), `stats` (the 11 mutation counters, snake_case),
/// `queryType` ("r"/"w"/"rw"/"s"), `namespace` (resolved, ADR-0285),
/// `notifications` (ADR-0363), `plan` (ADR-0381) and `bookmark`
/// (ADR-0502 §7). Rows are materialized at query time — there is no fetch
/// position. Cell values are the typed walker's JS representations
/// (ADR-0502 §1) — see docs/api/javascript/spec.md for the full table.
class Result {
  // One bundle in: the materialized result (columns, rows, bookmark, stats,
  // metadata, notifications, engine-reported elapsedMs). The summary is the
  // single source of the statement metadata and is built from that same
  // bundle; the accessors delegate to it (the Python side's F-4-5 lesson).
  constructor(metadata) {
    this.columns = metadata.columns;
    this.rows = metadata.rows;
    this._summary = new ResultSummary(metadata);
  }

  get stats() {
    return this._summary.counters;
  }

  get queryType() {
    return this._summary.queryType;
  }

  get namespace() {
    return this._summary.namespace;
  }

  /// The statement's auto-commit bookmark ("relate:<n>", ADR-0502 §7),
  /// or null when it committed nothing — a read, an EXPLAIN, a statement
  /// inside an explicit transaction (that commit's return value is the
  /// bookmark), or an error result.
  get bookmark() {
    return this._summary.bookmark;
  }

  /// The statement's query notifications (ADR-0363) as plain objects in the
  /// engine-native shape: `code`, `gql_status`, `severity`, `category`,
  /// `title`, `description`, `remedy`, plus `operator_id` on operator-scoped
  /// kinds. Empty on a plain run.
  get notifications() {
    return this._summary.notifications;
  }

  /// The statement's structured plan (LABS-418, ADR-0381): a
  /// `relate.plan/v1` document, or null on a plain run. Each node's
  /// `warnings` carry the engine's remedy-bearing notifications (ADR-0363)
  /// joined by operator id, so the diagnosis and its remedy ride together.
  get plan() {
    return this._summary.plan;
  }

  /// The one row of a single-row result; throws ResultNotSingleError on
  /// zero or more than one (the Neo4j drivers' Result.single()).
  single() {
    if (this.rows.length === 1) return this.rows[0];
    throw new ResultNotSingleError(this.rows.length);
  }

  /// The statement's ResultSummary. Idempotent; rows stay accessible
  /// (they are already materialized, so unlike the network drivers there
  /// is nothing to discard).
  consume() {
    return this._summary;
  }

  /// The result's unique graph entities — every node, relationship and
  /// path the rows carry, nested values walked — as a ResultGraph.
  /// A result with no graph entities yields an empty ResultGraph.
  graph() {
    return collectGraphEntities(this.rows);
  }
}

class Database {
  constructor(handle) {
    this.handle = handle;
  }
  // open(uri, { readOnly: true }) loads once and refuses writes (ADR-0136).
  // Omitting the options keeps the writable open.
  static open(uri, options) {
    const out = [null];
    const err = [null];
    const openFn = options && options.readOnly ? database_open_read_only : database_open;
    if (openFn(uri, out, err) !== 0) throw takeError("database_open", err);
    return new Database(out[0]);
  }
  // connect(namespace?) — namespace is "neo4j" (default), "system" or
  // "system_ontologies", matched case-insensitively (ADR-0117). Omitting it
  // is equivalent to connect("neo4j"): a null name reaches the ABI as NULL,
  // which relate_connect_namespace treats as "neo4j" (ADR-0220).
  connect(namespace) {
    const out = [null];
    const err = [null];
    const rc = connect_namespace(
      this.handle,
      namespace == null ? null : String(namespace),
      out,
      err
    );
    if (rc !== 0) throw takeError("connect_namespace", err);
    return new Connection(out[0]);
  }
  checkpoint() {
    const err = [null];
    if (checkpoint(this.handle, err) !== 0) throw takeError("checkpoint", err);
  }
}

// --- Query argument handling (incl. tagged templates, ADR-0222) ----------

/// Lower a tagged-template call site to {text, params}: each interpolated
/// value becomes an auto-named $p0, $p1, … parameter, so values never enter
/// the statement text — injection-safe by construction. The same primitive
/// value (SameValueZero, Map-keyed) interpolated twice maps to one
/// parameter; objects dedup by identity. Unsupported values reject when the
/// params builder walks them (values.js), naming the interpolation.
function buildTemplate(strings, values) {
  const params = {};
  const seen = new Map(); // value (primitives) / identity (objects) -> name
  let text = strings[0];
  for (let i = 0; i < values.length; i++) {
    const v = values[i];
    let name;
    if (seen.has(v)) {
      name = seen.get(v);
    } else {
      name = `p${seen.size}`;
      seen.set(v, name);
      params[name] = v;
    }
    text += `$${name}${strings[i + 1]}`;
  }
  return { text, params };
}

/// Validate the two call shapes — the string form query(cypher, params?)
/// and the tagged-template form (ADR-0222) — down to one total
/// {text, params} record (params is always an object), so query() itself is
/// one linear path. The deep value policy is the params builder's
/// (values.js putValue): binding is validation.
function resolveQueryArgs(cypher, rest) {
  if (typeof cypher === "string") {
    if (rest.length > 1) {
      throw new TypeError("query takes (cypher, params?) — extra arguments");
    }
    const params = rest[0] ?? {};
    if (!isPlainParamsObject(params)) {
      throw new TypeError("query params must be a plain object");
    }
    return { text: cypher, params };
  }
  if (!Array.isArray(cypher) || !Array.isArray(cypher.raw)) {
    throw new TypeError("query expects a Cypher string or a tagged template");
  }
  return buildTemplate(cypher, rest);
}

/// The one finish for a query call, both forms: the status check (a
/// connection failure throws through the error out-array) and the result
/// materialization (a statement failure throws from readResult, riding
/// the result handle) — the sync path returns it, the async callback
/// settles through it.
function queryResult(out, err, rc) {
  if (rc !== 0) throw takeError("query", err);
  return new Result(readResult(out[0]));
}

/// One call at a time per connection — the ABI's connection contract. The
/// async form runs on koffi's worker thread while the event loop continues,
/// so overlapping awaits on one connection are chained through this queue;
/// the walker decode stays inside the queued task (it is synchronous and
/// cheap). The executor is also the one owner of the async state: a task
/// counts as in flight from its start until its promise settles — exactly
/// the window its FFI call is on koffi's worker thread — and `busy()`
/// reports it, so the sync-call guard reads the same state the queue
/// serializes on instead of a hand-maintained counter beside it.
class SerialExecutor {
  #tail = Promise.resolve();
  #inFlight = 0;
  run(task) {
    // #tail only ever holds a caught promise (or the resolved seed), so
    // one fulfill arm is the whole queue — there is no rejection to route
    // around.
    const next = this.#tail.then(() => this.#tracked(task));
    this.#tail = next.catch(() => {});
    return next;
  }

  /// True while a task is between its start and its promise settling.
  busy() {
    return this.#inFlight > 0;
  }

  /// Run one task with the in-flight window counted: increment at the
  /// start, decrement when the task's promise settles (or it throws
  /// synchronously) — so a dispatch that fails before its callback ever
  /// fires cannot leak the window.
  #tracked(task) {
    this.#inFlight++;
    let result;
    try {
      result = task();
    } catch (e) {
      this.#inFlight--;
      throw e;
    }
    return Promise.resolve(result).finally(() => {
      this.#inFlight--;
    });
  }
}

class Connection {
  constructor(handle) {
    this.handle = handle;
    // The one async-state owner: the serial executor. Its queue serializes
    // async calls against each other, and its busy() covers the one
    // overlap the queue cannot see — a synchronous call (query, begin,
    // commit, rollback, setNamespace) running on the main thread while a
    // queued async call's FFI call is still on koffi's worker thread.
    this._serial = new SerialExecutor();
  }

  /// The one async-overlap policy for every synchronous FFI call on this
  /// connection: refuse definedly, not a silent contract break — one call
  /// at a time is the ABI's connection contract, and the in-flight async
  /// call owns the connection right now. clientRefusalError (errors.js) is
  /// the one RelateError built without the ABI's error shape.
  _refuseWhileAsyncInFlight(op) {
    if (this._serial.busy()) {
      throw clientRefusalError(
        `a synchronous ${op} cannot start while an async query is in flight on this connection`
      );
    }
  }

  // query(cypher, params?) — the documented primary form — or the
  // tagged-template form (ADR-0222): conn.query`MATCH (p {name: ${name}})
  // RETURN p`. The tag lowers to the params path before the C ABI boundary.
  query(cypher, ...rest) {
    const { text, params } = resolveQueryArgs(cypher, rest);
    this._refuseWhileAsyncInFlight("query");
    return this._querySync(text, params);
  }

  /// The Promise-returning form of query (same two call shapes). The call
  /// runs on koffi's worker thread; the statement error rides the result
  /// handle and a connection failure the error out-array, so both come back
  /// with the callback and reject the promise — the v1 thread-local error
  /// slot lost exactly these. Overlapping calls on one connection are
  /// serialized (one call at a time is the ABI's connection contract).
  queryAsync(cypher, ...rest) {
    const { text, params } = resolveQueryArgs(cypher, rest);
    return this._serial.run(() => this._queryAsync(text, params));
  }

  _querySync(text, params) {
    const root = buildParams(params, putterRefusal);
    const out = [null];
    const err = [null];
    let rc;
    try {
      rc = query_params(this.handle, text, root, out, err);
    } finally {
      // The builder was read, not consumed; either way it is done now.
      // paramsFree takes the null no-params root too.
      paramsFree(root);
    }
    return queryResult(out, err, rc);
  }

  _queryAsync(text, params) {
    // Fresh out-arrays per call: sharing them between concurrent calls races.
    const out = [null];
    const err = [null];
    const root = buildParams(params, putterRefusal);
    // One finalizer owns the builder's lifetime — the same policy as
    // _querySync's finally: the promise settles either when the callback
    // fires (the call has returned; the builder is done being read) or
    // when the dispatch itself throws (the call never started), and both
    // free here.
    return new Promise((resolve, reject) => {
      query_params.async(this.handle, text, root, out, err, (callError, rc) => {
        // The in-flight window is the executor's (#tracked in SerialExecutor):
        // it closes when this callback settles the promise.
        try {
          if (callError) return reject(callError);
          // The walker decode inside queryResult is synchronous and cheap;
          // run it here, on the main thread, inside the queued task.
          resolve(queryResult(out, err, rc));
        } catch (e) {
          reject(e);
        }
      });
    }).finally(() => paramsFree(root));
  }

  begin() {
    this._refuseWhileAsyncInFlight("begin");
    const err = [null];
    if (conn_begin(this.handle, err) !== 0) throw takeError("begin", err);
  }

  /// Commit the explicit transaction. Returns its bookmark
  /// ("relate:<n>", the engine's commit sequence — ADR-0502 §7), or
  /// null when the transaction committed nothing (no write ran, or every
  /// write was a no-op): no bookmark is fabricated. A failed commit throws
  /// RelateError through the same status/error channel as every other
  /// call (ADR-0510) — the status alone tells a failure from an empty
  /// commit, so a failure never masquerades as null (a retryable one
  /// belongs in the caller's retry loop).
  commit() {
    this._refuseWhileAsyncInFlight("commit");
    const bm = [null];
    const err = [null];
    if (conn_commit(this.handle, bm, err) !== 0) throw takeError("commit", err);
    // RELATE_OK with a NULL bookmark: the transaction committed nothing.
    if (bm[0] === null) return null;
    const bookmark = koffi.decode(bm[0], "char", -1);
    free_string(bm[0]);
    return bookmark;
  }

  rollback() {
    this._refuseWhileAsyncInFlight("rollback");
    const err = [null];
    if (conn_rollback(this.handle, err) !== 0) throw takeError("rollback", err);
  }

  // setNamespace(name) — change the session namespace between transactions
  // (ADR-0117). Case-insensitive; rejects while an explicit transaction is
  // open.
  setNamespace(name) {
    this._refuseWhileAsyncInFlight("setNamespace");
    const err = [null];
    if (conn_set_namespace(this.handle, String(name), err) !== 0) {
      throw takeError("conn_set_namespace", err);
    }
  }
}

module.exports = {
  Database,
  Connection,
  Result,
  ResultSummary,
  ResultGraph,
  ResultNotSingleError,
  RelateError,
  CypherFloat,
  CypherDate,
  CypherTime,
  CypherLocalTime,
  CypherDateTime,
  CypherLocalDateTime,
  CypherDuration,
  CypherPoint,
  CypherVector,
  float,
};
