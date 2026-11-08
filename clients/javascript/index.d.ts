/**
 * Relate Node.js client — embedded openCypher graph engine via the
 * typed C ABI v2 (ADR-0502).
 *
 * The engine runs in-process: `query` is synchronous; `queryAsync` is the
 * Promise-returning form. Databases are a local directory, a single `.rl`
 * file, or `":memory:"`. One writer process per database;
 * `readOnly: true` allows any number of readers.
 *
 * The library must be ABI v2 (`relate_abi_version() === 2`); loading
 * an incompatible library fails at `require` time with an
 * "incompatible librelate_ffi" error.
 */

/** The plan-time statement classification (ADR-0162 wire code). */
export type QueryType = "r" | "w" | "rw" | "s";

/**
 * A Cypher INTEGER (int64) as a JS value — the lossless representation
 * policy (LABS-901): a `number` when the value is exactly representable
 * (within ±2^53-1, `Number.isSafeInteger`), a `bigint` beyond it.
 * Deterministic by value, so `RETURN 9007199254740993` is exactly
 * `9007199254740993n` and never the rounded `9007199254740992`. Bind
 * either back: a `bigint` crosses to int64 untouched; an integer-valued
 * `number` binds as INTEGER (pass a `bigint` for values beyond the safe
 * window).
 */
export type Int64 = number | bigint;

/**
 * A node value: the engine's elementId() spelling, the deprecated numeric
 * id, labels, properties.
 */
export interface NodeValue {
  kind: "node";
  elementId: string;
  /** The deprecated numeric entity id — exactly what Cypher `id()` returns (ADR-0506), under the `Int64` policy. */
  id: Int64;
  labels: string[];
  properties: Record<string, unknown>;
}

/**
 * A relationship value: endpoints and type by element id, plus the
 * deprecated numeric ids (`id(r)`, `id(startNode(r))`, `id(endNode(r))`,
 * ADR-0506) under the `Int64` policy.
 */
export interface RelationshipValue {
  kind: "relationship";
  elementId: string;
  id: Int64;
  type: string;
  startElementId: string;
  endElementId: string;
  startId: Int64;
  endId: Int64;
  properties: Record<string, unknown>;
}

/** A path value: alternating [node, rel, node, …, node]. */
export type PathValue = Array<NodeValue | RelationshipValue>;

/** One row of a result: an object keyed by column name. */
export type Row = Record<string, unknown>;

/** The canonical 11 mutation counters, snake_case, zeros included. */
export interface Stats {
  nodes_created: number;
  nodes_deleted: number;
  relationships_created: number;
  relationships_deleted: number;
  properties_set: number;
  labels_added: number;
  labels_removed: number;
  indexes_added: number;
  indexes_removed: number;
  constraints_added: number;
  constraints_removed: number;
}

/** One query notification (ADR-0363), in the engine-native shape. */
export interface Notification {
  /** The notification code, e.g. "Neo.ClientNotification.Statement.CartesianProduct". */
  code: string;
  /** The GQLSTATUS value, e.g. "03N90". */
  gql_status: string;
  severity: "INFORMATION" | "WARNING";
  category: "PERFORMANCE" | "UNRECOGNIZED" | "DEPRECATION" | "SCHEMA" | "HINT";
  /** The short human-readable kind name, e.g. "Cartesian product". */
  title: string;
  /** The diagnosis: what the statement does and which part raises it. */
  description: string;
  /** The action that resolves the notification. */
  remedy: string;
  /** The raising plan operator's id; present only on operator-scoped kinds. */
  operator_id?: number;
}

/** The walk's chosen start element (LABS-369, ADR-0375). */
export type PlanStartReason = "bound-pivot" | "cheapest" | "seek-anchored" | "index-0";

/** One operator's PROFILE metrics (`null` on an EXPLAIN tree). */
export interface PlanActual {
  rows: number;
  time_ms: number;
  db_hits: number;
  memory_bytes: number;
  /** Relationship hops taken. */
  hops: number;
  /** Neighbors traversed at the adjacency enumeration. */
  neighbor_walks: number;
  /** Node-store reads (one per candidate examined). */
  node_reads: number;
  /** How the walk chose its start, when it walked. */
  start: { element: number; backward: boolean; reason: PlanStartReason } | null;
}

/** One operator of a `relate.plan/v1` document (LABS-418, ADR-0381). */
export interface PlanNode {
  operator: string;
  details: string;
  identifiers: string[];
  estimated_rows: number | null;
  actual: PlanActual | null;
  children: PlanNode[];
  /** The operator's remedy-bearing notifications (ADR-0363), joined by id. */
  warnings: Notification[];
}

/** A statement's structured plan (LABS-418, ADR-0381). */
export interface PlanDocument {
  schema: "relate.plan/v1";
  /** Whether the tree carries runtime metrics (PROFILE). */
  profiled: boolean;
  /** The echoed statement text, or `null` on a hand-built tree. */
  statement: string | null;
  root: PlanNode;
}

/**
 * The error every failing call throws (ADR-0502 §2): the engine's own
 * classification — never re-derived from message text. Retry a
 * `retryable` error (the write-write conflict, the retryable workspace
 * cap); everything else fails loudly. `stage === "internal"` means a
 * boundary failure (a poisoned handle, a caught panic, an argument that
 * failed to decode): close the handle and open a new one instead of
 * retrying.
 */
export class RelateError extends Error {
  /** The Neo4j status code, e.g. "Neo.ClientError.Statement.ArithmeticError". */
  readonly code: string | null;
  /** The GQLSTATUS value, e.g. "22G03". */
  readonly gqlStatus: string | null;
  /** "compile" | "runtime" | "internal". */
  readonly stage: "compile" | "runtime" | "internal" | null;
  /** The drivers' transient test: re-run the same work only when true. */
  readonly retryable: boolean;
  /** The drivers' retriable spelling (=== retryable). */
  get isRetriable(): boolean;
}

/**
 * A FLOAT-typed parameter value: `float(1)` binds the FLOAT 1.0, not the
 * INTEGER 1. Cypher's `=` compares numerically across the two, so the type
 * shows up in the rendering (`toString(float(1))` is `"1.0"`).
 */
export class CypherFloat {
  constructor(value: number);
  readonly value: number;
}

/** A DATE as its calendar components (binds back unchanged). */
export class CypherDate {
  constructor(year: Int64, month: Int64, day: Int64);
  readonly year: Int64;
  readonly month: Int64;
  readonly day: Int64;
}

/** A TIME: the local wall clock plus its UTC offset (a TIME keeps no zone id). */
export class CypherTime {
  constructor(hour: Int64, minute: Int64, second: Int64, nanosecond: Int64, offsetSeconds: Int64);
  readonly hour: Int64;
  readonly minute: Int64;
  readonly second: Int64;
  readonly nanosecond: Int64;
  readonly offsetSeconds: Int64;
}

/** A LOCAL_TIME: the wall clock, no offset. */
export class CypherLocalTime {
  constructor(hour: Int64, minute: Int64, second: Int64, nanosecond: Int64);
  readonly hour: Int64;
  readonly minute: Int64;
  readonly second: Int64;
  readonly nanosecond: Int64;
}

/**
 * A DATETIME: the local wall clock, its UTC offset, and the IANA zone id
 * when the value keeps one (`null` for an offset-only value).
 */
export class CypherDateTime {
  constructor(
    year: Int64, month: Int64, day: Int64,
    hour: Int64, minute: Int64, second: Int64, nanosecond: Int64,
    offsetSeconds: Int64, zone?: string | null
  );
  readonly year: Int64;
  readonly month: Int64;
  readonly day: Int64;
  readonly hour: Int64;
  readonly minute: Int64;
  readonly second: Int64;
  readonly nanosecond: Int64;
  readonly offsetSeconds: Int64;
  readonly zone: string | null;
}

/** A LOCAL_DATETIME: the wall clock, no offset or zone. */
export class CypherLocalDateTime {
  constructor(
    year: Int64, month: Int64, day: Int64,
    hour: Int64, minute: Int64, second: Int64, nanosecond: Int64
  );
  readonly year: Int64;
  readonly month: Int64;
  readonly day: Int64;
  readonly hour: Int64;
  readonly minute: Int64;
  readonly second: Int64;
  readonly nanosecond: Int64;
}

/**
 * A DURATION as its four lossless components — `PT36H` (seconds = 129600)
 * and `P1DT12H` (days = 1, seconds = 43200) stay distinct.
 */
export class CypherDuration {
  constructor(months: Int64, days: Int64, seconds: Int64, nanos: Int64);
  readonly months: Int64;
  readonly days: Int64;
  readonly seconds: Int64;
  readonly nanos: Int64;
}

/** A POINT: srid (7203/4326 2D, 9157/4979 3D) and coordinates; `z` is `undefined` on 2D. */
export class CypherPoint {
  constructor(srid: number, x: number, y: number, z?: number);
  readonly srid: number;
  readonly x: number;
  readonly y: number;
  readonly z: number | undefined;
}

/**
 * A VECTOR: the canonical coordinate-type name (one of `"FLOAT16"`,
 * `"BFLOAT16"`, `"FLOAT32"`, `"FLOAT"`, `"INTEGER8"`, `"INTEGER16"`,
 * `"INTEGER32"`, `"INTEGER"`) and its coordinates (the `Int64` policy for
 * the INTEGER* dtypes).
 */
export class CypherVector {
  constructor(dtype: string, values: Array<Int64>);
  readonly dtype: string;
  readonly values: Array<Int64>;
}

/** `float(v)` — bind this number as Cypher FLOAT (alias of `new CypherFloat(v)`). */
export function float(value: number): CypherFloat;

/** The metadata of one executed statement (Result#consume()). */
export class ResultSummary {
  readonly counters: Stats;
  /** The statement's resolved namespace: "neo4j", "system" or "system_ontologies". */
  readonly namespace: string;
  /** The plan-time classification. */
  readonly queryType: QueryType;
  /** Engine-reported wall-clock milliseconds for the statement (ADR-0221): parse, plan and run. */
  readonly elapsedMs: number;
  /** The phase split of `elapsedMs` (ADR-0491): parse and lowering window. */
  readonly parseMs: number;
  /** The phase split of `elapsedMs` (ADR-0491): binding and planning window. */
  readonly planMs: number;
  /** The phase split of `elapsedMs` (ADR-0491): the dispatch (or EXPLAIN render) window. */
  readonly executeMs: number;
  /**
   * The statement's query notifications (ADR-0363). Empty on a plain run:
   * derivation is opt-in with the plan tree, so only EXPLAIN and PROFILE
   * populate the list (PROFILE adds the PROFILE-only EstimationMiss kind).
   */
  readonly notifications: Notification[];
  /** The statement's structured plan (LABS-418, ADR-0381), or `null` on a plain run. */
  readonly plan: PlanDocument | null;
  /**
   * The statement's auto-commit bookmark ("relate:<n>", ADR-0502 §7),
   * or `null` when it committed nothing — a read, an EXPLAIN, a statement
   * inside an explicit transaction (that commit's return value is the
   * bookmark), or an error result.
   */
  readonly bookmark: string | null;
}

/** The unique graph entities of one result (Result#graph()). */
export class ResultGraph {
  /** Unique nodes, first-seen order, de-duplicated by element id. */
  readonly nodes: NodeValue[];
  /** Unique relationships, first-seen order, de-duplicated by element id. */
  readonly relationships: RelationshipValue[];
  /** Node + relationship count. */
  get size(): number;
}

/** Thrown by Result#single() on zero or more than one row. */
export class ResultNotSingleError extends Error {
  readonly rowCount: number;
}

/** One executed statement's result; rows are materialized at query time. */
export class Result {
  /** Column names, in RETURN order. */
  readonly columns: string[];
  /**
   * Rows as objects keyed by column name. Cell values are the typed value
   * walker's JS representations (ADR-0502 §1): `null`, `boolean`,
   * `Int64`, `number` (NaN/±Infinity as themselves), `string` (interior
   * NULs preserved), `NodeValue`, `RelationshipValue`, `PathValue`,
   * arrays, plain objects, `CypherPoint`, `CypherDate`, `CypherTime`,
   * `CypherLocalTime`, `CypherDateTime`, `CypherLocalDateTime`,
   * `CypherDuration`, `CypherVector`. There is no per-column type array:
   * every value carries its own kind.
   */
  readonly rows: Row[];
  /** The 11 mutation counters. */
  readonly stats: Stats;
  readonly queryType: QueryType;
  readonly namespace: string;
  /** The statement's auto-commit bookmark, or `null` (see ResultSummary). */
  readonly bookmark: string | null;
  /**
   * The statement's query notifications (ADR-0363) as plain objects in the
   * engine-native shape; `[]` on a plain run.
   */
  readonly notifications: Notification[];
  /** The statement's structured plan (LABS-418, ADR-0381), or `null` on a plain run. */
  readonly plan: PlanDocument | null;
  /** The one row of a single-row result; ResultNotSingleError otherwise. */
  single(): Row;
  /** The statement's summary. Idempotent. */
  consume(): ResultSummary;
  /** The result's unique graph entities; empty when the rows carry none. */
  graph(): ResultGraph;
}

/** An open database. Pass `":memory:"` or a filesystem path. */
export class Database {
  private constructor();
  /**
   * Open a database. `options.readOnly` loads once and refuses writes and
   * checkpoints (a missing store is not created); any number of read-only
   * handles per path are allowed, one writer.
   */
  static open(uri: string, options?: { readOnly?: boolean }): Database;
  /** A connection; `namespace` is "neo4j" (default), "system" or "system_ontologies". */
  connect(namespace?: string): Connection;
  /** Flush pending changes to disk (checkpoint). */
  checkpoint(): void;
}

/**
 * A parameter value: JSON-spelled kinds directly (`null`, booleans,
 * strings, finite or non-finite numbers, `bigint`, arrays, plain objects),
 * the typed classes for the rest (`float()`, `CypherDate`, `CypherTime`,
 * `CypherLocalTime`, `CypherDateTime`, `CypherLocalDateTime`,
 * `CypherDuration`, `CypherPoint`, `CypherVector` — the same classes
 * result values decode to). `undefined`, functions, symbols, cyclic
 * structures, non-plain objects (a JS `Date` is ambiguous — construct a
 * `CypherDateTime`) and out-of-int64 `bigint`s throw `TypeError`.
 */
export type ParamValue =
  | null
  | boolean
  | number
  | bigint
  | string
  | ParamValue[]
  | { [key: string]: ParamValue }
  | CypherFloat
  | CypherDate
  | CypherTime
  | CypherLocalTime
  | CypherDateTime
  | CypherLocalDateTime
  | CypherDuration
  | CypherPoint
  | CypherVector;

/** A connection: runs statements in its session namespace. */
export class Connection {
  /**
   * Run one statement synchronously. `params` is a plain object; Cypher
   * references the values as `$name`. Returns the materialized result.
   * Throws `RelateError` if an async query is in flight on this
   * connection (one call at a time is the connection contract).
   */
  query(cypher: string, params?: { [name: string]: ParamValue }): Result;
  /**
   * Tagged-template form (ADR-0222): conn.query`MATCH (p {name: ${name}})
   * RETURN p`. Each interpolated value becomes an auto-named `$p0, $p1, …`
   * parameter — injection-safe by construction. The same primitive value
   * twice maps to one parameter (objects by identity); `undefined`,
   * functions and symbols throw `TypeError`.
   */
  query(strings: TemplateStringsArray, ...values: unknown[]): Result;
  /**
   * The Promise-returning form of query (same two call shapes). The call
   * runs on koffi's worker thread; a failing statement's error rides the
   * result handle back, so the rejected promise carries the Neo4j code.
   * Overlapping calls on one connection are serialized; a synchronous
   * query during an in-flight async query refuses.
   */
  queryAsync(cypher: string, params?: { [name: string]: ParamValue }): Promise<Result>;
  /** The async tagged-template form, same lowering as query's. */
  queryAsync(strings: TemplateStringsArray, ...values: unknown[]): Promise<Result>;
  /**
   * Begin an explicit transaction. Like every synchronous call here
   * (`commit`, `rollback`, `setNamespace` too), throws `RelateError`
   * if an async query is in flight on this connection.
   */
  begin(): void;
  /**
   * Commit the explicit transaction. Returns its bookmark
   * ("relate:<n>", ADR-0502 §7) or `null` when the transaction
   * committed nothing; a failed commit throws `RelateError` through
   * the ordinary status/error channel (ADR-0510 — a failure never
   * surfaces as `null`; a `retryable` one belongs in the caller's retry
   * loop).
   */
  commit(): string | null;
  rollback(): void;
  /** Change the session namespace between transactions (case-insensitive). */
  setNamespace(name: string): void;
}
