const { Database } = require("./index");
const assert = require("assert");
const path = require("path");
const Module = require("module");

// Doc examples require("@relate/relate"); resolve that to this package.
const origResolve = Module._resolveFilename;
Module._resolveFilename = function (request, ...args) {
  if (request === "@relate/relate") return path.resolve(__dirname, "index.js");
  return origResolve.call(this, request, ...args);
};

function assertRejectsSuffix(fn, suffix) {
  let err = null;
  try {
    fn();
  } catch (e) {
    err = e;
  }
  assert(err, `expected error ending ${JSON.stringify(suffix)}`);
  assert(
    err.message.endsWith(suffix),
    `message ${JSON.stringify(err.message)} does not end with ${JSON.stringify(suffix)}`
  );
}

const db = Database.open(":memory:");
const conn = db.connect();
const r = conn.query("RETURN 1 AS n");
if (r.rows[0].n !== 1) {
  throw new Error("RETURN 1 failed");
}
conn.query("CREATE (n:Person {name: 'Ada'})");
const r2 = conn.query("MATCH (n:Person) RETURN n.name AS name");
if (r2.rows[0].name !== "Ada") {
  throw new Error("MATCH failed");
}
const w = db.connect();
const other = db.connect();
w.begin();
w.query("CREATE (n:Txn {name: 'Bob'})");
const isolated = other.query("MATCH (n:Txn) RETURN count(n) AS n");
if (isolated.rows[0].n !== 0) {
  throw new Error("uncommitted write leaked");
}
w.commit();
const after = other.query("MATCH (n:Txn) RETURN n.name AS name");
if (after.rows[0].name !== "Bob") {
  throw new Error("commit not visible");
}
// Namespace API (ADR-0117). system_ontologies is listed but not
// queryable (ADR-0229): selecting it works; any query fails 42N17.
const defaultConn = db.connect(); // omitting the argument keeps "neo4j"
assert.strictEqual(defaultConn.query("RETURN 1 AS n").rows[0].n, 1);
const sysConn = db.connect("system_ontologies");
assertRejectsSuffix(
  () => sysConn.query("RETURN 1 AS n"),
  "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead."
);
// setNamespace is case-insensitive.
sysConn.setNamespace("NEO4J");
sysConn.setNamespace("System_Ontologies");
// Unknown namespace name rejects.
assertRejectsSuffix(() => db.connect("nope"), "Graph not found: nope");
assertRejectsSuffix(() => sysConn.setNamespace("nope"), "Graph not found: nope");
// Namespace cannot change while an explicit transaction is open.
sysConn.begin();
assertRejectsSuffix(
  () => sysConn.setNamespace("neo4j"),
  "Cannot change the namespace while a transaction is open."
);
sysConn.rollback();
sysConn.setNamespace("neo4j");

// Result metadata (ADR-0285): queryType and namespace on the query result.
// Fresh db so MATCH (n) really pins "EXPLAIN never executed the CREATE".
const metaDb = Database.open(":memory:");
const metaConn = metaDb.connect();
// EXPLAIN reports the wrapped statement's query type without executing it.
const explained = metaConn.query("EXPLAIN CREATE (n)");
assert.strictEqual(explained.queryType, "w");
assert.strictEqual(explained.namespace, "neo4j");
assert.strictEqual(metaConn.query("MATCH (n) RETURN n").rows.length, 0);
// Query type value set (ADR-0162, ADR-0332): "r" read, "w" a write with no
// returned columns, "rw" a write with returned columns, "s" schema.
assert.strictEqual(metaConn.query("MATCH (n) RETURN n").queryType, "r");
assert.strictEqual(metaConn.query("CREATE (n:Meta)").queryType, "w");
// A USE-routed statement reports its resolved namespace; the session
// namespace is unchanged, so a following unrouted statement still resolves
// to "neo4j".
const routed = metaConn.query("USE system SHOW DATABASES YIELD name RETURN name");
assert.strictEqual(routed.namespace, "system");
assert.strictEqual(metaConn.query("RETURN 1 AS n").namespace, "neo4j");
// consume().elapsedMs is the engine-reported statement elapsed in
// milliseconds (ADR-0221): a finite, non-negative number on every result.
const timed = metaConn.query("RETURN 1 AS n").consume();
assert.ok(Number.isFinite(timed.elapsedMs) && timed.elapsedMs >= 0, `elapsedMs: ${timed.elapsedMs}`);
// The phase split (ADR-0491): three finite, non-negative sub-windows whose
// sum never exceeds the total (the inter-stage gaps stay in the total alone).
for (const field of ["parseMs", "planMs", "executeMs"]) {
  assert.ok(
    Number.isFinite(timed[field]) && timed[field] >= 0,
    `${field}: ${timed[field]}`
  );
}
assert.ok(
  timed.parseMs + timed.planMs + timed.executeMs <= timed.elapsedMs,
  `phase split ${timed.parseMs}+${timed.planMs}+${timed.executeMs} exceeds elapsed ${timed.elapsedMs}`
);

// Int64 (LABS-901, ADR-0502): a Cypher INTEGER is a JS number inside the
// safe-integer window and a BigInt beyond it — deterministic by value, and
// `RETURN 9007199254740993` round-trips exactly, never as 9007199254740992.
const bigRow = metaConn.query("RETURN 1 AS one, 9007199254740993 AS big").single();
assert.strictEqual(bigRow.one, 1);
assert.strictEqual(bigRow.big, 9007199254740993n);
// The same policy at every depth, and for a bound parameter in both
// directions (a BigInt crosses to int64_t untouched).
assert.deepStrictEqual(
  metaConn.query("RETURN [1, 9007199254740993] AS xs").single().xs,
  [1, 9007199254740993n]
);
assert.strictEqual(
  metaConn.query("RETURN $v AS v", { v: 9007199254740993n }).single().v,
  9007199254740993n
);
assert.strictEqual(
  metaConn.query("RETURN $v AS v", { v: 9223372036854775807n }).single().v,
  9223372036854775807n
);
// The full-range bound value still compares equal in Cypher.
assert.strictEqual(
  metaConn.query("RETURN $v = 9223372036854775807 AS ok", { v: 9223372036854775807n }).single().ok,
  true
);

// The typed value walker (ADR-0502 §1): every kind decodes to its JS shape.
// Strings carry interior NULs whole (read by length, never to the first
// NUL); floats arrive as themselves, NaN included.
const {
  CypherDate, CypherTime, CypherLocalTime, CypherDateTime, CypherLocalDateTime,
  CypherDuration, CypherPoint, CypherVector, float, RelateError,
} = require("./index");
const tv = metaConn
  .query(
    "RETURN 'a\\u0000b' AS s, '' AS empty, 1.5 AS f, [1.5, null] AS xs, {k: 'v'} AS m, " +
      "date('2026-11-04') AS d, time('09:30:00.000000006+01:00') AS t, " +
      "localtime('12:34:56') AS lt, datetime('2026-11-04T09:30:00+01:00[Europe/Berlin]') AS dt, " +
      "localdatetime('2026-01-02T03:04:05.000000007') AS ldt, " +
      "duration('P1Y2M3DT4H5M6.000000007S') AS dur, point({x: 1, y: 2, z: 3}) AS p, " +
      "point({x: 1, y: 2}) AS p2, vector([1, 2, 3], 3, INTEGER8) AS vec, " +
      "vector([0.25, 1.5], 2, FLOAT32) AS fvec"
  )
  .single();
assert.strictEqual(tv.s, "a\u0000b");
// A map key with an interior NUL is refused: map_get reads keys
// NUL-terminated, so 'a\0b' would otherwise resolve to the 'a' entry's value.
assert.throws(
  () => metaConn.query("RETURN {'a\\u0000b': 1, a: 2} AS m").single(),
  /interior NUL/
);
// An integer-valued Number beyond int64 is refused, not wrapped (the BigInt
// arm and the Number arm share one range check).
assert.throws(() => metaConn.query("RETURN $x AS x", { x: 1e20 }), /outside the int64 range/);
assert.strictEqual(tv.empty, "");
assert.strictEqual(tv.f, 1.5);
assert.deepStrictEqual(tv.xs, [1.5, null]);
assert.deepStrictEqual(tv.m, { k: "v" });
assert.ok(tv.d instanceof CypherDate);
assert.deepStrictEqual([tv.d.year, tv.d.month, tv.d.day], [2026, 11, 4]);
assert.ok(tv.t instanceof CypherTime);
assert.deepStrictEqual(
  [tv.t.hour, tv.t.minute, tv.t.second, tv.t.nanosecond, tv.t.offsetSeconds],
  [9, 30, 0, 6, 3600]
);
assert.ok(tv.lt instanceof CypherLocalTime);
assert.deepStrictEqual([tv.lt.hour, tv.lt.minute, tv.lt.second], [12, 34, 56]);
assert.ok(tv.dt instanceof CypherDateTime);
assert.strictEqual(tv.dt.zone, "Europe/Berlin");
assert.strictEqual(tv.dt.offsetSeconds, 3600);
assert.ok(tv.ldt instanceof CypherLocalDateTime);
assert.strictEqual(tv.ldt.nanosecond, 7);
// An offset-only datetime keeps no zone id.
assert.strictEqual(
  metaConn.query("RETURN datetime('2026-11-04T09:30:00+01:00') AS dt").single().dt.zone,
  null
);
// Durations are lossless: PT36H and P1DT12H stay distinct.
assert.ok(tv.dur instanceof CypherDuration);
assert.deepStrictEqual([tv.dur.months, tv.dur.days, tv.dur.seconds, tv.dur.nanos], [14, 3, 4 * 3600 + 5 * 60 + 6, 7]);
const durs = metaConn.query("RETURN duration('PT36H') AS a, duration('P1DT12H') AS b").single();
assert.deepStrictEqual([durs.a.months, durs.a.days, durs.a.seconds], [0, 0, 129600]);
assert.deepStrictEqual([durs.b.months, durs.b.days, durs.b.seconds], [0, 1, 43200]);
assert.ok(tv.p instanceof CypherPoint);
assert.deepStrictEqual([tv.p.srid, tv.p.x, tv.p.y, tv.p.z], [9157, 1, 2, 3]);
assert.ok(tv.p2 instanceof CypherPoint);
assert.strictEqual(tv.p2.z, undefined);
assert.ok(tv.vec instanceof CypherVector);
assert.strictEqual(tv.vec.dtype, "INTEGER8");
assert.deepStrictEqual(tv.vec.values, [1, 2, 3]);
assert.ok(tv.fvec instanceof CypherVector);
assert.strictEqual(tv.fvec.dtype, "FLOAT32");
assert.deepStrictEqual(tv.fvec.values, [0.25, 1.5]);
// Entities: elementId identity, labels through the node's list, properties
// through its map, and the numeric id exactly id(n) (ADR-0506).
const nodeVal = metaConn.query("MATCH (n:Meta) RETURN n, id(n) AS nid").single();
assert.strictEqual(nodeVal.n.kind, "node");
assert.deepStrictEqual(nodeVal.n.labels, ["Meta"]);
assert.strictEqual(typeof nodeVal.n.elementId, "string");
assert.strictEqual(nodeVal.n.id, nodeVal.nid);
assert.deepStrictEqual(Object.keys(nodeVal.n.properties), []);

// The typed classes bind back unchanged (decode and encode share them):
// the round-trip through parameters compares equal in Cypher itself.
const echo = metaConn
  .query(
    "RETURN $d = date('2026-11-04') AS dOk, $dur = duration('P1Y2M3DT4H5M6.000000007S') AS durOk, " +
      "$dt = datetime('2026-11-04T09:30:00+01:00[Europe/Berlin]') AS dtOk, " +
      "$t = time('09:30:00.000000006+01:00') AS tOk, $lt = localtime('12:34:56') AS ltOk, " +
      "$ldt = localdatetime('2026-01-02T03:04:05.000000007') AS ldtOk, " +
      "$p AS p, $vec AS vec, $fvec AS fvec",
    {
      d: tv.d, dur: tv.dur, dt: tv.dt, t: tv.t, lt: tv.lt, ldt: tv.ldt,
      p: tv.p, vec: tv.vec, fvec: tv.fvec,
    }
  )
  .single();
assert.strictEqual(echo.dOk, true);
assert.strictEqual(echo.durOk, true);
assert.strictEqual(echo.dtOk, true);
assert.strictEqual(echo.tOk, true);
assert.strictEqual(echo.ltOk, true);
assert.strictEqual(echo.ldtOk, true);
assert.ok(echo.p instanceof CypherPoint);
assert.deepStrictEqual([echo.p.srid, echo.p.x, echo.p.y, echo.p.z], [9157, 1, 2, 3]);
assert.ok(echo.vec instanceof CypherVector);
assert.deepStrictEqual(echo.vec.values, [1, 2, 3]);
assert.deepStrictEqual(echo.fvec.values, [0.25, 1.5]);

// A float-typed 1 (the JSON era could not send it): float(1) binds FLOAT
// 1.0 — Cypher's `=` compares numerically across INT and FLOAT (1 = 1.0 is
// true either way), so the observable discriminator is the rendering.
const f1 = metaConn
  .query("RETURN $f AS f, toString($f) AS s", { f: float(1) })
  .single();
assert.strictEqual(f1.f, 1);
assert.strictEqual(f1.s, "1.0");
// Integer-valued numbers bind as INTEGER; plain 1 renders "1".
assert.strictEqual(
  metaConn.query("RETURN toString($n) AS s", { n: 1 }).single().s,
  "1"
);
assert.strictEqual(metaConn.query("RETURN toString(1.0) AS s").single().s, "1.0");

// Non-finite floats bind now (ADR-0502's builder carries them; the JSON era
// could not) and decode back as themselves.
assert.ok(Number.isNaN(metaConn.query("RETURN $v AS v", { v: NaN }).single().v));
assert.strictEqual(metaConn.query("RETURN $v AS v", { v: Infinity }).single().v, Infinity);
assert.strictEqual(metaConn.query("RETURN $v AS v", { v: -Infinity }).single().v, -Infinity);

// Errors (ADR-0502 §2): one typed exception carrying the engine's own
// classification — never re-derived from message text.
try {
  metaConn.query("RETURN 1/0");
  throw new Error("expected RETURN 1/0 to throw");
} catch (e) {
  assert.ok(e instanceof RelateError, `RelateError, got ${e}`);
  assert.strictEqual(e.code, "Neo.ClientError.Statement.ArithmeticError");
  assert.strictEqual(e.gqlStatus, "22G03");
  assert.strictEqual(e.stage, "runtime");
  assert.strictEqual(e.retryable, false);
  assert.strictEqual(e.isRetriable, false);
}
try {
  metaConn.query("RETURN (");
  throw new Error("expected a syntax error to throw");
} catch (e) {
  assert.ok(e instanceof RelateError);
  assert.strictEqual(e.stage, "compile");
}

// The error shape is validated, never defaulted (LABS-930): a malformed
// or partial ABI error JSON is itself an error naming the problem — never
// a RelateError with fabricated null fields (a missing "retryable"
// would silently decode as false and a retry loop would decline to
// retry). The Go and Swift clients hold the same contract.
const { errorFromJson, decodeQueryType, decodeNamespace } = require("./errors");
const shape = (over = {}) => JSON.stringify({
  code: "Neo.ClientError.Statement.ArithmeticError",
  gql_status: "22G03",
  stage: "runtime",
  retryable: false,
  message: "division by zero",
  ...over,
});
const parsed = errorFromJson(shape());
assert.ok(parsed instanceof RelateError);
assert.strictEqual(parsed.message, "division by zero");
assert.strictEqual(parsed.code, "Neo.ClientError.Statement.ArithmeticError");
assert.strictEqual(parsed.gqlStatus, "22G03");
assert.strictEqual(parsed.stage, "runtime");
assert.strictEqual(parsed.retryable, false);
assert.strictEqual(parsed.isRetriable, false);
// A retryable one keeps its flag end to end.
const transient = errorFromJson(shape({
  code: "Neo.TransientError.Transaction.Outdated",
  retryable: true,
  message: "write-write conflict",
}));
assert.strictEqual(transient.isRetriable, true);
// The closed result-metadata vocabularies (ADR-0162, ADR-0117): every listed
// value passes through, anything else is an ABI contract violation.
for (const qt of ["r", "w", "rw", "s"]) assert.strictEqual(decodeQueryType(qt), qt);
for (const ns of ["neo4j", "system", "system_ontologies"]) {
  assert.strictEqual(decodeNamespace(ns), ns);
}
for (const bad of ["", "R", "x", null]) {
  assert.throws(() => decodeQueryType(bad), /ABI contract violated: query type/);
}
for (const bad of ["", "Neo4j", "other", null]) {
  assert.throws(() => decodeNamespace(bad), /ABI contract violated: namespace/);
}
// Every violation names itself: not JSON, not an object, a missing field,
// a mistyped field, an off-vocabulary stage.
assert.throws(() => errorFromJson("{not json"), /ABI contract violated: .*not JSON/);
assert.throws(() => errorFromJson("null"), /ABI contract violated: .*not an object/);
assert.throws(() => errorFromJson("[1]"), /ABI contract violated: .*not an object/);
assert.throws(
  () => errorFromJson('{"code":"c","gql_status":"22G03","stage":"runtime","retryable":false}'),
  /ABI contract violated: .*missing "message"/
);
assert.throws(
  () => errorFromJson(shape({ retryable: "yes" })),
  /ABI contract violated: .*"retryable" must be a boolean, got string/
);
assert.throws(
  () => errorFromJson(shape({ stage: "later" })),
  /ABI contract violated: .*error stage "later" is not one of compile, runtime, internal/
);

// Query notifications (ADR-0363): the engine-native list rides every result,
// always present. A disconnected multi-pattern EXPLAIN plans as a Cartesian
// product; a plain run derives none.
const notifDb = Database.open(":memory:");
const notifConn = notifDb.connect();
notifConn.query("CREATE (:Person)");
notifConn.query("CREATE (:City)");
const cartesian = notifConn.query("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b");
assert.ok(Array.isArray(cartesian.notifications), "notifications is an array");
assert.strictEqual(
  cartesian.notifications.length,
  1,
  `one CartesianProduct notification: ${JSON.stringify(cartesian.notifications)}`
);
const [entry] = cartesian.notifications;
assert.strictEqual(entry.code, "Neo.ClientNotification.Statement.CartesianProduct");
assert.strictEqual(entry.gql_status, "03N90");
assert.strictEqual(entry.severity, "INFORMATION");
assert.strictEqual(entry.category, "PERFORMANCE");
assert.strictEqual(entry.title, "Cartesian product");
assert.ok(entry.description.length > 0, "description is present");
assert.ok(entry.remedy.length > 0, "remedy is present");
assert.strictEqual(typeof entry.operator_id, "number");
// consume() exposes the same list on the summary.
assert.deepStrictEqual(cartesian.consume().notifications, cartesian.notifications);
// A plain run carries the always-present empty array, not an absent field.
const plain = notifConn.query("RETURN 1 AS n");
assert.deepStrictEqual(plain.notifications, []);
assert.deepStrictEqual(plain.consume().notifications, []);

// Doc example for the namespace API.
require("../../docs/api/javascript/examples/namespace.js");
// Doc example for result metadata.
require("../../docs/api/javascript/examples/result_metadata.js");
// Doc example for result ergonomics.
require("../../docs/api/javascript/examples/result_ergonomics.js");

const readOnly = Database.open(":memory:", { readOnly: true });
const readConn = readOnly.connect();
assert.strictEqual(readConn.query("RETURN 1 AS n").rows[0].n, 1);
assertRejectsSuffix(
  () => readConn.query("CREATE ()"),
  "No write operations are allowed on this database. The database is in read-only mode on this Neo4j instance."
);
Database.open(":memory:").connect().query("CREATE ()");

console.log("ok");

// --- Result ergonomics (ADR-0216) ---------------------------------------

const ergoDb = Database.open(":memory:");
const ergo = ergoDb.connect();

// Rows are objects keyed by column name.
const named = ergo.query("RETURN 1 AS n, 'x' AS s");
assert.strictEqual(named.rows[0].n, 1);
assert.strictEqual(named.rows[0].s, "x");
assert.deepStrictEqual(named.columns, ["n", "s"]);

// single(): the one row, or ResultNotSingleError.
assert.strictEqual(ergo.query("RETURN 1 AS n").single().n, 1);
assert.throws(() => ergo.query("RETURN 1 AS n UNION RETURN 2 AS n").single(),
  (e) => e.name === "ResultNotSingleError");
assert.throws(() => ergo.query("MATCH (n:NothingHere) RETURN n").single(),
  (e) => e.name === "ResultNotSingleError");

// stats: the canonical 11 counters, zeros included.
ergo.query("CREATE (a:P {name: 'a'})-[:R]->(b:P {name: 'b'})");
const created = ergo.query("CREATE (c:P {name: 'c'})");
assert.strictEqual(created.stats.nodes_created, 1);
assert.strictEqual(created.stats.properties_set, 1);
assert.strictEqual(Object.keys(created.stats).length, 11);
assert.strictEqual(created.stats.constraints_removed, 0);

// consume(): summary with counters, metadata, and client-edge elapsed.
const summary = ergo.query("MATCH (p:P) RETURN p.name AS name").consume();
assert.strictEqual(summary.counters.nodes_created, 0);
assert.strictEqual(summary.queryType, "r");
assert.strictEqual(summary.namespace, "neo4j");
assert.ok(summary.elapsedMs >= 0, "elapsedMs is measured");
assert.ok(
  summary.parseMs + summary.planMs + summary.executeMs <= summary.elapsedMs,
  "the phase split stays within the elapsed total (ADR-0491)"
);
// ADR-0332: a write with no returned columns is "w", not "rw".
assert.strictEqual(ergo.query("MATCH (p:P) DETACH DELETE p").consume().queryType, "w");

// graph(): unique entities, element-id-deduplicated, first-seen order,
// nested values and paths walked.
ergo.query("CREATE (x:G {n: 'x'})-[:LINK]->(y:G {n: 'y'})");
ergo.query("MATCH (x:G {n: 'x'}) CREATE (z:G {n: 'z'})-[:LINK]->(x)");
const g = ergo.query("MATCH (a:G)-[r:LINK]->(b:G) RETURN a, r, b").graph();
assert.strictEqual(g.nodes.length, 3);
assert.strictEqual(g.relationships.length, 2);
assert.strictEqual(new Set(g.nodes.map((n) => n.elementId)).size, 3);
assert.strictEqual(g.relationships[0].type, "LINK");
assert.strictEqual(g.size, 5);
// A path value contributes all of its entities.
const viaPath = ergo.query("MATCH p = (a:G)-[r:LINK]->(b:G) RETURN p").graph();
assert.strictEqual(viaPath.nodes.length, 3);
assert.strictEqual(viaPath.relationships.length, 2);
// No entities -> empty graph, not an error.
const none = ergo.query("MATCH (a:G) RETURN a.n AS n").graph();
assert.strictEqual(none.size, 0);
// Entity objects are the decoded walker shapes: the engine's elementId()
// spelling as the identity, the kind discriminator as the dispatch key, the
// deprecated numeric ids (ADR-0506) exactly id()/id(startNode())/id(endNode()).
assert.deepStrictEqual(
  Object.keys(g.nodes[0]).sort(),
  ["elementId", "id", "kind", "labels", "properties"]
);
assert.strictEqual(g.nodes[0].kind, "node");
assert.strictEqual(typeof g.nodes[0].elementId, "string");
assert.strictEqual(g.relationships[0].kind, "relationship");
assert.deepStrictEqual(
  Object.keys(g.relationships[0]).sort(),
  ["elementId", "endElementId", "endId", "id", "kind", "properties",
   "startElementId", "startId", "type"]
);
assert.strictEqual(typeof g.relationships[0].startElementId, "string");
const numericIds = ergo
  .query(
    "MATCH (a:G)-[r:LINK]->(b:G) RETURN a, r, b, " +
      "id(a) AS aid, id(r) AS rid, id(startNode(r)) AS rsid, id(endNode(r)) AS reid"
  )
  .rows[0];
assert.strictEqual(numericIds.a.id, numericIds.aid);
assert.strictEqual(numericIds.r.id, numericIds.rid);
assert.strictEqual(numericIds.r.startId, numericIds.rsid);
assert.strictEqual(numericIds.r.endId, numericIds.reid);
// Entity-ness is structural (the decoder's non-enumerable marker), never
// key-sniffed: a user map with a kind tag — even one carrying every entity
// key — is a map, contributes nothing to the graph, and stays a plain map.
const fake = ergo
  .query("RETURN {kind: 'node', labels: ['Fake']} AS m, {kind: 'relationship', elementId: 'x'} AS r")
  .graph();
assert.strictEqual(fake.size, 0);
const lookAlike = {
  kind: "node",
  elementId: "neo4j:fake:n1",
  id: 1,
  labels: ["Fake"],
  properties: { name: "not a node" },
};
const lookAlikeResult = ergo.query("RETURN $m AS m, [$m] AS l, {inner: $m} AS d", { m: lookAlike });
assert.strictEqual(lookAlikeResult.graph().size, 0);
const lookAlikeRow = lookAlikeResult.single();
assert.deepStrictEqual(lookAlikeRow.m, lookAlike);
// The marker is module-local (graph.js), never registered globally: the
// Symbol.for spelling never matches, so no user code can forge or probe
// entity-ness, and a look-alike map reads as no entity through the one
// reader.
const { decodedEntityKind } = require("./graph");
assert.strictEqual(decodedEntityKind(lookAlikeRow.m), undefined);
assert.strictEqual(lookAlikeRow.m[Symbol.for("relate.decoded")], undefined);
// A real decoded entity carries the marker, invisibly to Object.keys.
assert.strictEqual(decodedEntityKind(g.nodes[0]), "node");
assert.strictEqual(decodedEntityKind(g.relationships[0]), "relationship");
assert.strictEqual(g.nodes[0][Symbol.for("relate.decoded")], undefined);

console.log("ergonomics ok");

// --- Tagged-template queries (ADR-0222) ---------------------------------

const tagDb = Database.open(":memory:");
const tag = tagDb.connect();

// Interpolation: values become auto-named parameters; results match the
// values, not their source text.
tag.query`CREATE (p:Person {name: ${"Ada"}, age: ${36}})`;
const found = tag.query`MATCH (p:Person {name: ${"Ada"}}) RETURN p.age AS age`.single();
assert.strictEqual(found.age, 36);

// Dedup: the same primitive twice maps to one parameter; the query still
// sees both positions.
const twice = tag.query`RETURN ${"same"} AS a, ${"same"} AS b`.single();
assert.deepStrictEqual(twice, { a: "same", b: "same" });
assert.strictEqual(tag.query`RETURN ${1} AS x, ${1.0} AS y`.single().x, 1);

// Nested arrays and objects are values; the same object identity dedups.
// (Map properties are not a supported stored-property type, so objects are
// exercised via RETURN/UNWIND and arrays via a stored property.)
const tags = ["go", "climbing"];
const props = { city: "London", tags };
const echoed = tag.query`RETURN ${props} AS a, ${props} AS b`.single();
assert.deepStrictEqual(echoed.a, { city: "London", tags: ["go", "climbing"] });
assert.deepStrictEqual(echoed.b, echoed.a);
tag.query`CREATE (p:Person {name: ${"Bob"}, hobbies: ${tags}})`;
assert.deepStrictEqual(
  tag.query`MATCH (p:Person {name: "Bob"}) RETURN p.hobbies AS h`.single().h,
  ["go", "climbing"]
);
// A list of maps drives a batch create.
tag.query`UNWIND ${[{ name: "E1" }, { name: "E2" }]} AS r CREATE (e:E {name: r.name})`;
assert.strictEqual(tag.query`MATCH (e:E) RETURN count(e) AS n`.single().n, 2);
// null is a value; booleans round-trip.
assert.strictEqual(tag.query`RETURN ${null} AS v`.single().v, null);
assert.strictEqual(tag.query`RETURN ${true} AS v`.single().v, true);

// Injection attempt: a value containing Cypher text / braces stays data.
const hostile = "x'} DETACH DELETE p //";
tag.query`CREATE (p:Person {name: ${hostile}})`;
const kept = tag.query`MATCH (p:Person) RETURN p.name AS name ORDER BY name`;
assert.deepStrictEqual(
  kept.rows.map((r) => r.name),
  ["Ada", "Bob", hostile]
);

// Zero interpolations: the tag form works on a plain template too.
assert.strictEqual(tag.query`RETURN 7 AS n`.single().n, 7);

// Unsupported interpolations reject; a stray argument does too.
assert.throws(() => tag.query`RETURN ${undefined} AS v`, TypeError);
assert.throws(() => tag.query`RETURN ${(() => {})()} AS v`, TypeError); // undefined
assert.throws(() => tag.query`RETURN ${Symbol("s")} AS v`, TypeError);
assert.throws(() => tag.query`RETURN ${(() => 1)} AS v`, TypeError);
assert.throws(() => tag.query("RETURN 1", {}, {}), TypeError);
// The string form applies the same value policy: unsupported param values
// and non-object params reject loudly instead of being silently dropped.
assert.throws(() => tag.query("RETURN $v AS v", { v: undefined }), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", { v: () => 1 }), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", ["not", "an", "object"]), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", new Map([["v", 1]])), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", new Date()), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", "not an object"), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", 42), TypeError);
// The policy walks nested objects and arrays in both forms — a nested
// unsupported leaf rejects with the path named, instead of being silently
// dropped by JSON.stringify (object keys vanish, array slots null out).
assert.throws(() => tag.query("RETURN $v AS v", { v: { nested: undefined } }), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", { v: [1, () => 2] }), TypeError);
assert.throws(() => tag.query`RETURN ${{ nested: Symbol("s") }} AS v`, TypeError);
assert.throws(() => tag.query`RETURN ${[1, { deep: undefined }]} AS v`, TypeError);
assert.throws(
  () => tag.query("RETURN $v AS v", { v: { nested: undefined } }),
  /query params: "v"\.nested cannot be a Cypher parameter/
);
// A cyclic value carries no unsupported leaf; the binder's identity walk
// terminates and reports the cycle.
const cyclic = { a: 1 };
cyclic.self = cyclic;
assert.throws(() => tag.query("RETURN $v AS v", { v: cyclic }), /"v"\.self is cyclic/);
const cyclicList = [1];
cyclicList.push({ back: cyclicList });
assert.throws(() => tag.query("RETURN $v AS v", { v: cyclicList }), /is cyclic/);
// A shared (acyclic) reference is not a cycle: the same object under two
// keys, twice in one list, and under two nesting depths all bind.
const shared = { n: 7, tags: ["x"] };
const sharedRow = tag
  .query("RETURN $a AS a, $b AS b, $l AS l, $d AS d", {
    a: shared,
    b: shared,
    l: [shared, shared],
    d: { inner: shared, again: { inner: shared } },
  })
  .single();
assert.deepStrictEqual(sharedRow.a, shared);
assert.deepStrictEqual(sharedRow.b, shared);
assert.deepStrictEqual(sharedRow.l, [shared, shared]);
assert.deepStrictEqual(sharedRow.d, { inner: shared, again: { inner: shared } });
// The plain-object rule applies at every depth: a nested Map/Set/Date/
// class instance has no Cypher shape (a JS Date's UTC/local reading is
// ambiguous — construct a CypherDateTime instead).
assert.throws(() => tag.query("RETURN $v AS v", { v: { d: new Date() } }), TypeError);
assert.throws(() => tag.query("RETURN $v AS v", { v: [new Map([["k", 1]])] }), TypeError);
assert.throws(() => tag.query`RETURN ${{ s: new Set([1]) }} AS v`, TypeError);
// A BigInt outside int64 rejects client-side, naming the value.
assert.throws(() => tag.query("RETURN $v AS v", { v: 2n ** 64n }), TypeError);
// Non-finite numbers bind now (the JSON era could not carry them).
assert.ok(Number.isNaN(tag.query("RETURN $v AS v", { v: NaN }).single().v));
assert.strictEqual(tag.query("RETURN $v AS v", { v: [Infinity] }).single().v[0], Infinity);
assert.strictEqual(tag.query`RETURN ${-Infinity} AS v`.single().v, -Infinity);
assert.strictEqual(tag.query("RETURN $v AS v", { v: 42 }).single().v, 42);
// The empty string binds as a zero-length STRING, distinct from null
// (LABS-1198): koffi marshals a zero-length buffer as NULL, and the ABI
// reads that spelling as "" instead of refusing it.
const empty = tag
  .query("RETURN $v AS v, $v IS NULL AS n, $v = '' AS e", { v: "" })
  .single();
assert.strictEqual(empty.v, "");
assert.strictEqual(empty.n, false);
assert.strictEqual(empty.e, true);
const nullProto = Object.create(null); // null-prototype objects are plain too
nullProto.v = 7;
assert.strictEqual(tag.query("RETURN $v AS v", nullProto).single().v, 7);

// Doc example for tagged templates.
require("../../docs/api/javascript/examples/tagged_templates.js");

console.log("tagged templates ok");

// The structured plan (LABS-418, ADR-0381): a relate.plan/v1 document on
// EXPLAIN/PROFILE, null on a plain run. Each node's warnings carry the
// engine's remedy-bearing notifications, joined by operator id.
const planDb = Database.open(":memory:");
const planConn = planDb.connect();
planConn.query("CREATE (:Person {name: 'Alice'})");
planConn.query("CREATE (:City)");

const planExplained = planConn.query("EXPLAIN MATCH (p:Person) RETURN p.name AS name");
assert.strictEqual(planExplained.plan.schema, "relate.plan/v1");
assert.strictEqual(planExplained.plan.profiled, false);
assert.ok(planExplained.plan.statement.startsWith("MATCH (p:Person)"));
assert.strictEqual(planExplained.plan.root.operator, "ProduceResults");
assert.strictEqual(planExplained.plan.root.actual, null, "EXPLAIN carries no metrics");
const planWalk = (node, out = []) => {
  out.push(node);
  for (const child of node.children) planWalk(child, out);
  return out;
};
assert.ok(
  planWalk(planExplained.plan.root).some((n) => n.operator === "NodeByLabelScan"),
  "the label scan rides beneath the projection"
);

const planProfiled = planConn.query("PROFILE MATCH (p:Person) RETURN p.name AS name").plan;
assert.strictEqual(planProfiled.profiled, true);
const scan = planWalk(planProfiled.root).find((n) => n.operator === "NodeByLabelScan");
assert.strictEqual(scan.actual.rows, 1);
assert.strictEqual(scan.actual.node_reads, 1, "one store read per candidate");
assert.strictEqual(scan.actual.start.reason, "index-0");

// A warning carries the action, joined by operator id.
const planWarned = planConn.query("EXPLAIN MATCH (a:Person), (c:City) RETURN a, c").plan;
const warnings = planWalk(planWarned.root).flatMap((n) => n.warnings);
assert.strictEqual(warnings.length, 1, "the cartesian MATCH warns");
assert.strictEqual(warnings[0].code, "Neo.ClientNotification.Statement.CartesianProduct");
assert.ok(warnings[0].remedy.length > 0, "the warning carries its remedy");

// A plain run carries null, never an empty document; consume() matches.
assert.strictEqual(planConn.query("RETURN 1 AS n").plan, null);
assert.strictEqual(planConn.query("RETURN 1 AS n").consume().plan, null);
assert.deepStrictEqual(
  planConn.query("EXPLAIN MATCH (p:Person) RETURN p").consume().plan,
  planConn.query("EXPLAIN MATCH (p:Person) RETURN p").plan
);

// --- Bookmarks (ADR-0502 §7) ---------------------------------------------

const bmDb = Database.open(":memory:");
const bmConn = bmDb.connect();
// An auto-commit write's bookmark rides the result; a read commits nothing.
const wrote = bmConn.query("CREATE (:Bm)");
assert.strictEqual(typeof wrote.bookmark, "string");
assert.ok(/^relate:\d+$/.test(wrote.bookmark), `bookmark shape: ${wrote.bookmark}`);
assert.strictEqual(bmConn.query("RETURN 1 AS n").bookmark, null);
// consume() carries the same bookmark.
assert.strictEqual(bmConn.query("CREATE (:Bm2)").consume().bookmark !== null, true);
// Inside an explicit transaction the result carries no bookmark — the
// commit's return value is the bookmark.
bmConn.begin();
const inner = bmConn.query("CREATE (:Bm3)");
assert.strictEqual(inner.bookmark, null);
const commitBookmark = bmConn.commit();
assert.strictEqual(typeof commitBookmark, "string");
assert.ok(/^relate:\d+$/.test(commitBookmark));
// A later commit's bookmark sorts after an earlier one's (compare the
// sequence numbers — the strings are "relate:<n>" and lexicographic
// order misleads across digit-count boundaries).
const seq = (bm) => Number(bm.split(":")[1]);
bmConn.begin();
bmConn.query("CREATE (:Bm4)");
assert.ok(seq(bmConn.commit()) > seq(commitBookmark), "the commit sequence is monotonic");
// A transaction that committed nothing returns null — no fabricated
// bookmark, and no error either.
bmConn.begin();
bmConn.query("RETURN 1 AS n");
assert.strictEqual(bmConn.commit(), null);
// A failed commit throws through the ordinary status/error channel
// (ADR-0510) — the status alone discriminates, so a failure never
// masquerades as null. Committing with no transaction open fails.
assert.throws(
  () => bmConn.commit(),
  (e) => e instanceof RelateError && e.stage !== "internal"
);
// A failed statement poisons the transaction: commit refuses until the
// caller rolls back.
bmConn.begin();
bmConn.query("CREATE (:Bm5)");
assert.throws(() => bmConn.query("RETURN 1/0"), RelateError);
assert.throws(
  () => bmConn.commit(),
  (e) => e instanceof RelateError && e.stage !== "internal"
);
bmConn.rollback();

// --- Async queries (the koffi async form, ADR-0502) ----------------------

async function asyncSuite() {
  const db = Database.open(":memory:");
  const conn = db.connect();
  conn.query("CREATE (:Async {n: 1})");

  // The happy path: the same Result, resolved.
  const found = await conn.queryAsync("MATCH (n:Async) RETURN n.n AS n");
  assert.strictEqual(found.single().n, 1);

  // THE v1 loss, recovered (LABS-901's sibling): a failing statement's
  // error rides the result handle back from the worker thread, so the
  // rejected promise carries the Neo4j code — v1 read a thread-local slot
  // on the wrong thread and lost it.
  await assert.rejects(
    conn.queryAsync("RETURN 1/0"),
    (e) =>
      e instanceof RelateError &&
      e.code === "Neo.ClientError.Statement.ArithmeticError" &&
      e.gqlStatus === "22G03" &&
      e.stage === "runtime" &&
      e.retryable === false
  );

  // Both call shapes, parameters included: the tag form and the string
  // form with typed values and a lossless int64.
  const tagged = await conn.queryAsync`RETURN ${41} + 1 AS n`;
  assert.strictEqual(tagged.single().n, 42);
  const bound = await conn.queryAsync("RETURN $v AS v", { v: 9007199254740993n });
  assert.strictEqual(bound.single().v, 9007199254740993n);

  // The serial executor: overlapping awaits on one connection queue —
  // one call at a time is the ABI's connection contract — and every
  // result is its own.
  const queued = await Promise.all([
    conn.queryAsync("RETURN 1 AS n"),
    conn.queryAsync("RETURN 2 AS n"),
    conn.queryAsync("RETURN 3 AS n"),
  ]);
  assert.deepStrictEqual(queued.map((r) => r.single().n), [1, 2, 3]);

  // Distinct connections need no queue between themselves.
  const other = db.connect();
  const [a, b] = await Promise.all([
    conn.queryAsync("RETURN 'a' AS x"),
    other.queryAsync("RETURN 'b' AS x"),
  ]);
  assert.strictEqual(a.single().x, "a");
  assert.strictEqual(b.single().x, "b");

  // A sync query while an async query is in flight on the same connection
  // refuses definedly (the guard), and works again once the flight lands.
  // The window is synchronized, not raced: one microtask hop drains the
  // queued task's start (the window's opening edge — the FFI dispatch)
  // first, and the koffi callback can only be delivered on a later
  // event-loop turn, so the assertions run inside the window however fast
  // the worker finishes the count.
  const inFlight = conn.queryAsync("UNWIND range(1, 500000) AS i RETURN count(i) AS n");
  await Promise.resolve();
  assert.throws(() => conn.query("RETURN 1 AS n"), /synchronous query .*in flight/);
  // Every synchronous FFI call on the connection follows the same policy:
  // begin, commit, rollback and setNamespace refuse during the flight too.
  assert.throws(() => conn.begin(), /synchronous begin .*in flight/);
  assert.throws(() => conn.commit(), /synchronous commit .*in flight/);
  assert.throws(() => conn.rollback(), /synchronous rollback .*in flight/);
  assert.throws(() => conn.setNamespace("neo4j"), /synchronous setNamespace .*in flight/);
  await inFlight;
  assert.strictEqual(conn.query("RETURN 1 AS n").single().n, 1);
  conn.setNamespace("neo4j"); // the flight landed: the connection is free again

  // Transactions stay synchronous calls; a commit bookmark reads the same
  // from either form.
  conn.begin();
  conn.query("CREATE (:Async {n: 2})");
  const tx = await conn.queryAsync("MATCH (n:Async) RETURN count(n) AS n");
  assert.strictEqual(tx.single().n, 2);
  assert.ok(/^relate:\d+$/.test(conn.commit()));
}

asyncSuite()
  .then(() => {
    console.log("async ok");
    console.log("v2 ok");
  })
  .catch((e) => {
    console.error(e);
    process.exit(1);
  });

// Doc example for the typed value codec, errors and async queries.
require("../../docs/api/javascript/examples/typed_values.js");
