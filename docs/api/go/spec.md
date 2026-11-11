# Go client — spec

Module: `github.com/relatedb/relate/clients/go` (`clients/go`).

```go
db, err := relate.Open(":memory:")
reader, err := relate.OpenReadOnly(":memory:")
conn, err := db.Connect()
conn, err := db.ConnectNamespace("system") // ADR-0117; Connect is ConnectNamespace("neo4j")
err = conn.SetNamespace("neo4j")           // case-insensitive; only between transactions
err = db.Checkpoint()
err = conn.Begin()
bookmark, err := conn.Commit()  // "relate:<n>", or "" when nothing committed (ADR-0502 §7)
err = conn.Rollback()
result, err := conn.Query("RETURN 1 AS n")
result, err := conn.QueryParams("RETURN $n AS n", map[string]any{"n": 1})
// result.Columns []string
// result.Rows [][]any            — decoded cells, see "Values" below
// result.QueryType QueryType     — QueryTypeRead | QueryTypeWrite | QueryTypeReadWrite | QueryTypeSchema (ADR-0162)
// result.Namespace Namespace     — NamespaceNeo4j | NamespaceSystem | NamespaceSystemOntologies (ADR-0117)
// result.Bookmark string         — auto-commit bookmark, or "" (ADR-0502 §7)
// result.Stats Stats             — the 11 mutation counters, zeros included (ADR-0216)
// result.Plan *PlanDocument      — the relate.plan/v1 document (ADR-0381), nil on a plain run
// result.Notifications []Notification — query notifications (ADR-0363)
```

`OpenReadOnly` loads once and refuses writes, checkpoints, and creation
(ADR-0136) — a missing store is an error. `Open` stays writable: it creates
a missing store at open, in both target forms (a Native directory and a
`.rl` bundle).

## Errors (ADR-0502 §2, §3)

Two classes of error reach the caller:

- **Engine and boundary failures** are a `*relate.Error`, decoded from the
  ABI's error shape (`code`, `gql_status`, `stage`, `retryable`, `message`).
- **Client-side refusals** are plain `error` values, not `*Error`. See the
  end of this section.

The `*Error` type:

```go
type Error struct {
    Code      string // Neo4j status code, e.g. "Neo.ClientError.Statement.ArithmeticError"
    GQLStatus string // five-character GQLSTATUS, e.g. "22G03"
    Stage     Stage  // StageCompile | StageRuntime | StageInternal
    Retryable bool   // true exactly when re-running the same work may succeed
    Message   string // the engine's message, or the boundary's for StageInternal
}
func (e *Error) Error() string // returns Message
```

Match with `errors.As` and branch on the fields, never on `Message`:

```go
var e *relate.Error
if errors.As(err, &e) && e.Retryable {
    // write-write conflict or a retryable resource cap: retry the unit of work
}
```

Where an error arrives:

- **A statement failure** (syntax, type, arithmetic, constraint, conflict,
  missing parameter, …) rides the result. `QueryParams` returns `nil` and the
  `*Error`. The result is destroyed before the call returns.
- **A connection-level failure** (open, connect, `SetNamespace`, `Begin`,
  `Rollback`, `Checkpoint`, `Commit`, a parameter the builder refuses) returns
  the `*Error` from the call's own `error_json` out-param.
- **`Commit`** returns `("", err)` on failure. A conflict on an explicit
  transaction surfaces here, so a managed retry loop must check both `Query`
  and `Commit`.
- **`StageInternal`** means a boundary failure (a poisoned handle, a caught
  panic, a refused parameter). Do not retry it; close the handle and open a new
  one.

Client-side refusals are plain `error` values, not `*Error`. They name the
violation: `ABI contract violated: ...` (a shape that breaks the ABI contract),
`unsupported value kind N` (a value kind this client does not know), or one of
the parameter refusals listed under "Parameters". Match them by text only for
diagnostics; never branch on them. Any untrusted wire value or payload a
refusal echoes (an unknown query type, namespace, stage, severity or category,
a malformed notification payload) is sanitized: control and other
non-printable characters appear as `\uXXXX` escapes and the echo is bounded
(64 characters for a value, 256 for a payload), so a hostile native library
cannot inject log lines (CWE-117).

## Values (ADR-0502 §1)

`Query` and `QueryParams` decode each cell through the typed value walker. The
decode is one pass per result, and nothing is parsed as JSON. Each engine kind
maps to one Go type:

| Engine kind | Go type |
| --- | --- |
| `NULL` | `nil` |
| `BOOL` | `bool` |
| `INT` | `int64` (lossless) |
| `FLOAT` | `float64` (NaN and ±Inf included) |
| `STRING` | `string` (an interior NUL is kept) |
| `LIST` | `[]any` |
| `MAP` | `map[string]any` (engine order is not kept; Go maps are unordered) |
| `NODE` | `relate.Node{ElementID, ID int64, Labels []string, Properties map[string]any}` |
| `RELATIONSHIP` | `relate.Relationship{ElementID, StartElementID, EndElementID, ID, StartID, EndID int64, Type, Properties}` |
| `PATH` | `relate.Path` (`[]any`: node, relationship, …, node) |
| `POINT` | `relate.Point{SRID int32, Dimension int32, X, Y, Z float64}` (`Z` set only when `Dimension` is 3) |
| `DATE` | `relate.Date{Year, Month, Day int64}` |
| `TIME` | `relate.Time{Hour, Minute, Second, Nanosecond, OffsetSeconds int64}` |
| `LOCAL_TIME` | `relate.LocalTime{Hour, Minute, Second, Nanosecond int64}` |
| `DATETIME` | `relate.DateTime{Year…Nanosecond, OffsetSeconds int64, Zone string}` |
| `LOCAL_DATETIME` | `relate.LocalDateTime{Year…Nanosecond int64}` |
| `DURATION` | `relate.Duration{Months, Days, Seconds, Nanos int64}` |
| `VECTOR` | `relate.Vector{DType string, Ints []int64, Floats []float64}` (the slice for the other family is nil) |

Temporals are components. The zoned kinds report their local wall clock; `Zone`
is the IANA id, or `""` when the value carries only an offset. Byte arrays come
back as `[]any` of `int64`, and a UUID as `string`. A node's `ElementID` is `""`
in the `system` namespace, which has no element ids. Every entity also carries
the deprecated numeric entity ids (ADR-0506): `Node.ID` / `Relationship.ID` are
exactly the INTEGER Cypher `id()` returns for the same entity, and
`Relationship.StartID` / `EndID` the endpoints' numeric ids, as
`id(startNode(r))` / `id(endNode(r))` return them — prefer the element ids for
lookups. A kind this client does not know is an error, never a `nil`.

## Parameters (ADR-0502 §4)

`QueryParams` binds its map through the typed params builder. No JSON is
produced. A `nil` map runs the statement with no parameters. A non-nil map
(including an empty one) is bound as the statement's parameter map. The Go
value maps to one value kind:

| Go value | Engine value |
| --- | --- |
| `nil`, a nil pointer, interface, slice or map | `NULL` |
| `bool` | `BOOL` |
| any signed or unsigned integer | `INT` (an unsigned value above `MaxInt64` is refused) |
| `float32`, `float64` | `FLOAT` |
| `string` | `STRING` (an interior NUL is kept) |
| slice or array (`[]byte` included) | `LIST` |
| `map[string]T` (string keys) | `MAP`, keys sorted before binding |
| struct | `MAP` of its exported fields, keyed by the `json` tag name; `json:"-"` is skipped and `omitempty` drops a zero value |

A value the builder cannot bind is refused before the statement runs, as a
plain `error` (not `*Error`). The message names the parameter, prefixed
`parameter "<name>": `, and adds the nested position (`item N`, `field "f"`,
`key "k"`) when the value sits inside a container. The refusal texts:

| Case | Message (after the prefix) |
| --- | --- |
| unsigned integer above `MaxInt64` | `unsigned value N exceeds the INT range` |
| map whose key type is not a string | `map key type K is not a string` |
| struct with no exported fields (`time.Time` among them) | `struct type T has no exported fields to bind` |
| any other type (channel, function, complex, …) | `unsupported parameter type T` |

A name, not a value, is refused outside that prefix shape: a parameter,
key or field name holding an interior NUL never crosses the C boundary —
`parameter "a\x00b" holds an interior NUL and cannot cross the C boundary`
(the one `cString` refusal every NUL-terminated port shares). The
statement text is refused the same way before it runs: `statement holds
an interior NUL and cannot cross the C boundary`.

For example, `map[string]any{"c": make(chan int)}` fails with
`parameter "c": unsupported parameter type chan int`.

## Result metadata (ADR-0285, ADR-0363)

Every `Result` carries its construction-time metadata alongside `Columns` and
`Rows`, populated from the `relate_result_*` accessors while the result is
alive:

- `Result.QueryType` — the statement's plan-time classification (ADR-0162)
  as the typed `QueryType` vocabulary: `QueryTypeRead` (`"r"`),
  `QueryTypeWrite` (`"w"`, a write with no returned columns),
  `QueryTypeReadWrite` (`"rw"`, a write with returned columns) or
  `QueryTypeSchema` (`"s"`), the split per ADR-0332. The decoder refuses any
  other value as an ABI contract violation. EXPLAIN and PROFILE report the
  wrapped statement's type without executing it, so `EXPLAIN CREATE (n)`
  reports `QueryTypeWrite` and creates nothing.
- `Result.Namespace` — the statement's *resolved* namespace (ADR-0117 §7)
  as the typed `Namespace` vocabulary: exactly `NamespaceNeo4j`
  (`"neo4j"`), `NamespaceSystem` (`"system"`) or
  `NamespaceSystemOntologies` (`"system_ontologies"`); the decoder refuses
  any other value as an ABI contract violation. This is the `USE`
  target when the statement carries a `USE` clause, otherwise the session
  namespace; a `USE`-routed statement reports its target without changing
  the session namespace, so a following unrouted statement still reports the
  session namespace.
- `Result.Stats` — the statement's mutation counters (ADR-0216) as the typed
  `Stats`, decoded from `relate_result_stats`: the canonical 11 —
  `NodesCreated`, `NodesDeleted`, `RelationshipsCreated`,
  `RelationshipsDeleted`, `PropertiesSet`, `LabelsAdded`, `LabelsRemoved`,
  `IndexesAdded`, `IndexesRemoved`, `ConstraintsAdded`, `ConstraintsRemoved`
  — zeros included, so a read, an EXPLAIN (nothing executes) or a no-op
  write reports all zeros. The counting follows Neo4j 4.4 (ADR-0309):
  labels per node, one `PropertiesSet` per property write, schema DDL in
  the index and constraint counters. The decoder requires all 11 keys: a
  missing one is an ABI contract violation, never a zero value in its
  place.
- `Result.Plan` — the statement's structured plan (LABS-418, ADR-0381) as typed
  values: `*PlanDocument` (`Schema`, `Profiled`, `Statement`, `Root`), `PlanNode`
  (`Operator`, `Details`, `Identifiers`, `EstimatedRows`, `Actual`, `Children`,
  `Warnings`), `PlanActual` (the PROFILE metrics) and `PlanStart`; `nil` on a plain
  run. Each node's `Warnings` carry the operator's remedy-bearing notifications
  (ADR-0363), joined by operator id.
- `Result.Notifications` — the statement's query notifications (ADR-0363) in
  the engine-native shape, decoded from `relate_result_notifications`. It
  is an empty non-nil slice on a plain run — a plain run derives no
  notifications — and is populated by `EXPLAIN` and `PROFILE`, which build
  the plan tree the list comes from:

  ```go
  type Notification struct {
      Code        string   `json:"code"`
      GQLStatus   string   `json:"gql_status"`
      Severity    Severity `json:"severity"`
      Category    Category `json:"category"`
      Title       string `json:"title"`
      Description string `json:"description"`
      Remedy      string `json:"remedy"`
      OperatorID  *int64 `json:"operator_id,omitempty"`
  }
  ```

  `Severity` is the closed `Severity` vocabulary (`SeverityInformation`
  `"INFORMATION"`, `SeverityWarning` `"WARNING"`) and `Category` the closed
  `Category` vocabulary (`CategoryPerformance`, `CategoryUnrecognized`,
  `CategoryDeprecation`, `CategorySchema`, `CategoryHint`, each the upper-case
  wire name). The decoder refuses a missing, null or unknown value — in the
  statement's list and in a plan node's `Warnings` — as an ABI contract
  violation naming the key and the offending value (`ABI contract violated:
  notification entry carries an unknown severity "LOUD": {...}`), the same
  strictness as the Java and Swift clients.

  `OperatorID` is set only on operator-scoped kinds and names the raising
  plan operator's id; statement-scoped kinds leave it `nil`. For example,
  `EXPLAIN MATCH (a:Person), (b:City) RETURN a, b` yields one entry with
  `Code == "Neo.ClientNotification.Statement.CartesianProduct"`,
  `GQLStatus == "03N90"`, `Severity == "INFORMATION"`,
  `Category == "PERFORMANCE"` and a non-empty `Remedy`.

See [`examples/result_metadata.go`](examples/result_metadata.go) and
[`examples/stats.go`](examples/stats.go).

**Byte arrays (ADR-0327).** The engine's statement-scoped byte array enters only through Bolt or a Rust `Value::Bytes` parameter, never through this surface's own parameters. A result value holding one renders as a list of integers 0–255 (the `value_to_cypher` crossing) and its column stamps `BINARY`; byte arrays cannot be stored as properties.

Namespaces (ADR-0117) are the fixed set `neo4j`, `system`,
`system_ontologies`, matched case-insensitively; the namespace is the session
default for statements without a `USE` clause. An unknown name fails with a
message ending `Graph not found: <name>` (ADR-0146).
`SetNamespace` fails while an explicit transaction is open with a message
ending `Cannot change the namespace while a transaction is open.`

`Query` is `QueryParams` with a nil map. `Begin` / `Commit` / `Rollback` are an
explicit transaction (ADR-0028); `Close` is implicit rollback. A write-write
conflict is a `*Error` with `Retryable == true` (`Neo.TransientError.Transaction.Outdated`);
retry the unit of work in the caller (ADR-0017).

The package links `librelate_ffi` (ABI version 2, ADR-0502). Build it with
`cargo build --release -p relate-ffi` from the repository root; the cgo
directives search `target/debug` and `target/release`. On macOS the linker
needs `librelate_ffi.dylib` (a Linux `.so` left in `target/debug` will
fail with `unknown file type`). Set `DYLD_LIBRARY_PATH` (or `LD_LIBRARY_PATH`
on Linux) to the FFI library directory when running a binary outside the
build tree. The C contract is [`docs/api/c/spec.md`](../c/spec.md).

Fraud and identity load-test binaries (`cmd/fraud-loadtest`,
`cmd/identity-loadtest`) wrap this client. Flags, mix, and report JSON are
documented in [`docs/datasets.md`](../../datasets.md).
