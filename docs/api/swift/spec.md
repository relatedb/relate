# Swift client — spec

Package: `Relate` (SwiftPM, repository root `Package.swift`, sources in
`clients/swift`), ADR-0433. Platforms: iOS 16+, macOS 13+. Swift 6 language
mode. Wraps the tabular C ABI v2 ([`../c/spec.md`](../c/spec.md)) only: the
typed value walker, the typed params builder, the error channel that
travels with the call, and bookmarks.

```swift
// Package.swift of a consumer
// pin a release tag that carries the Swift binary (releaseTag in Package.swift is non-empty)
.package(url: "https://github.com/relatedb/relate", from: "<release>")
// target dependency
.product(name: "Relate", package: "relate")
```

## Types

```swift
// `throws(RelateError)` is a typed throw: a `catch` block's `error` is a
// `RelateError` with no cast. `transaction(_:)` and `read(_:)` run the
// caller's closure, so they stay untyped.
public final class Database: Sendable {
    public init(path: String, options: Options = Options()) throws(RelateError)            // ":memory:", a single-file `.rl` store, or a store directory (created if missing)
    public init(readOnlyPath path: String, options: Options = Options()) throws(RelateError) // existing store, loaded once; writes/checkpoint refused (ADR-0136)
    public init(url: URL, readOnly: Bool = false, options: Options = Options()) throws(RelateError) // a local file URL; another scheme, or a host other than localhost, is .invalidParameter
    public static func inMemory(options: Options = Options()) throws(RelateError) -> Database
    public struct Options: Sendable, Hashable { public var maxReaders: Int /* = 2; below 1 is .invalidParameter at open */ }
    public let path: String
    public let isReadOnly: Bool
    public func connect() throws(RelateError) -> Connection                              // namespace "neo4j"
    public func connect(namespace: DatabaseNamespace) throws(RelateError) -> Connection  // .neo4j | .system | .systemOntologies (ADR-0117)
    @available(*, deprecated) public func connect(namespace: String?) throws(RelateError) -> Connection  // by name; use the typed form
    public func checkpoint() throws(RelateError)

    // Managed asynchronous access — see "Managed asynchronous access" below.
    public func read<T: Sendable>(_ body: @escaping @Sendable (Transaction) throws -> T) async throws -> T
    @discardableResult public func write<T: Sendable>(retries: Int = 0, tags: Set<String> = [], _ body: @escaping @Sendable (Transaction) throws -> T) async throws -> T
    public func read(_ statement: Statement) async throws -> QueryResult
    @discardableResult public func write(_ statement: Statement, retries: Int = 0, tags: Set<String> = []) async throws -> QueryResult
    @discardableResult public func write(_ statements: [Statement], retries: Int = 0, tags: Set<String> = []) async throws -> [QueryResult]
    public func checkpointInBackground() async throws

    // Observation — see "Observation" below.
    public func setTraceHandler(_ handler: (@Sendable (QueryTrace) -> Void)?)
    public func commits(bufferingPolicy: AsyncStream<Commit>.Continuation.BufferingPolicy = .unbounded) -> AsyncStream<Commit>

    // Live values — see "Live values" below.
    public func values<T: Sendable>(tags: Set<String>? = nil, _ body: @escaping @Sendable (Transaction) throws -> T) -> AsyncThrowingStream<T, any Error>
    public func values<T: Sendable & Equatable>(tags: Set<String>? = nil, _ body: @escaping @Sendable (Transaction) throws -> T) -> AsyncThrowingStream<T, any Error>  // skips a value equal to the last
}

public final class Connection {                  // NOT Sendable
    public let database: Database
    public func query(_ cypher: String, _ parameters: [String: CypherValue] = [:]) throws(RelateError) -> QueryResult
    public func setNamespace(_ namespace: DatabaseNamespace) throws(RelateError)  // only between transactions
    public func setStatementTimeout(_ timeout: Duration?) throws(RelateError)  // ADR-0692; nil clears; a statement past it throws .engine with TransactionTimedOutClientConfiguration
    @available(*, deprecated) public func setNamespace(_ name: String) throws(RelateError)  // by name; use the typed form
    public func begin() throws(RelateError)
    @discardableResult public func commit() throws(RelateError) -> String?  // the transaction's bookmark; nil when it committed nothing
    public func rollback() throws(RelateError)
    public func transaction<T>(retries: Int = 0, tags: Set<String> = [], _ body: (Transaction) throws -> T) throws -> T
    public func read<T>(_ body: (Transaction) throws -> T) throws -> T               // always rolled back
    @discardableResult public func runInTransaction(_ statements: [Statement], retries: Int = 0, tags: Set<String> = []) throws(RelateError) -> [QueryResult]
    public func isPoisoned() -> Bool                                                 // probes with RETURN 1
}

public final class Transaction {                 // NOT Sendable; valid only inside the body it was passed to
    public var isReadOnly: Bool { get }
    public func query(_ cypher: String, _ parameters: [String: CypherValue] = [:]) throws(RelateError) -> QueryResult
    public func query(_ cypher: String, _ parameters: [String: any CypherValueConvertible]) throws(RelateError) -> QueryResult
    public func run(_ statement: Statement) throws(RelateError) -> QueryResult
    public func run(_ statements: [Statement]) throws(RelateError) -> [QueryResult]
}

public struct QueryResult: Sendable {
    public let columns: [String]
    public let rows: [[CypherValue]]
    public let bookmark: String?                 // ADR-0502 §7: the auto-commit write's bookmark ("relate:<n>");
                                                 // nil for a read, an empty write pipeline, EXPLAIN, a statement
                                                 // inside an explicit transaction, or an error result
    public let queryType: QueryType              // ADR-0162: .read "r" | .write "w" | .readWrite "rw" | .schema "s"
    public let namespace: DatabaseNamespace      // ADR-0117: .neo4j | .system | .systemOntologies (USE target, else session)
    public let stats: QueryStats                 // ADR-0216: the canonical 11 counters
    public let notifications: [QueryNotification] // ADR-0363: [] on a plain run
    public let plan: PlanDocument?               // ADR-0381: relate.plan/v1; nil on a plain run
    public let elapsedMs: Double                 // ADR-0221: engine parse + plan + execute
    public let parseMs: Double                   // ADR-0491: parse + lowering window
    public let planMs: Double                    // ADR-0491: binding + planning window
    public let executeMs: Double                 // ADR-0491: dispatch (or EXPLAIN render) window
                                                 // parseMs + planMs + executeMs <= elapsedMs
    public var records: [Record] { get }                      // the records iterating the result yields
    public func scalar() throws(RelateError) -> CypherValue   // exactly one row and one column, else .unexpectedResult
}
// QueryResult is Equatable (every field, timings included) and a
// RandomAccessCollection of Record — see "Records" below.

public enum CypherValue: Sendable, Hashable, CustomStringConvertible {
    case null, bool(Bool), int(Int64), float(Double), string(String)
    case list([CypherValue]), map([String: CypherValue])
    case node(Node), relationship(Relationship), path(Path)
    case point(Point), date(Date), time(Time), localTime(LocalTime)
    case dateTime(DateTime), localDateTime(LocalDateTime)
    case duration(Duration), vector(Vector)
}

public enum CypherValueKind: UInt32, Sendable, Hashable, CaseIterable {
    case null, bool, int, float, string, node, relationship, path, list, map
    case point, date, time, localTime, dateTime, localDateTime, duration, vector
}

public enum RelateError: LocalizedError, Sendable, Equatable {
    case engine(EngineError)       // the typed error shape (below)
    case invalidParameter(String)  // refused before reaching the engine
    case decoding(String)          // the C ABI broke its documented contract
    case unexpectedResult(ResultMismatch) // a result was not the shape or type asked for (Records, below)
    case misuse(String)            // used outside its contract: an ended transaction scope, a write in a read
}

public struct ResultMismatch: Sendable, Hashable {
    public let path: [String]      // the column, then map keys and list positions ("[2]") below it; [] for a whole-result shape
    public let expected: String    // a Swift type name, or a shape ("exactly one record")
    public let found: String       // a value kind ("null", "string"), or a shape ("2 records")
    public let reason: String?     // why an in-kind value did not convert ("300 is out of range"), or the text of a type's own decoding error
    public var message: String     // "rating: expected Double, found null"
}

public struct EngineError: Sendable, Hashable {
    public let code: String        // "Neo.ClientError.Statement.ArithmeticError" — pass through unchanged
    public let gqlStatus: String   // the GQLSTATUS, e.g. "22G03"
    public let stage: Stage        // .compile | .runtime | .internal (a boundary failure)
    public let retryable: Bool     // the drivers' transient test: retry exactly when true
    public let message: String
    public enum Stage: String, Sendable, Hashable { case compile, runtime, internal }

    public var classification: Classification?   // the code's second segment; nil for a code not shaped `Neo.<Class>.<Category>.<Title>`
    public enum Classification: String, Sendable, Hashable { case clientError, transientError, databaseError }
    public var isConstraintViolation: Bool       // Neo.ClientError.Schema.ConstraintValidationFailed
    public var isSyntaxError: Bool               // Neo.ClientError.Statement.SyntaxError
    public var isMissingParameter: Bool          // Neo.ClientError.Statement.ParameterMissing
    public var isWriteConflict: Bool             // Neo.TransientError.Transaction.Outdated (retryable)
    public enum StatusCode { /* the four code strings above, as static constants */ }
}

extension RelateError {
    public var isRetryable: Bool                 // engineError?.retryable ?? false
}
```

### Names

The result-metadata types are `QueryNotification`, `NotificationSeverity`,
`NotificationCategory` and `DatabaseNamespace`. They were `Notification`,
`Severity`, `Category` and `Namespace`, which collide with
`Foundation.Notification`, `SwiftUI.Namespace` and the Objective-C runtime's
`Category`: a file importing both modules could not spell them unqualified.
`Severity` and `Category` remain as deprecated aliases. `Notification` and
`Namespace` are removed, because an alias of either name keeps the ambiguity.

`CypherValue` conforms to every `ExpressibleBy…Literal` protocol, so literal
parameters need no wrapping (`["n": 1, "tags": ["a"], "m": ["k": true]]`);
variables use `CypherValue(x)` (`Int`, `Double`, `String`, `Bool`) or the
typed struct initializers for the temporal / point / vector cases. Accessors:
`boolValue`, `intValue`, `doubleValue` (int widened), `stringValue`,
`listValue`, `mapValue`, `nodeValue`, `relationshipValue`, `pathValue`,
`pointValue`, `dateValue`, `timeValue`, `localTimeValue`, `dateTimeValue`,
`localDateTimeValue`, `durationValue`, `vectorValue`, `kind`, `isNull`, and
`subscript(key:)` over maps and node/relationship properties.

`Node` is `{id, elementId, labels, properties}` and `Relationship` is
`{id, elementId, type, startId, endId, startElementId, endElementId,
properties}` — the element ids are the engine's own spelling
(`"4:rl-id:n0"`), exactly what `elementId(n)` returns, so
`WHERE elementId(n) = $id` round-trips through a string parameter. The
numeric `id` / `startId` / `endId` (`Int64`) are the deprecated numeric
entity ids (ADR-0506): exactly the INTEGERs Cypher's `id()` /
`id(startNode(r))` / `id(endNode(r))` return for the same entities — read
them for driver-style entity equality, keep `elementId()`-based lookups on
the element ids. `Path` is `{nodes, relationships}`
(`nodes.count == relationships.count + 1`, in walk order). `Point` is
`{srid, x, y, z?}` (`z` only for the 3D srids 9157/4979; `x`/`y` are
longitude/latitude for the geographic srids 4326/4979). `Date`, `LocalTime`,
`LocalDateTime` carry their components; `Time` adds `offsetSeconds` (a TIME
keeps no zone id — the ABI's contract); `DateTime` adds `offsetSeconds` and
`zone: String?` (`nil` for offset-only values). `Duration` is
`{months, days, seconds, nanoseconds}` — the four lossless components
(`PT36H` stays 36 hours, not 1½ days). `Vector` is
`case floats(dtype:values:)` / `case ints(dtype:values:)` with the eight
canonical dtype names (`INTEGER8`…`FLOAT`) as a closed `DType` enum.
`QueryStats`, `QueryNotification`, `PlanDocument`, `PlanNode`, `PlanActual` and
`PlanStart` carry the same fields as the Go client's types, in camelCase.

## Value decoding

Cells decode through the C ABI's typed value walker (ADR-0502 §1:
`relate_value_at` + the `relate_value_*` accessors) — there is no
wire JSON and no per-column type stamp. Every value carries its kind at every
depth: a map that merely looks like a node stays `.map`, a path nested in a
list stays `.path`, and `value.kind` (``CypherValueKind``) reads it back. An
enumerator this client does not know (a later, additive ABI) is a defined
`.decoding` error, never a silent null. The result metadata is closed the same
way: a `query_type` outside `r`/`w`/`rw`/`s` or a `namespace` outside the three
fixed names is a `.decoding` error reading `ABI contract violated: ...`, and so
is a notification `severity` outside the closed `NotificationSeverity` enum
(`.information`, `.warning`) or a `category` outside the closed `NotificationCategory` enum
(`.performance`, `.unrecognized`, `.deprecation`, `.schema`, `.hint`) — a
missing or null key too — as in the Go and Java clients. The refusal names the
key and the offending value, sanitized: control and other non-printable
characters appear as `\uXXXX` escapes and the echoed value is bounded (64
characters; an echoed payload 256), so a hostile native library cannot inject
log lines (CWE-117).

| ABI kind | Swift value |
| --- | --- |
| NULL / BOOL / INT / FLOAT / STRING | `.null` / `.bool` / `.int` / `.float` / `.string` (`1.0` stays `.float`; NaN and ±∞ stay `.float`) |
| NODE / RELATIONSHIP / PATH | `.node` / `.relationship` / `.path` |
| LIST / MAP | `.list` / `.map` — a map key holding an interior `\u{0}` throws `.decoding`: `relate_value_map_get` takes a NUL-terminated key, so the client refuses rather than return another key's value (string values keep their NULs) |
| POINT / DATE / TIME / LOCAL_TIME / DATETIME / LOCAL_DATETIME / DURATION / VECTOR | the typed structs above |

Strings and map keys carry a byte length, so interior NULs cross whole
(`'a\u0000b'` is `.string("a\u{0}b")`); a present key holding `null`
(`.map(["a": .null])`) is distinct from an absent key. UUIDs cross as
`STRING`, byte arrays as `LIST` of `INT`.

## Managed asynchronous access

`Database.read` and `Database.write` run a **synchronous** body on a
connection the database opens and owns, off the caller's thread:

```swift
let titles = try await db.read { tx in
    try tx.run("MATCH (b:Book) RETURN b.title AS title").map { try $0.get("title", as: String.self) }
}
try await db.write { tx in
    try tx.run("MATCH (b:Book {key: \(key)}) SET b.rating = \(4)")
}
```

- **Connections.** One writer connection and up to `Options.maxReaders`
  reader connections (default 2), each opened on first use on the default
  namespace and confined to its own serial dispatch queue. A read goes to the
  least busy reader; another reader is opened only while every existing one
  has work pending.
- **`read`** is `Connection.read`: a transaction that is always rolled back.
  Its statements see one snapshot, nothing it does is kept, and a statement
  that is not a read throws `.misuse`. Reads run concurrently with each other
  and with a write, and do not see a write's uncommitted changes.
- **`write`** is `Connection.transaction(retries:)` on the single writer:
  committed when the body returns, rolled back when it throws. Writes through
  the database run one at a time and so never conflict with each other;
  `retries` covers a conflict with a writer outside the pool. Schema
  statements are writes. On a database opened read-only, `write` and
  `checkpointInBackground` throw `.misuse`.
- **Blocking.** The awaiting task suspends and the engine call blocks the
  connection's queue, so no cooperative-pool thread is held and the
  `MainActor` can `await` these directly. The body cannot `await`, so a
  transaction is never held across a suspension.
- **Cancellation.** A task cancelled before its body starts — before the
  call, or while the call waits its turn on a connection — throws
  `CancellationError` when that turn comes, and runs nothing. A statement
  already running is not interrupted; the C ABI has no cancel call.
- **Sendability.** The body is `@Sendable` and its result is `Sendable`; the
  `Transaction` it receives is not, and never leaves the body.
- **Poison.** After a body fails at stage `internal`, the connection is
  probed (`isPoisoned()`); a poisoned one is discarded and replaced on the
  next use. The failed body's error is rethrown and the body is not run
  again.
- **Lifetime.** The pool's connections keep the store open but do not keep
  the `Database` object alive: releasing the last `Database` reference
  releases them and, with no other connection outstanding, closes the store.
- `checkpointInBackground()` checkpoints on the writer's queue, between
  writes.

The synchronous `connect()` API is unchanged and may be used alongside; a
connection made that way is a writer outside the pool.

## Observation

```swift
public struct QueryTrace: Sendable {
    public let statement: String           // the text the engine received, spliced literals included
    public let parameterNames: [String]    // sorted; the names, not the values
    public let rowCount: Int               // 0 when the statement failed
    public let queryType: QueryType?       // nil when it failed
    public let elapsedMs, parseMs, planMs, executeMs: Double  // engine-reported; 0 when it failed
    public let wallMs: Double              // the whole call: binding, engine, copying the result out
    public let error: RelateError?         // the engine's message; may quote a value
}

public struct Commit: Sendable, Hashable {
    public let bookmark: String            // "relate:<n>"
    public let stats: QueryStats           // summed over the transaction's statements
    public let tags: Set<String>           // what the writer attached; empty for an auto-commit statement
}
```

- **Tracing.** `setTraceHandler` installs one handler per database (`nil`
  clears it). It is called once for every statement run on any connection
  of that database — a caller's own or a pooled one — synchronously on the
  thread that ran the statement, after it finished, failures included. A
  trace has parameter names, not values. Its `statement` holds any literal
  spliced into the text instead of bound, and its `error` may quote a value
  (the engine names a refused parameter with what it held): log those two
  only when they carry no user data.
- **Signposts.** Every statement is also bracketed by an `os_signpost`
  interval named `statement` (subsystem `ai.relatedb.relate`, category
  `Query`), with no configuration. The package's tests do not assert on
  signposts.
- **Commits.** `commits()` returns a new `AsyncStream` per call. A stream
  receives each auto-commit statement that returned a bookmark and each
  explicit transaction whose `commit()` returned one, made after the stream
  was created. A read, a rolled-back transaction and a write that changed
  nothing report nothing; a retried transaction reports once. The stream
  ends when the store closes, and releasing a stream unsubscribes it.
- **Order.** A commit is published as soon as the engine has made it: an
  auto-commit statement's before the trace handler is called for that
  statement, a transaction's after the traces of its statements. Commits
  from one connection (every `Database.write` shares the writer) arrive in
  commit order. Two connections committing at the same moment may arrive in
  either order; their bookmarks tell which came first.
- **Buffering.** Events wait in the stream until consumed: without bound by
  default, so an observer that stops consuming while writes continue grows
  the backlog. `commits(bufferingPolicy: .bufferingNewest(n))` keeps only
  the latest `n`, which is enough for an observer that reloads on any
  commit.
- **Tags** are the caller's routing labels: `db.write(tags: ["notes"]) { … }`
  or `conn.transaction(tags:)`. The engine does not report which labels a
  commit touched, so an observer that wants to react selectively relies on
  the writer's tags or on `stats`.
- **Scope.** Both are in-process and per `Database` object. A write by
  another process, or through a second `Database` opened on the same store,
  is not seen.

## Live values

A live value is a read that is run again when a commit may have changed its
answer. `Database.values` is the stream form; `@LiveQuery` is the same
observation as a SwiftUI property.

```swift
for try await books in db.values(tags: ["books"]) { tx in
    try tx.run("MATCH (b:Book) RETURN b.title AS title ORDER BY title").decode(Book.self)
} {
    show(books)
}
```

- **Order.** The stream yields the current value first. It subscribes to
  the commit stream before that first read, so a write made at any point
  after the call is followed by a read that sees it.
- **Which commits.** With `tags: nil` (the default) every commit made
  through the `Database` object is followed by a read. With a set, only a
  commit carrying at least one of its tags is. An auto-commit statement
  carries no tag, and a writer that forgets its tag leaves the value stale:
  narrow with tags only where every writer sets them. The engine does not
  report which labels a commit touched, so there is no automatic tracking.
- **Reads.** `body` runs as `Database.read`: one snapshot, nothing kept.
  Commits that arrive while a read runs are followed by one read, not one
  each, and a consumer that falls behind receives the latest value only.
- **Repeats.** For an `Equatable` value, a read equal to the last value
  yielded yields nothing, so a commit that left the answer alone is not a
  new value. A consumer that fell behind while the answer changed and
  changed back can still receive the same value twice. A value that is not
  `Equatable` is yielded after every read.
- **End.** A read that throws ends the stream with its error. The stream
  also ends when its consumer stops (a cancelled task, a released stream)
  and when the store closes; an observation does not keep the store open.
- **Scope.** As for the commit stream: in-process and per `Database`
  object.

### `@LiveQuery` (SwiftUI)

```swift
@MainActor @propertyWrapper
public struct LiveQuery<Value: Sendable & Equatable>: DynamicProperty {
    public init(initial: Value, tags: Set<String>? = nil, id: AnyHashable = AnyHashable(0),
                _ read: @escaping @Sendable (Transaction) throws -> Value)
    public init<Element: Decodable>(_ statement: @autoclosure () throws -> Statement, tags: Set<String>? = nil,
                                    using decoder: CypherValueDecoder = CypherValueDecoder()) where Value == [Element]
    public var wrappedValue: Value { get }            // the latest value; `initial` (or []) before the first read
    public var projectedValue: LiveQueryState { get }
}

public enum LiveQueryState: Sendable {
    case loading                      // no value or failure yet since the query last started
    case live                         // the property holds the latest value and follows commits
    case failed(any Error)            // the observation ended: a read threw, or there is no database
    public var isLoading: Bool { get }
    public var isLive: Bool { get }
    public var error: (any Error)? { get }
}

extension EnvironmentValues { public var relateDatabase: Database? { get set } }
extension View { public func relateDatabase(_ database: Database?) -> some View }
```

```swift
struct Library: View {
    @LiveQuery("MATCH (b:Book) RETURN b.title AS title ORDER BY title", tags: ["books"])
    private var books: [Book]

    var body: some View {
        List(books, id: \.title) { Text($0.title) }
            .overlay { if $books.isLoading { ProgressView() } }
    }
}

// once, near the root
ContentView().relateDatabase(db)
```

- The property reads the database in the environment. With none there, the
  query reports `.misuse` through `$books.error`; it does not trap.
- The observation lasts as long as the view. It starts again when the query
  becomes a different one: when the statement or one of its bound values
  changes (for the closure form, when `id` changes), when `tags` changes,
  or when the database in the environment does. The decoder is not part of
  a query's identity. A query that depends on the view's inputs is built in
  the view's initializer: `_books = LiveQuery(try "… \(search) …")`.
- A changed query is `.loading` from the view update that changed it. The
  previous value stays on screen while it runs its first read, and after a
  read fails.
- A read equal to the value the property holds redraws nothing.
- A statement whose interpolation throws makes the query `.failed` with
  that error from the view's first body; nothing is read, and a view's
  initializer does not have to throw.
- Available where SwiftUI is (`#if canImport(SwiftUI)`), from the package's
  minimum platforms: the property wrapper uses `StateObject`, not the
  Observation framework.

## Provisioning and migration

```swift
extension Database {
    public static func defaultStoreURL(named name: String) throws -> URL
    @discardableResult public static func provision(
        at target: URL, from seed: URL,
        protection: FileProtectionType? = .completeUntilFirstUserAuthentication) throws -> Bool
}

public struct Migrator: Sendable {
    public init(versionLabel: String = "RelateMigration")   // the version node is (:label {key: 'schema_version'}).version
    public var versionLabel: String
    public var schema: [Statement]                          // run every time; must be idempotent
    public mutating func register(version: Int, _ statements: [Statement])
    public mutating func register(version: Int, _ body: @escaping @Sendable (Transaction) throws -> Void)
    public var latestVersion: Int { get }
    public func currentVersion(_ connection: Connection) throws(RelateError) -> Int   // 0 for a store never migrated; a broken record (several version nodes, or one without an integer version) is .unexpectedResult
    @discardableResult public func migrate(_ connection: Connection) throws -> Int
    @discardableResult public func migrate(_ database: Database) async throws -> Int  // on the writer connection
}
```

- **`defaultStoreURL(named:)`** is `name` inside the user's Application
  Support directory — on macOS inside a folder named for the app's bundle
  identifier, since that directory is shared between apps (a process with no
  bundle identifier, such as a command-line tool, gets `name` directly in
  that directory). It creates the containing directory, not the store. `name` is one path component: an
  empty name, `.`, `..` or one containing `/` is `.invalidParameter`.
- **`provision(at:from:)`** copies a seed store to `target` when nothing is
  there, and returns whether it copied. A store bundled with an app cannot
  be opened in place (the bundle is read-only and a store writes beside
  itself). The copy goes to a temporary name in the target's directory and is
  renamed into place, so an interrupted launch leaves no partial store. A
  temporary copy an interrupted launch left behind is removed by a later
  call, once it was started more than ten minutes ago (a younger one may be
  another launch's copy in progress). A seed that is a symbolic link is
  followed. Both URLs follow the rule of `Database(url:)`: one that is not
  a local file URL is `.invalidParameter`. The copy is made owner-writable (`0600` for files, `0700` for
  directories) and, on iOS, given `protection`. Both store forms work: a
  `.rl` file and a store directory. File-system failures are thrown as
  Foundation reports them.
- **`Migrator.migrate`** runs `schema` (each statement on its own), then
  every registered step whose version is above the store's, in ascending
  order, and returns the resulting version. A step and the record of its
  version commit in one transaction: a failing step leaves neither its work
  nor its version, and later steps do not run. Versions are positive and
  registered in ascending order, each once; a migrator whose steps are not
  is refused with `.misuse`. A store already above `latestVersion` was
  written by a newer build and is refused the same way, as is a database
  opened read-only (through either `migrate`). The refusals come before
  anything runs, `schema` included. Run one migration at a time:
  `migrate(_ database:)` does, on the database's single writer.
- **The version** is the `version` property of one node,
  `(:RelateMigration {key: 'schema_version'})`, in the namespace of the
  connection the migration runs on (the default namespace for
  `migrate(_ database:)`). `versionLabel` changes the label, for an app
  whose own labels would collide. Several version nodes, or one without an
  integer version, are `.unexpectedResult`, never read as version zero.

## Records

```swift
public struct Record: Sendable, Hashable, RandomAccessCollection {   // of CypherValue, by column position
    public let values: [CypherValue]
    public var columns: [String] { get }
    public subscript(position: Int) -> CypherValue { get }
    public subscript(column: String) -> CypherValue? { get }      // nil: no such column (a null cell is .null)
    public var dictionary: [String: CypherValue] { get }
    public func get<T: CypherValueConvertible>(_ column: String, as: T.Type = T.self) throws(RelateError) -> T
    public func get<T: CypherValueConvertible>(_ position: Int, as: T.Type = T.self) throws(RelateError) -> T
}

extension QueryResult: RandomAccessCollection {                   // of Record: for-in, first, count, map, result[i]
    public func single() throws(RelateError) -> Record            // exactly one record
    public func scalar<T: CypherValueConvertible>(as: T.Type) throws(RelateError) -> T
}

public protocol CypherValueConvertible {
    func cypherValue() throws(RelateError) -> CypherValue
    init(cypherValue: CypherValue) throws(RelateError)
}
```

A `Record` is a view of one row, with the name lookup built once per result
(a duplicated column name resolves to its last column). `rows` is unchanged.
`records` returns the same `Record`s that iterating the result yields; it
returned `[[String: CypherValue]]` before, and `record.dictionary` is that
form. Code that reads `record["name"]?.stringValue` compiles unchanged.

Typed reads are strict. The conversions are:

| Swift type | Reads from | Binds as |
| --- | --- | --- |
| `Bool`, `String`, `Substring` | `.bool`, `.string` | the same |
| `Int`, `Int8`…`Int64`, `UInt`, `UInt8`…`UInt64` | `.int`, refusing a value the type cannot hold | `.int`; a value above `Int64.max` is `.invalidParameter` |
| `Double`, `Float` | `.float`, or `.int` widened (`Float` narrows a `.float`) | `.float` |
| `Optional<T>` | `.null` as `nil`, else `T` | `nil` as `.null` |
| `[T]` | `.list`, or a `.vector` as the list of its coordinates | `.list` (never a vector: use `CypherValue(vector:)`) |
| `[String: T]` | `.map` (not a node or relationship) | `.map` |
| `Foundation.Date` | `.dateTime` (a `.localDateTime` has no instant) | a UTC `.dateTime` |
| `URL`, `UUID` | `.string` that parses as one | `.string` (a `UUID` lowercase, as `randomUUID()` spells one) |
| `Data` | a `.list` of integers `0...255` | the same |
| `CLLocationCoordinate2D` | a geographic `.point` | a 2D WGS 84 `.point` (an invalid coordinate is refused) |
| `CypherValue` and its payload structs (`Point`, `Date`, `DateTime`, `Vector`, `Node`, …) | their own case | their own case |
| a `RawRepresentable` type that declares the conformance | its raw value, refusing one outside the type | its raw value |

Nothing else converts: a string is never parsed into a number, a float never
truncates to an integer, a `null` is never a default. A failure throws
`.unexpectedResult` with the column first in `path`, then the map key or
list position inside it (`["mixed", "[1]"]`). `single()` throws it for zero
or several rows; `scalar()` and `scalar(as:)` throw it for anything but one
row and one column.

## Statements

```swift
public struct Statement: Sendable, Hashable, ExpressibleByStringInterpolation, CustomStringConvertible {
    public init(_ cypher: String, _ parameters: [String: CypherValue] = [:])          // the text names its parameters ($key)
    public init(_ cypher: String, _ parameters: [String: any CypherValueConvertible]) throws(RelateError)
    public var cypher: String { get }                       // interpolated values rendered as $_p0, $_p1, …
    public var parameters: [String: CypherValue] { get }    // named and generated
    public mutating func append(_ other: Statement) throws(RelateError)
    public static func quoted(identifier: String) -> String // `name`, a backtick inside doubled
}

extension Connection {
    @discardableResult public func run(_ statement: Statement) throws(RelateError) -> QueryResult
}
```

A `Statement` written as a string literal **binds** what it interpolates:

| Interpolation | Effect |
| --- | --- |
| `\(value)` — any `CypherValueConvertible`, optional or not | a parameter; the text gets `$_pN`. Can throw (a value Cypher cannot hold), so the literal is written under `try` |
| `\(statement)` — another `Statement` | its text in place, its parameters with it |
| `\(identifier: name)` | a label, relationship type or property key, backtick-quoted |
| `\(raw: text)` | text spliced as written — for text the program wrote, never for input |

Generated names are assigned on the whole statement in order of appearance,
so one literal always renders one text whatever its values. A name the
statement's own parameters already use (`_p0` passed explicitly) is skipped.
Appending or embedding a statement that binds a name this one already binds
to a different value throws `.invalidParameter`.

`query(_:_:)` takes a `String` and `run(_:)` takes a `Statement`; they are
separate names so that an interpolated literal cannot resolve to the wrong
one. The same literal passed to `query` is an ordinary Swift string, with its
values in the text.

## Codable

```swift
public enum CypherKeyStrategy: Sendable, Hashable { case useDefaultKeys, snakeCase }

public struct CypherValueEncoder: Sendable {
    public var keyStrategy: CypherKeyStrategy
    public init(keyStrategy: CypherKeyStrategy = .useDefaultKeys)
    public func encode<T: Encodable>(_ value: T) throws(RelateError) -> CypherValue
}
CypherValue.init<T: Encodable>(encoding: T, keyStrategy: CypherKeyStrategy = .useDefaultKeys) throws(RelateError)

public struct CypherValueDecoder: Sendable {
    public var keyStrategy: CypherKeyStrategy
    public init(keyStrategy: CypherKeyStrategy = .useDefaultKeys)
    public func decode<T: Decodable>(_ type: T.Type, from value: CypherValue) throws(RelateError) -> T
    public func decode<T: Decodable>(_ type: T.Type, from record: Record) throws(RelateError) -> T
}
Record.decode<T: Decodable>(_: T.Type = T.self, using: CypherValueDecoder = .init()) throws(RelateError) -> T
Record.decode<T: Decodable>(_: T.Type = T.self, column: String, using: CypherValueDecoder = .init()) throws(RelateError) -> T
QueryResult.decode<T: Decodable>(_: T.Type = T.self, using: CypherValueDecoder = .init()) throws(RelateError) -> [T]
QueryResult.decode<T: Decodable>(_: T.Type = T.self, column: String, using: CypherValueDecoder = .init()) throws(RelateError) -> [T]
```

- **Encoding.** A struct is a `.map`, an array a `.list`. A value that is
  `CypherValueConvertible` encodes through that conversion (a `Date` is a
  `DATETIME`, a `URL` a string) rather than through its own `Encodable`. A
  `nil` optional property is left out of the map, as synthesized `Encodable`
  does — which is what `SET n += $props` needs, since a key held as `null`
  there removes the property. A failure is `.invalidParameter`.
- **Decoding.** A record decodes by column name; a `.map`, `.node` or
  `.relationship` by its keys; `decode(_:column:)` decodes the value in one
  column (`MATCH (b:Book) RETURN b`). A type that decodes from a single
  value reads the one column of a one-column record. `CypherValueConvertible`
  types decode through that conversion, with the strict rules under "Records";
  a `.vector` decodes as an array of its coordinates.
- **Keys.** `.snakeCase` writes `readingStatus` as `reading_status`. When
  decoding, a property is looked up under its own name first and then under
  its snake-case spelling. Converting the Swift name (not the stored key) is
  what lets `coverURL` find `cover_url`.
- **Failures** throw `.unexpectedResult` with the full path:
  `["shelf", "books", "[1]", "n"]` is column `shelf`, key `books`, second
  element, key `n`. A `DecodingError` thrown by a type's own `init(from:)`
  is reported the same way at its coding path, and any other error it throws
  at the path of the value that type was decoding from. The error's own
  text, which can quote the value it refused, is the mismatch's `reason`;
  `found` is a fixed phrase.

## Bridges

Conversions between `CypherValue`'s typed structs and the platform's own
types. Inside an `extension CypherValue`, the bare names `Date` and
`Duration` are the nested Cypher types; write `Foundation.Date` and
`Swift.Duration` there.

```swift
// Foundation.Date — to the microsecond
CypherValue.init(_ date: Foundation.Date) throws(RelateError)                    // a UTC .dateTime
CypherValue.DateTime.init(_ date: Foundation.Date, offsetSeconds: Int64 = 0) throws(RelateError)
CypherValue.DateTime.init(_ date: Foundation.Date, in: TimeZone) throws(RelateError) // the zone's offset at that instant; no zone id
CypherValue.DateTime.foundationDate: Foundation.Date?                            // wall clock less offset; nil when the stored components name no instant
CypherValue.LocalDateTime.init(_ date: Foundation.Date, in: TimeZone) throws(RelateError)
CypherValue.LocalDateTime.foundationDate(in: TimeZone) -> Foundation.Date?       // nil in a daylight-saving gap; the earlier instant in an overlap
CypherValue.Date.init(_ date: Foundation.Date, in: TimeZone) throws(RelateError) // the calendar day there
CypherValue.Date.init(_ components: DateComponents) throws(RelateError)          // needs year, month and day; refuses a non-Gregorian calendar's components; era 0 is BCE (1 BCE = year 0)
CypherValue.Date.supportedYears: ClosedRange<Int64>                              // -999,999,999 ... 999,999,999
CypherValue.Date.dateComponents: DateComponents                                  // carries the Gregorian calendar
CypherValue.Date.startOfDay(in: TimeZone) -> Foundation.Date?

// Swift.Duration
CypherValue.Duration.init(_ duration: Swift.Duration) throws(RelateError)        // months and days zero; sub-nanosecond truncated
CypherValue.Duration.swiftDuration: Swift.Duration?                              // nil when months or days are non-zero
CypherValue.Duration.dateComponents: DateComponents                              // month, day, second, nanosecond

// Vectors
CypherValue.init(vector: [Float]) throws(RelateError)                            // FLOAT32
CypherValue.init(vector: [Double]) throws(RelateError)                           // FLOAT
CypherValue.Vector.float32(_: [Float]), .float64(_: [Double]) throws(RelateError)
CypherValue.Vector.maximumCount: Int                                             // 4,096
CypherValue.Vector.dtype: DType, .count: Int
CypherValue.Vector.float32Values: [Float]?                                       // nil for an integer dtype; a FLOAT vector is narrowed (nil if a coordinate exceeds Float's range)
CypherValue.Vector.doubleValues: [Double]?                                       // nil for an integer dtype
CypherValue.Vector.init(_: MLShapedArray<Float>) throws(RelateError)             // Core ML: scalars in row-major order, any shape
CypherValue.Vector.init(_: MLMultiArray) throws(RelateError)                     // float16/float32 → FLOAT32, double → FLOAT, int32 → INTEGER32

// Points
CypherValue.Point.init(latitude: Double, longitude: Double, height: Double? = nil) throws(RelateError) // srid 4326, or 4979 with a height
CypherValue.Point.init(x: Double, y: Double, z: Double? = nil)                     // srid 7203, or 9157 with z
CypherValue.Point.crs: CRS?                                                        // .cartesian | .cartesian3D | .wgs84 | .wgs84_3D; nil for another srid
CypherValue.Point.latitude, .longitude: Double?                                    // nil unless geographic
CypherValue.Point.height: Double?                                                  // nil unless 3D geographic
CypherValue.Point.init(_: CLLocationCoordinate2D) throws(RelateError)              // Core Location
CypherValue.Point.coordinate: CLLocationCoordinate2D?                              // nil unless geographic
CypherValue.init(_: CLLocationCoordinate2D) throws(RelateError)
```

- **Precision.** `Foundation.Date` is a `Double` of seconds, resolving about
  a quarter of a microsecond near the present, so the date bridges carry
  microseconds. A value at millisecond or microsecond precision round-trips
  unchanged for instants within about two centuries of 2001.
- **Checked input.** A bridge that builds a value refuses input it cannot
  represent, with `.invalidParameter`, and no input makes its arithmetic
  overflow:
  - a non-finite `Foundation.Date`, or one whose wall clock (at the offset
    asked for) falls in a year outside `CypherValue.Date.supportedYears`;
  - a UTC offset beyond 18 hours either side, given directly or reported
    by a `TimeZone`;
  - a `Swift.Duration` beyond what `Int64` seconds hold;
  - date components that are not a day of the Gregorian calendar (month
    outside 1...12, a day the month does not have, a year outside the
    supported years, a missing field);
  - a latitude outside -90...90, a longitude outside -180...180, or a
    non-finite height. The engine's `point()` function wraps a longitude
    instead; the initializer does not;
  - a vector with a NaN or infinite coordinate, or with more than
    `CypherValue.Vector.maximumCount` coordinates (4,096, the engine's bound
    on a `vector()` dimension and on a vector index). Nothing is truncated.

  The memberwise initializers and the enum cases (`Point(srid:x:y:z:)`,
  `Date(year:month:day:)`, `.floats(dtype:values:)`) store what they are
  given. The bridges that read such a value back (`foundationDate`,
  `foundationDate(in:)`, `startOfDay(in:)`) are `nil` when its components
  name no instant, and never trap on them. An empty vector is accepted.
- **Calendar.** The bridges use proleptic-Gregorian arithmetic at every
  year, as Cypher's temporal values do. Foundation's Gregorian `Calendar`
  is Julian before October 1582 and is not used.
- **Zones.** `DateTime.init(_:in:)` records the zone's offset, not its id.
  The engine resolves a named zone from the wall clock, which in a
  daylight-saving overlap can name the other of the two instants; an offset
  round-trips the instant exactly.
- **Vectors.** A `Vector` stores `Double` or `Int64` coordinates whatever
  its dtype; widening a `Float` is exact, so a `FLOAT32` vector reads back
  bit-identical through `float32Values`.
- The Core ML and Core Location members compile where those frameworks can
  be imported (`#if canImport`).

## Parameters

`query(_:_:)` takes `[String: CypherValue]` or
`[String: any CypherValueConvertible]`. The second converts each Swift value
first (the table under "Records"), so variables bind without wrapping:
`["title": title, "year": year, "rating": rating as Double?]`. A dictionary
of literals, `nil` or enum cases resolves to the first, as before. Either
way the call binds `parameters` through the C ABI's typed params builder
(ADR-0502 §4) — one putter per value, no JSON, so temporals, points,
vectors, durations, non-finite floats and full-range integers all bind
directly (`["d": .date(...)]`, `["v": .vector(.ints(dtype: .integer8,
values: [1, 2, 3]))]`, `["f": .float(.nan)]`); an empty dictionary binds
none. `.node`, `.relationship` and `.path` values are refused with
`RelateError.invalidParameter` (pass their element id or properties), as
are a query, path, namespace, parameter name or map key containing a NUL
byte (C strings would truncate it). A putter refusal the engine's builder
makes (an impossible calendar date, an unknown zone, a srid/dimension
mismatch) surfaces as the typed `.engine` error with stage `internal`.

## Errors

Every failure the C ABI reports crosses as one typed shape
(``EngineError``): `code`, `gqlStatus`, `stage`, `retryable`, `message` —
classified by the engine's own single table, so the client never sniffs
message text. Branch on `code` / `stage`; retry exactly when `retryable`
(today: the write-write conflict and the retryable workspace cap). A
statement failure rides the result (`RETURN 1/0` throws
`.engine(EngineError(code: "Neo.ClientError.Statement.ArithmeticError",
gqlStatus: "22G03", stage: .runtime, retryable: false, …))`); every other
call failure fills that call's own `error_json` out-param. The predicates
(`isConstraintViolation`, `isSyntaxError`, `isMissingParameter`,
`isWriteConflict`) and `classification` are read off `code`; the package's
tests provoke each from the engine, so they track the engine's table. `stage ==
.internal` means a boundary failure (a NULL or poisoned handle, a caught
panic, an argument that failed to decode): do not retry on it — release the
handle and open a new one.

## Lifecycle and concurrency

- A `Database` closes when its last reference is released; every
  `Connection` holds its database, so the store outlives its connections.
  A `Connection` disconnects when released.
- `Database` is `Sendable`: share one across tasks and actors. `Connection` is
  not: Swift 6 rejects sending it across isolation domains, which is the C
  ABI's "do not share a connection across threads" rule. Use
  `Database.read` / `Database.write`, which manage their own connections, or
  own each `Connection` in one actor and open more for concurrent work.
- Calls on a `Connection` are synchronous and block while the engine runs.
  Call them from a background actor, not the `MainActor`. `Database.read`
  and `Database.write` are `async` and safe to await from the `MainActor`.
- Errors travel with the call that failed (the v2 error channel — ADR-0502
  §2): a query run from any task reports its own error to whoever reads its
  throw. There is no thread-local slot, so nothing depends on the calling
  thread.
- A panic inside the engine poisons the handle (C ABI contract): later calls
  throw `.engine` with stage `internal`; release it and open a new one.
- `transaction(retries:_:)` commits when the body returns and rolls back,
  then rethrows the body's error, when it throws; a throwing `commit()`
  inside it likewise rolls back and rethrows the commit's error. The body
  receives a `Transaction`: it can run statements, and cannot begin, commit,
  roll back or change the namespace. A `Transaction` used after its body
  returned throws `.misuse`.
- `retries` is a count of additional attempts; below one, the body runs
  once. A failure the engine marks `retryable` — on a statement or on the
  commit — rolls back and runs the body again; any other failure, and any
  error the body itself throws, is rethrown at once. The body must be safe
  to run more than once: only what it did inside the transaction is undone.
  Without `retries`, a retry loop written by hand must check both the
  statement errors and `commit()`. `commit()` returns the transaction's
  bookmark, or `nil` when the transaction committed nothing.
- `read(_:)` runs the body in a transaction that is always rolled back.
  Every statement in it sees one snapshot, whatever other connections commit
  meanwhile, and nothing the body does is kept — schema statements included.
  A statement whose query type is not `read` is refused with `.misuse` after
  it ran in the transaction's own overlay.
- `runInTransaction(_:retries:)` runs a fixed list of statements in one
  transaction, all or none, and is `throws(RelateError)`.
- `isPoisoned()` answers whether an engine panic poisoned the connection.
  The error channel has no typed poison signal, so it probes: it runs
  `RETURN 1`, and reports `true` exactly when that fails at stage
  `internal`. Call it after an error whose stage is `internal`; not every
  such error is poison (the params builder refusing an impossible date is
  one). On `true`, release the connection and `connect()` again; do not
  retry the failed work.

## Building and linking

The binary target `RelateFFI` is a static XCFramework
(`ios-arm64`, `ios-arm64_x86_64-simulator`, `macos-arm64_x86_64`) carrying
the C header and a `CRelate` module map. Its module map links `iconv` and
`CoreFoundation`. The `Relate` target ships `PrivacyInfo.xcprivacy`
(no collection, no tracking; file-timestamp API reason `C617.1`).

| Binary source | When |
| --- | --- |
| `binaryTarget(url:checksum:)` — the release asset `RelateFFI.xcframework.zip`, tag and checksum stamped by the release workflow | a consumer resolving a release tag |
| `binaryTarget(path: "target/apple/RelateFFI.xcframework")` | `RELATE_LOCAL_XCFRAMEWORK=1`, or no release stamped yet |

### `scripts/build-xcframework.sh`

```sh
scripts/build-xcframework.sh                 # all slices
scripts/build-xcframework.sh --slices macos  # macOS only (swift test)
scripts/build-xcframework.sh --zip           # also target/apple/RelateFFI.xcframework.zip
```

| Flag / env | Default | Meaning |
| --- | --- | --- |
| `--slices all\|macos` | `all` | `all`: iOS device, iOS simulator (arm64 + x86_64), macOS (arm64 + x86_64); `macos`: the macOS slice only |
| `--zip` | off | Write the release zip next to the XCFramework (`ditto -c -k --keepParent`) |
| `-h`, `--help` | — | Print usage |
| `IPHONEOS_DEPLOYMENT_TARGET` | `16.0` | iOS floor the static libraries are built for; matches `Package.swift` |
| `MACOSX_DEPLOYMENT_TARGET` | `13.0` | macOS floor; matches `Package.swift` |
| `XCFRAMEWORK_CARGO_PROFILE` | `release` | Cargo profile name for `relate-ffi` (not the Makefile's `CARGO_PROFILE` flag) |

Requires the Rust targets `aarch64-apple-darwin x86_64-apple-darwin
aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios` (macOS: the first
two); the script names the missing ones.

### `scripts/stamp-swift-package.sh`

```sh
scripts/stamp-swift-package.sh v0.14.0 target/apple/RelateFFI.xcframework.zip
```

| Positional | Meaning |
| --- | --- |
| `TAG` | Release tag (`vMAJOR.MINOR.PATCH[-pre]`) written to `releaseTag` |
| `ZIP` | Release zip whose `swift package compute-checksum` is written to `releaseChecksum` |

Fails if either constant is not declared exactly once in `Package.swift`.
Release-workflow use only.

### Make targets

| Target | Runs |
| --- | --- |
| `make build-xcframework` | `scripts/build-xcframework.sh` |
| `make test-swift` | the XCFramework build, `swift test`, the examples, the release-stamp harness (`scripts/tests/test_stamp_swift_package.sh`), the iOS example's core tests and its iOS Simulator build — the CI `swift` job |
| `make swift-examples` | `swift run` of each `docs/api/swift/examples` executable |

> **Note:** a fresh `swift test` compile fails with
> `plugin for module 'TestingMacros' not found` on hosts running only
> CommandLineTools (no full Xcode) — the Swift Testing macro plugin is not
> auto-loaded there. Once the suite has compiled once (e.g. via
> `make test-swift` on a full-Xcode host), `swift test --skip-build` runs it
> against the already-compiled bundle.

## Examples

[`examples/`](examples/) is a SwiftPM package with one executable per
directory under `Sources/`; each asserts its own output, and
`make swift-examples` runs every one. The iOS app [`examples/ios-snotel`](../../../examples/ios-snotel/)
is the end-to-end consumer, and
[`docs/guides/ios-apps.md`](../../guides/ios-apps.md) is the task guide.
